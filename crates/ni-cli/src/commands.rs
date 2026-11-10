//! Everything but `flash`: interfaces, preflight, assets, profiles.

use crate::cli::{
    exit, AfterArg, AssetsCommand, Cli, Command, ConfigArg, NetArgs, ProfilesCommand, RecipeArgs,
};
use crate::output::finding;
use ni_assets::{default_cache_root, AssetEvent, Assets, HttpConfig, PkgArch, VersionSpec};
use ni_engine::preflight::{self, PreflightReport};
use ni_engine::profile::spec_to_string;
use ni_engine::{
    default_profiles_root, license_key_from_file, AfterInstall, ConfigMode, EngineConfig, Failure,
    Profile, ProfileStore,
};
use ni_net::{list_interfaces, Interface};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An exit code, or an error (printed, exit code 1).
pub type Outcome = Result<u8, Failure>;

/// Settings shared by all commands.
pub struct Ctx {
    pub json: bool,
    pub cache_dir: PathBuf,
    pub profiles: ProfileStore,
    pub http: HttpConfig,
}

impl Ctx {
    pub fn new(cli: &Cli) -> Result<Ctx, Failure> {
        let no_home = || Failure::new("no_home");
        let cache_dir = match &cli.cache_dir {
            Some(d) => d.clone(),
            None => default_cache_root().ok_or_else(no_home)?,
        };
        let profiles = match &cli.profiles_dir {
            Some(d) => d.clone(),
            None => default_profiles_root().ok_or_else(no_home)?,
        };
        let mut http = HttpConfig::default();
        if let Some(b) = &cli.download_base {
            http.download_base = b.clone();
        }
        if let Some(b) = &cli.upgrade_base {
            http.upgrade_base = b.clone();
        }
        Ok(Ctx {
            json: cli.json,
            cache_dir,
            profiles: ProfileStore::new(profiles),
            http,
        })
    }

    /// An asset manager; in human mode downloads are reported on stderr.
    pub fn assets(&self) -> Result<Assets, Failure> {
        let assets = Assets::new(&self.cache_dir, self.http.clone())
            .map_err(|e| Failure::new("assets_failed").with("detail", e))?;
        if self.json {
            return Ok(assets);
        }
        Ok(assets.with_events(Arc::new(|e| match e {
            AssetEvent::DownloadStarted { file, total } => match total {
                Some(t) => eprintln!("downloading {file} ({t} bytes)"),
                None => eprintln!("downloading {file}"),
            },
            AssetEvent::DownloadFinished { file, .. } => eprintln!("downloaded {file}"),
            _ => {}
        })))
    }
}

pub async fn run(cli: Cli) -> Outcome {
    let ctx = Ctx::new(&cli)?;
    match cli.command {
        Command::Interfaces { all } => interfaces(&ctx, all),
        Command::Preflight { net, profile } => preflight(&ctx, &net, profile.as_deref()).await,
        Command::Assets {
            command:
                AssetsCommand::Fetch {
                    version,
                    archs,
                    packages,
                },
        } => fetch(&ctx, &version, &archs, &packages).await,
        Command::Profiles { command } => profiles(&ctx, command).await,
        Command::Flash(args) => crate::flash::flash(&ctx, args).await,
        Command::Version { check, update_base } => {
            version(&ctx, check, update_base.as_deref()).await
        }
    }
}

// --------------------------------------------------------------- version

