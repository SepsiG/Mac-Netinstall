//! Everything the commands do, without Tauri: an [`AppCore`] owns the
//! engine, the profile store and the settings. The `#[tauri::command]`
//! functions in `commands.rs` only unwrap their arguments and call it, so
//! this is tested directly (see `tests/bridge.rs`).

use crate::settings::{Settings, SettingsStore};
use crate::views::{
    without_paths, BrokenProfile, CacheView, InterfaceView, PackagesView, ProfilesView, ScriptView,
    StartRequest,
};
use ni_assets::{
    default_cache_root, item_version, AssetError, DeleteScope, HttpConfig, LocalKind, PkgArch,
    Version, VersionSpec,
};
use ni_engine::{
    default_profiles_root, preflight, EngineConfig, EngineHandle, Failure, PreflightReport,
    Profile, ProfileStore, Snapshot, StartInfo,
};
use ni_proto::Mac;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::watch;

/// Where the app keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Downloads (`~/Library/Caches/<bundle id>`).
    pub cache: PathBuf,
    /// Profiles (`~/Library/Application Support/<bundle id>/profiles`).
    pub profiles: PathBuf,
    /// `~/Library/Application Support/<bundle id>/settings.json`.
    pub settings: PathBuf,
}

impl Paths {
    /// The standard locations; `no_home` without `HOME`.
    pub fn for_user() -> Result<Paths, Failure> {
        let (Some(cache), Some(profiles)) = (default_cache_root(), default_profiles_root()) else {
            return Err(Failure::new("no_home"));
        };
        let settings = profiles.parent().map_or_else(
            || PathBuf::from("settings.json"),
            |p| p.join("settings.json"),
        );
        Ok(Paths {
            cache,
            profiles,
            settings,
        })
    }

    /// Everything under `root` (tests).
    pub fn under(root: &Path) -> Paths {
        Paths {
            cache: root.join("cache"),
            profiles: root.join("profiles"),
            settings: root.join("settings.json"),
        }
    }
}

/// Changes every [`EngineConfig`] the app builds (tests use it for
/// loopback ports and fast timeouts).
pub type ConfigHook = Arc<dyn Fn(&mut EngineConfig) + Send + Sync>;

/// `invalid_mac` (`mac`) unless `s` is a MAC address.
pub fn parse_mac(s: &str) -> Result<Mac, Failure> {
    s.parse()
        .map_err(|_| Failure::new("invalid_mac").with("mac", s))
}

/// The interfaces, for the picker. Error: `interface_list_failed`.
pub fn list_interfaces() -> Result<Vec<InterfaceView>, Failure> {
    let ifaces = ni_net::list_interfaces()
        .map_err(|e| Failure::new("interface_list_failed").with("detail", e))?;
    let routed = ni_net::default_route_interfaces().unwrap_or_default();
    Ok(ifaces
        .iter()
        .map(|i| InterfaceView::new(i, &routed))
        .collect())
}

/// `path` with the home folder shown as `~`.
pub fn display_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn asset_cause(code: &str, e: &AssetError) -> Failure {
    Failure::new(code).with("cause", e.code())
}