/// `ni-cli <version>`; with `check`, the latest release on GitHub (or at
/// `base`, testing) too. Error: `update_check_failed` (`problem`).
async fn version(ctx: &Ctx, check: bool, base: Option<&str>) -> Outcome {
    let current = env!("CARGO_PKG_VERSION");
    if !check {
        if ctx.json {
            print_json(&json!({ "current": current }));
        } else {
            println!("ni-cli {current}");
        }
        return Ok(exit::OK);
    }
    let mut cfg = ni_update::UpdateConfig::default();
    if let Some(b) = base {
        let b = b.trim_end_matches('/');
        cfg.api_url = format!("{b}/repos/SepsiG/Mac-Netinstall/releases/latest");
        cfg.download_prefix = format!("{b}/SepsiG/Mac-Netinstall/releases/download/");
    }
    let failed = |e: ni_update::UpdateError| {
        Failure::new("update_check_failed")
            .with("problem", e.code())
            .with("detail", e)
    };
    let latest = ni_update::Updater::new(cfg)
        .map_err(failed)?
        .latest()
        .await
        .map_err(failed)?;
    let version = latest.as_ref().map(|r| r.version.clone());
    let newer = version
        .as_deref()
        .is_some_and(|v| ni_update::is_newer(v, current));
    let page = latest.as_ref().map_or_else(
        || format!("{}latest", ni_update::RELEASES_PAGE),
        |r| r.page_url.clone(),
    );
    if ctx.json {
        print_json(&json!({ "current": current, "latest": version, "newer": newer, "page": page }));
    } else if newer {
        println!(
            "ni-cli {current}; Mac Netinstall {} is available: {page}",
            version.unwrap_or_default()
        );
    } else {
        println!("ni-cli {current} is up to date");
    }
    Ok(exit::OK)
}

/// Engine settings from the network arguments (including the hidden
/// testing overrides).
pub fn engine_config(net: &NetArgs) -> EngineConfig {
    let mut cfg = EngineConfig::new(&net.iface);
    cfg.server_ip = net.server_ip;
    cfg.allow_non_ethernet = net.allow_any_interface;
    cfg.netmask = net.netmask;
    cfg.pool = net.pool;
    let p = &mut cfg.ports;
    if let Some(ip) = net.bind_ip {
        p.bind_ip = ip;
    }
    if let Some(port) = net.bootp_port {
        p.bootp = port;
    }
    if let Some(port) = net.tftp_port {
        p.tftp = port;
    }
    if let Some(port) = net.netinstall_port {
        p.netinstall = port;
    }
    if let Some(a) = net.bootp_reply_to {
        p.bootp_reply_to = a;
    }
    if let Some(a) = net.netinstall_dest {
        p.netinstall_dest = a;
    }
    cfg
}

fn print_json(v: &impl serde::Serialize) {
    println!("{}", serde_json::to_string(v).expect("serializable"));
}

// ------------------------------------------------------------ interfaces

fn interfaces(ctx: &Ctx, all: bool) -> Outcome {
    let list =
        list_interfaces().map_err(|e| Failure::new("interface_list_failed").with("detail", e))?;
    let shown: Vec<&Interface> = list
        .iter()
        .filter(|i| all || i.is_netinstall_capable())
        .collect();
    let addrs = |i: &Interface| -> Vec<String> {
        i.ipv4
            .iter()
            .map(|(ip, mask)| format!("{ip}/{}", u32::from(*mask).leading_ones()))
            .collect()
    };
    if ctx.json {
        let v: Vec<_> = shown
            .iter()
            .map(|i| {
                json!({
                    "name": i.name,
                    "kind": i.kind.as_str(),
                    "display_name": i.display_name,
                    "mac": i.mac.map(|m| m.to_string()),
                    "ipv4": addrs(i),
                    "up": i.is_up,
                    "running": i.is_running,
                    "netinstall_capable": i.is_netinstall_capable(),
                })
            })
            .collect();
        print_json(&v);
        return Ok(exit::OK);
    }
    for i in shown {
        let state = match (i.is_up, i.is_running) {
            (true, true) => "up",
            (true, false) => "no link",
            _ => "down",
        };
        let ips = addrs(i).join(", ");
        let name = i.display_name.as_deref().unwrap_or("");
        println!(
            "{:<10} {:<14} {:<8} {:<20} {name}",
            i.name,
            i.kind.as_str(),
            state,
            if ips.is_empty() { "-" } else { &ips }
        );
    }
    Ok(exit::OK)
}

// ------------------------------------------------------------ preflight

/// Print a report; the exit code is 4 if it has errors.
pub fn print_report(ctx: &Ctx, report: &PreflightReport) -> u8 {
    for f in &report.findings {
        if ctx.json {
            let mut v = serde_json::to_value(f).expect("serializable");
            v["event"] = json!("preflight");
            print_json(&v);
        } else {
            println!("{}", finding(f));
        }
    }
    if report.ok() {
        exit::OK
    } else {
        exit::PREFLIGHT
    }
}

async fn preflight(ctx: &Ctx, net: &NetArgs, profile: Option<&str>) -> Outcome {
    let cfg = engine_config(net);
    let profile = profile.map(|name| ctx.profiles.load(name)).transpose()?;
    let assets = match profile {
        Some(_) => Some(ctx.assets()?),
        None => None,
    };
    let report = preflight::run(&cfg, assets.as_ref(), profile.as_ref()).await;
    let code = print_report(ctx, &report);
    if !ctx.json {
        if let Some(n) = &report.network {
            println!(
                "server {} / {}, pool {} - {}",
                n.server_ip, n.netmask, n.pool_start, n.pool_end
            );
        }
        println!(
            "{}",
            if code == exit::OK {
                "preflight passed"
            } else {
                "preflight FAILED"
            }
        );
    }
    Ok(code)
}

// ------------------------------------------------------------ assets

fn asset_err(e: ni_assets::AssetError) -> Failure {
    Failure::new("assets_failed")
        .with("cause", e.code())
        .with("detail", e)
}

async fn fetch(ctx: &Ctx, spec: &VersionSpec, archs: &[String], extras: &[String]) -> Outcome {
    let assets = ctx.assets()?;
    let version = assets.resolve(spec).await.map_err(asset_err)?;
    let kernels = assets.ensure_kernels(&version).await.map_err(asset_err)?;
    let mut kernel_archs: Vec<&str> = kernels.keys().map(|a| a.as_str()).collect();
    kernel_archs.sort();
    let mut packages = Vec::new();
    for arch in archs {
        let pkg_arch = PkgArch::from_device_arch(arch).map_err(|e| asset_err(e.into()))?;
        let files = assets
            .ensure_packages(&version, pkg_arch, extras)
            .await
            .map_err(asset_err)?;
        packages.push((pkg_arch, files));
    }
    if ctx.json {
        let pkgs: Vec<_> = packages
            .iter()
            .map(|(arch, files)| {
                let files: Vec<_> = files
                    .iter()
                    .map(|f| json!({"name": f.name, "size": f.data.len()}))
                    .collect();
                json!({"arch": arch.as_str(), "files": files})
            })
            .collect();
        print_json(
            &json!({"version": version.as_str(), "kernels": kernel_archs, "packages": pkgs}),
        );
    } else {
        println!("{version}: boot kernels for {}", kernel_archs.join(", "));
        for (arch, files) in &packages {
            for f in files {
                println!("{arch}: {} ({} bytes)", f.name, f.data.len());
            }
        }
    }
    Ok(exit::OK)
}

// ------------------------------------------------------------ profiles