/// The app's state behind the commands.
pub struct AppCore {
    paths: Paths,
    http: HttpConfig,
    engine: watch::Sender<EngineHandle>,
    /// Download timeout of the running engine task's asset manager.
    engine_timeout: Mutex<Duration>,
    profiles: ProfileStore,
    store: SettingsStore,
    settings: Mutex<Settings>,
    /// Held while the engine task may be replaced and until a start on it
    /// is done, so a concurrent preflight cannot replace (orphan) the
    /// engine a start is starting.
    engine_swap: tokio::sync::Mutex<()>,
    hook: Option<ConfigHook>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AppCore {
    /// Load the settings and spawn the engine (inside a tokio runtime).
    /// `http` gives the mirror; its download timeout comes from the
    /// settings. Error: `assets_failed`.
    pub fn new(paths: Paths, http: HttpConfig) -> Result<AppCore, Failure> {
        let store = SettingsStore::new(&paths.settings);
        let settings = store.load();
        let timeout = settings.download_timeout();
        let engine = EngineHandle::spawn(
            &paths.cache,
            HttpConfig {
                download_timeout: timeout,
                ..http.clone()
            },
        )?;
        Ok(AppCore {
            profiles: ProfileStore::new(&paths.profiles),
            paths,
            http,
            engine: watch::Sender::new(engine),
            engine_timeout: Mutex::new(timeout),
            store,
            settings: Mutex::new(settings),
            engine_swap: tokio::sync::Mutex::new(()),
            hook: None,
        })
    }

    /// Apply `hook` to every engine configuration (tests).
    pub fn with_config_hook(mut self, hook: ConfigHook) -> Self {
        self.hook = Some(hook);
        self
    }

    /// The current engine task.
    pub fn engine(&self) -> EngineHandle {
        self.engine.borrow().clone()
    }

    /// Follows engine replacements (a new download timeout needs a new
    /// engine task); the event forwarder re-subscribes on each change.
    pub fn watch_engine(&self) -> watch::Receiver<EngineHandle> {
        self.engine.subscribe()
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    // ------------------------------------------------------------ settings

    pub fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    /// Validate, store and apply the settings dialog's values. What the app
    /// remembers by itself (last interface, profile, auto-repeat) is kept
    /// from the current state ([`Settings::merged_edit`]), so a dialog
    /// opened earlier cannot undo it. Returns the stored settings. While
    /// running, the concurrency limit and the cooldown take effect at once;
    /// a new download timeout applies from the next start.
    pub async fn set_settings(&self, edit: Settings) -> Result<Settings, Failure> {
        let s = {
            // Held across the save, so the file and the state cannot
            // disagree with a concurrent `remember`.
            let mut current = lock(&self.settings);
            let s = current.merged_edit(edit);
            self.store.save(&s)?;
            *current = s.clone();
            s
        };
        let engine = self.engine();
        if engine.snapshot().await?.running {
            engine.set_max_parallel(s.max_parallel).await?;
            engine
                .set_cooldown(Duration::from_secs(s.cooldown_secs))
                .await?;
        }
        Ok(s)
    }

    /// Change a few fields and store them; a failure to store is only
    /// logged (remembering the last interface must not fail a start).
    fn remember(&self, change: impl FnOnce(&mut Settings)) {
        let mut s = lock(&self.settings);
        change(&mut s);
        if let Err(f) = self.store.save(&s) {
            tracing::warn!("cannot store settings: {f}");
        }
    }

    /// Replace the engine task if the download timeout changed and it is
    /// not running. The caller holds `engine_swap`.
    async fn ensure_engine(
        &self,
        _swap: &tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<EngineHandle, Failure> {
        let wanted = self.settings().download_timeout();
        let engine = self.engine();
        if *lock(&self.engine_timeout) == wanted || engine.snapshot().await?.running {
            return Ok(engine);
        }
        let fresh = EngineHandle::spawn(
            &self.paths.cache,
            HttpConfig {
                download_timeout: wanted,
                ..self.http.clone()
            },
        )?;
        *lock(&self.engine_timeout) = wanted;
        self.engine.send_replace(fresh.clone());
        tracing::info!(
            secs = wanted.as_secs(),
            "engine restarted for a new download timeout"
        );
        Ok(fresh)
    }

    // ------------------------------------------------------------ engine

    fn load_profile(&self, name: &str) -> Result<Profile, Failure> {
        self.profiles.load(name).map_err(without_paths)
    }

    /// The engine configuration for `req` and the current settings.
    pub fn engine_config(&self, req: &StartRequest) -> Result<EngineConfig, Failure> {
        let s = self.settings();
        let mut cfg = EngineConfig::new(&req.interface);
        // Server IP and netmask: the adapter's manual IPv4 in macOS.
        cfg.pool = s.pool_start.zip(s.pool_end);
        cfg.max_parallel = s.max_parallel;
        cfg.cooldown = Duration::from_secs(s.cooldown_secs);
        cfg.profile = req
            .profile
            .as_deref()
            .map(|n| self.load_profile(n))
            .transpose()?;
        cfg.auto_repeat = req.auto_repeat;
        if let Some(hook) = &self.hook {
            hook(&mut cfg);
        }
        Ok(cfg)
    }

    /// Run the preflight checks for `req`.
    pub async fn preflight(&self, req: &StartRequest) -> Result<PreflightReport, Failure> {
        let cfg = self.engine_config(req)?;
        let engine = {
            let swap = self.engine_swap.lock().await;
            self.ensure_engine(&swap).await?
        };
        Ok(preflight::run(&cfg, Some(engine.assets()), cfg.profile.as_ref()).await)
    }

    /// The quick, local preflight (interface, link, configured address):
    /// no port is bound and nothing is downloaded, so the window can repeat
    /// it every few seconds.
    pub async fn check_interface(&self, req: &StartRequest) -> Result<PreflightReport, Failure> {
        let cfg = self.engine_config(req)?;
        tokio::task::spawn_blocking(move || preflight::run_local(&cfg))
            .await
            .map_err(|e| Failure::new("interface_list_failed").with("detail", e))
    }

    /// Start serving; remembers the interface, profile and auto-repeat.
    pub async fn start(&self, req: &StartRequest) -> Result<StartInfo, Failure> {
        let cfg = self.engine_config(req)?;
        let swap = self.engine_swap.lock().await;
        let engine = self.ensure_engine(&swap).await?;
        let info = engine.start(cfg).await?;
        drop(swap);
        self.remember(|s| {
            s.last_interface = Some(req.interface.clone());
            s.last_profile = req.profile.clone();
            s.auto_repeat = req.auto_repeat;
        });
        Ok(info)
    }

    pub async fn stop(&self) -> Result<(), Failure> {
        self.engine().stop().await
    }

    pub async fn snapshot(&self) -> Result<Snapshot, Failure> {
        self.engine().snapshot().await
    }

    pub async fn flash(&self, mac: &str, profile: &str) -> Result<(), Failure> {
        let mac = parse_mac(mac)?;
        let profile = self.load_profile(profile)?;
        self.engine().flash(mac, profile).await
    }

    pub async fn retry(&self, mac: &str) -> Result<(), Failure> {
        self.engine().retry(parse_mac(mac)?).await
    }

    pub async fn cancel(&self, mac: &str) -> Result<(), Failure> {
        self.engine().cancel(parse_mac(mac)?).await
    }

    pub async fn forget(&self, mac: &str) -> Result<(), Failure> {
        self.engine().forget(parse_mac(mac)?).await
    }

    pub async fn set_auto_repeat(&self, on: bool) -> Result<(), Failure> {
        self.engine().set_auto_repeat(on).await?;
        self.remember(|s| s.auto_repeat = on);
        Ok(())
    }

    /// Change the active profile by name (`None`: none).
    pub async fn set_active_profile(&self, name: Option<&str>) -> Result<(), Failure> {
        let profile = name.map(|n| self.load_profile(n)).transpose()?;
        self.engine().set_active_profile(profile).await?;
        self.remember(|s| s.last_profile = name.map(String::from));
        Ok(())
    }

    pub async fn set_only_mac(&self, mac: Option<&str>) -> Result<(), Failure> {
        let mac = mac.map(parse_mac).transpose()?;
        self.engine().set_only_mac(mac).await
    }

    pub async fn set_max_parallel(&self, n: usize) -> Result<(), Failure> {
        self.engine().set_max_parallel(n).await?;
        self.remember(|s| s.max_parallel = n);
        Ok(())
    }

    pub async fn set_cooldown(&self, secs: u64) -> Result<(), Failure> {
        self.engine()
            .set_cooldown(Duration::from_secs(secs))
            .await?;
        self.remember(|s| s.cooldown_secs = secs);
        Ok(())
    }

    // ------------------------------------------------------------ profiles

    pub fn list_profiles(&self) -> ProfilesView {
        let listing = self.profiles.list();
        ProfilesView {
            profiles: listing.profiles,
            broken: listing
                .broken
                .into_iter()
                .map(|(path, f)| BrokenProfile {
                    file: path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    code: f.code,
                })
                .collect(),
        }
    }

    fn profile_exists(&self, name: &str) -> bool {
        !matches!(self.profiles.load(name), Err(f) if f.code == "profile_not_found")
    }

    /// Store `profile`; `previous` is its name before editing (`None` for a
    /// new one). Errors: profile validation codes, `profile_exists`
    /// (`name`; another profile has that name), `profile_io`.
    pub fn save_profile(&self, profile: &Profile, previous: Option<&str>) -> Result<(), Failure> {
        profile.validate()?;
        let same = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();
        let renamed = previous.filter(|p| !same(p, &profile.name));
        if (previous.is_none() || renamed.is_some()) && self.profile_exists(&profile.name) {
            return Err(Failure::new("profile_exists").with("name", &profile.name));
        }
        self.profiles.save(profile).map_err(without_paths)?;
        if let Some(old) = renamed {
            match self.profiles.delete(old) {
                Ok(()) => {}
                Err(f) if f.code == "profile_not_found" => {}
                Err(f) => return Err(without_paths(f)),
            }
            if self.settings().last_profile.as_deref() == Some(old) {
                self.remember(|s| s.last_profile = Some(profile.name.clone()));
            }
        }
        Ok(())
    }

    pub fn delete_profile(&self, name: &str) -> Result<(), Failure> {
        self.profiles.delete(name).map_err(without_paths)?;
        if self.settings().last_profile.as_deref() == Some(name) {
            self.remember(|s| s.last_profile = None);
        }
        Ok(())
    }

    /// Extra packages of `version` (a channel or a version) for `arch`.
    /// Error: `packages_unavailable` (`cause`: an asset error code).
    pub async fn available_packages(
        &self,
        version: &str,
        arch: &str,
    ) -> Result<PackagesView, Failure> {
        let fail = |e: AssetError| asset_cause("packages_unavailable", &e);
        let spec: VersionSpec = version.parse().map_err(|e| fail(AssetError::from(e)))?;
        let pkg_arch = PkgArch::from_device_arch(arch).map_err(|e| fail(e.into()))?;
        let engine = self.engine();
        let assets = engine.assets();
        let resolved = assets.resolve(&spec).await.map_err(fail)?;
        let packages = assets
            .available_packages(&resolved, pkg_arch)
            .await
            .map_err(fail)?;
        Ok(PackagesView {
            version: resolved.to_string(),
            arch: pkg_arch.as_str().to_string(),
            packages,
        })
    }

    /// Register a picked `.rsc` file. Errors: `not_a_script` (`name`),
    /// `script_failed` (`cause`).
    pub async fn register_script(&self, path: &Path) -> Result<ScriptView, Failure> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !name.to_lowercase().ends_with(".rsc") {
            return Err(Failure::new("not_a_script").with("name", name));
        }
        let file = self
            .engine()
            .assets()
            .register_local_file(path)
            .await
            .map_err(|e| asset_cause("script_failed", &e))?;
        debug_assert_eq!(file.kind, LocalKind::Script);
        Ok(ScriptView {
            name: file.name,
            size: file.size,
        })
    }

    // ------------------------------------------------------------ cache, log

    /// What is in the download cache: quick (sizes, remembered checks), or
    /// with every file's SHA-256 checked (`verify`). Error: `cache_failed`
    /// (`cause`).
    pub async fn cache_inventory(&self, verify: bool) -> Result<CacheView, Failure> {
        let engine = self.engine();
        let assets = engine.assets();
        let inventory = if verify {
            assets.verify_cache().await
        } else {
            assets.cached_inventory().await
        }
        .map_err(|e| asset_cause("cache_failed", &e))?;
        Ok(CacheView {
            location: display_path(&self.paths.cache),
            inventory,
        })
    }

    /// Delete part of the download cache; returns the bytes freed. Allowed
    /// while running, except for a version the engine needs
    /// ([`EngineHandle::versions_in_use`]). Errors: `asset_in_use`
    /// (`version`), `cache_failed` (`cause`: `invalid_item`, `busy` while
    /// it is being downloaded, `io`).
    pub async fn delete_cached(&self, scope: DeleteScope) -> Result<u64, Failure> {
        let engine = self.engine();
        let assets = engine.assets();
        let in_use = engine.versions_in_use().await?;
        if !in_use.is_empty() {
            let targets: Vec<Version> = match &scope {
                DeleteScope::Item { id } => item_version(id).into_iter().collect(),
                DeleteScope::Version { version } => Version::parse(version).into_iter().collect(),
                DeleteScope::All | DeleteScope::Damaged => {
                    let inv = assets
                        .cached_inventory()
                        .await
                        .map_err(|e| asset_cause("cache_failed", &e))?;
                    let damaged_only = scope == DeleteScope::Damaged;
                    inv.versions
                        .iter()
                        .filter(|v| !damaged_only || v.items.iter().any(|i| i.integrity.damaged()))
                        .filter_map(|v| Version::parse(&v.version).ok())
                        .collect()
                }
            };
            if let Some(v) = targets.iter().find(|v| in_use.contains(v)) {
                return Err(Failure::new("asset_in_use").with("version", v));
            }
        }
        assets
            .delete_cached(scope)
            .await
            .map_err(|e| asset_cause("cache_failed", &e))
    }

    /// Write the log text the window collected. Error: `log_io`.
    pub fn write_log(&self, path: &Path, text: &str) -> Result<(), Failure> {
        std::fs::write(path, text).map_err(|e| Failure::new("log_io").with("detail", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs_are_parsed_or_rejected() {
        assert_eq!(
            parse_mac("48:a9:8a:01:02:03").unwrap(),
            Mac([0x48, 0xa9, 0x8a, 1, 2, 3])
        );
        let err = parse_mac("nope").unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("mac")),
            ("invalid_mac", Some("nope"))
        );
    }

    #[test]
    fn user_paths_live_under_the_bundle_id() {
        let p = Paths::for_user().unwrap();
        assert!(p
            .cache
            .ends_with("Library/Caches/io.github.sepsig.mac-netinstall"));
        assert!(p.settings.ends_with(
            "Library/Application Support/io.github.sepsig.mac-netinstall/settings.json"
        ));
        assert_eq!(p.profiles.parent(), p.settings.parent());
    }

    #[test]
    fn display_paths_hide_the_home_folder() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(
            display_path(&home.join("Library/Caches/x")),
            "~/Library/Caches/x"
        );
        assert_eq!(display_path(Path::new("/tmp/x")), "/tmp/x");
    }
}