/// A profile from command-line arguments. Everything is checked first
/// (options, key file, script files, the profile itself); only then are the
/// scripts registered in the cache (as their lowercase file names), so a
/// refused profile leaves the cache alone. Errors besides the profile
/// validation codes: `script_with_config` (`config`: `--script` with
/// `--config keep` or `empty`, which the official tool refuses too),
/// `mode_script_with_keep`, `script_failed`, `empty_script` (`name`: use
/// `--config empty` instead), `license_key_failed` (`path`, `detail`:
/// unreadable key file), `invalid_license_key`.
pub async fn recipe_profile(
    assets: &Assets,
    name: &str,
    recipe: &RecipeArgs,
) -> Result<Profile, Failure> {
    let mut p = Profile::new(name, recipe.version.clone());
    p.extra_packages = recipe.packages.clone();
    let c = &recipe.config;
    let script = match (c.config, &c.script) {
        (None | Some(ConfigArg::Default), None) => {
            p.config = ConfigMode::Default;
            None
        }
        (Some(ConfigArg::Keep), None) => {
            p.config = ConfigMode::Keep;
            None
        }
        (Some(ConfigArg::Empty), None) => {
            p.config = ConfigMode::Empty;
            None
        }
        (None | Some(ConfigArg::Default), Some(script)) => Some(script),
        (Some(other), Some(_)) => {
            let config = format!("{other:?}").to_lowercase();
            return Err(Failure::new("script_with_config").with("config", config));
        }
    };
    if c.mode_script.is_some() && p.config == ConfigMode::Keep {
        return Err(Failure::new("mode_script_with_keep"));
    }
    p.remove_branding = c.remove_branding;
    if let Some(path) = &c.license_key {
        let data = std::fs::read(path).map_err(|e| {
            Failure::new("license_key_failed")
                .with("path", path.display())
                .with("detail", e)
        })?;
        p.license_key = Some(license_key_from_file(&data)?);
    }
    p.after_install = match c.after {
        None | Some(AfterArg::Reboot) => AfterInstall::Reboot,
        Some(AfterArg::RebootMessage) => AfterInstall::RebootMessage,
        Some(AfterArg::Shutdown) => AfterInstall::Shutdown,
    };
    if let Some(path) = script {
        let checked = check_script(assets, path).await?;
        if checked.size == 0 {
            return Err(ni_engine::prepare::empty_script(&checked.name));
        }
        p.config = ConfigMode::Script {
            local_name: checked.name,
        };
    }
    if let Some(path) = &c.mode_script {
        p.mode_script = Some(check_script(assets, path).await?.name);
    }
    p.validate()?;
    // Everything checked: register.
    for path in script.into_iter().chain(&c.mode_script) {
        register_script(assets, path).await?;
    }
    Ok(p)
}

fn script_failed(path: &Path, e: &ni_assets::AssetError) -> Failure {
    Failure::new("script_failed")
        .with("path", path.display())
        .with("cause", e.code())
        .with("detail", e)
}

async fn check_script(assets: &Assets, path: &Path) -> Result<ni_assets::LocalFile, Failure> {
    assets
        .check_local_file(path)
        .await
        .map_err(|e| script_failed(path, &e))
}

async fn register_script(assets: &Assets, path: &Path) -> Result<String, Failure> {
    let local = assets
        .register_local_file(path)
        .await
        .map_err(|e| script_failed(path, &e))?;
    Ok(local.name)
}

/// How a license key is shown: never the key itself, only its last 4
/// characters and its length (`****abcd (44 chars)`).
pub fn masked_key(key: &str) -> String {
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    format!("****{tail} ({} chars)", key.chars().count())
}

/// A profile as JSON for output, its license key masked ([`masked_key`]).
fn shown(p: &Profile) -> serde_json::Value {
    let mut v = serde_json::to_value(p).expect("profiles serialize");
    if let Some(key) = &p.license_key {
        v["license_key"] = serde_json::Value::String(masked_key(key));
    }
    v
}

/// `default config`, `script lab.rsc, mode script m.rsc, shutdown`, ...
fn config_text(p: &Profile) -> String {
    let mut out = match &p.config {
        ConfigMode::Keep => "keep config".to_string(),
        ConfigMode::Default => "default config".to_string(),
        ConfigMode::Empty => "empty config".to_string(),
        ConfigMode::Script { local_name } => format!("script {local_name}"),
    };
    if let Some(m) = &p.mode_script {
        out.push_str(&format!(", mode script {m}"));
    }
    if p.remove_branding {
        out.push_str(", remove branding");
    }
    if let Some(key) = &p.license_key {
        out.push_str(&format!(", license key {}", masked_key(key)));
    }
    match p.after_install {
        AfterInstall::Reboot => {}
        AfterInstall::RebootMessage => out.push_str(", reboot with message"),
        AfterInstall::Shutdown => out.push_str(", shutdown"),
    }
    out
}

async fn profiles(ctx: &Ctx, command: ProfilesCommand) -> Outcome {
    match command {
        ProfilesCommand::List => {
            let listing = ctx.profiles.list();
            for (path, f) in &listing.broken {
                eprintln!("warning: {}: {f}", path.display());
            }
            if ctx.json {
                let shown: Vec<_> = listing.profiles.iter().map(shown).collect();
                print_json(&shown);
            } else if listing.profiles.is_empty() {
                println!("no profiles in {}", ctx.profiles.root().display());
            } else {
                for p in &listing.profiles {
                    let mut packages = vec!["routeros".to_string()];
                    packages.extend(p.extra_packages.iter().cloned());
                    println!(
                        "{:<24} {:<10} {:<30} {}",
                        p.name,
                        spec_to_string(&p.version),
                        packages.join(","),
                        config_text(p)
                    );
                }
            }
        }
        ProfilesCommand::Show { name } => {
            let p = ctx.profiles.load(&name)?;
            if ctx.json {
                print_json(&shown(&p));
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&shown(&p)).expect("serializable")
                );
            }
        }
        ProfilesCommand::Save { name, recipe } => {
            let assets = ctx.assets()?;
            let p = recipe_profile(&assets, &name, &recipe).await?;
            ctx.profiles.save(&p)?;
            if ctx.json {
                print_json(&shown(&p));
            } else {
                println!("saved profile {}", p.name);
            }
        }
        ProfilesCommand::Delete { name } => {
            ctx.profiles.delete(&name)?;
            if !ctx.json {
                println!("deleted profile {name}");
            }
        }
    }
    Ok(exit::OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn testing_flags_reach_the_engine_config() {
        let cli = Cli::try_parse_from([
            "ni-cli",
            "preflight",
            "--iface",
            "lo0",
            "--bootp-port",
            "6700",
            "--tftp-port",
            "6900",
            "--netinstall-port",
            "0",
            "--bind-ip",
            "127.0.0.1",
            "--bootp-reply-to",
            "127.0.0.1:6800",
            "--netinstall-dest",
            "127.0.0.1:6801",
            "--server-ip",
            "127.0.0.1",
        ])
        .unwrap();
        let Command::Preflight { net, .. } = cli.command else {
            panic!()
        };
        let cfg = engine_config(&net);
        assert_eq!(cfg.interface, "lo0");
        assert_eq!(cfg.server_ip, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(
            (cfg.ports.bootp, cfg.ports.tftp, cfg.ports.netinstall),
            (6700, 6900, 0)
        );
        assert_eq!(cfg.ports.bootp_reply_to.to_string(), "127.0.0.1:6800");
        assert_eq!(cfg.ports.netinstall_dest.to_string(), "127.0.0.1:6801");
        assert_eq!(cfg.ports.bind_ip.to_string(), "127.0.0.1");
        let defaults = engine_config(&NetArgs {
            iface: "en7".into(),
            allow_any_interface: false,
            server_ip: None,
            netmask: None,
            pool: None,
            bind_ip: None,
            bootp_port: None,
            tftp_port: None,
            netinstall_port: None,
            bootp_reply_to: None,
            netinstall_dest: None,
        });
        assert_eq!(defaults.ports, ni_engine::Ports::default());
    }

    #[test]
    fn ctx_uses_overrides() {
        let cli = Cli::try_parse_from([
            "ni-cli",
            "--cache-dir",
            "/tmp/c",
            "--profiles-dir",
            "/tmp/p",
            "--download-base",
            "http://x/dl/",
            "--upgrade-base",
            "http://x/up",
            "interfaces",
        ])
        .unwrap();
        let ctx = Ctx::new(&cli).unwrap();
        assert_eq!(ctx.cache_dir, PathBuf::from("/tmp/c"));
        assert_eq!(ctx.profiles.root(), Path::new("/tmp/p"));
        assert_eq!(ctx.http.download_base, "http://x/dl/");
        assert_eq!(ctx.http.upgrade_base, "http://x/up");
        assert!(!ctx.json);
    }
}
