//! Turning a profile and a device's arch into an [`InstallPlan`], and
//! fetching the boot kernels.

use crate::event::Failure;
use crate::profile::{follows_kernels, mode_script_needs_7_22, ConfigMode, PackageSource, Profile};
use ni_assets::npk::{contained_packages, package_arch, package_name, package_version};
use ni_assets::{ArchError, AssetError, Assets, KernelSet, PkgArch, Version, VersionSpec};
use ni_bootp::BootArch;
use ni_proto::{DeviceConfig, DeviceInfo, InstallPlan, PlanFile};
use ni_tftp::FileMap;
use std::collections::HashMap;

/// The device a plan is prepared for.
#[derive(Debug, Clone)]
pub struct Target {
    /// Its Netinstall hello (`arch` decides the packages).
    pub info: DeviceInfo,
    /// The boot kernel its BOOTP request asked for, if it was seen
    /// netbooting: narrows the candidates when its arch must be resolved.
    pub boot_arch: Option<BootArch>,
    /// The download arch its hello arch was resolved to earlier in this
    /// session (see [`Prepared::resolved`]).
    pub known: Option<PkgArch>,
}

impl Target {
    pub fn new(info: DeviceInfo) -> Self {
        Target {
            info,
            boot_arch: None,
            known: None,
        }
    }

    pub fn with_boot_arch(mut self, boot_arch: Option<BootArch>) -> Self {
        self.boot_arch = boot_arch;
        self
    }

    pub fn with_known(mut self, known: Option<PkgArch>) -> Self {
        self.known = known;
        self
    }
}

/// A plan ready to be offered.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The RouterOS version installed; `None` for local packages whose
    /// header names no version this app can read.
    pub version: Option<Version>,
    /// Downloaded or local packages.
    pub source: PackageSource,
    pub plan: InstallPlan,
    /// The download architecture of the plan's packages (`None` for local
    /// packages).
    pub arch: Option<PkgArch>,
    /// `Some(arch)` when the hello arch was resolved from package contents
    /// this time (unknown to [`PkgArch::from_device_arch`], or its choice
    /// did not fit): worth remembering for the session and reporting as
    /// `arch_resolved`.
    pub resolved: Option<PkgArch>,
}

/// The device's hello arch as compared with the packages' own: trimmed,
/// compared without case.
fn arch_matches(hello_arch: &str, package_arch: &str) -> bool {
    package_arch.trim().eq_ignore_ascii_case(hello_arch.trim())
}

/// Check that every package in `packages` is built for `hello_arch` (the
/// architecture inside the NPK, part type 16, is exactly the arch a device
/// sends in its hello). Error: `package_arch_mismatch` (`arch`, `package`,
/// `package_arch`; `?` if the package names none).
pub fn verify_package_arches(hello_arch: &str, packages: &[PlanFile]) -> Result<(), Failure> {
    for file in packages {
        let found = package_arch(&file.data);
        if !found.as_deref().is_ok_and(|a| arch_matches(hello_arch, a)) {
            let mut f = Failure::new("package_arch_mismatch")
                .with("arch", hello_arch.trim())
                .with("package", &file.name)
                .with("package_arch", found.as_deref().unwrap_or("?"));
            if let Err(e) = found {
                f = f.with("detail", e);
            }
            return Err(f);
        }
    }
    Ok(())
}

/// A license key as logs and reports show it: only its length
/// (`<license key, 12 chars>`; empty for none).
pub fn redacted_key(key: &str) -> String {
    match key.len() {
        0 => String::new(),
        n => format!("<license key, {n} chars>"),
    }
}

/// `unknown_device_arch`: no candidate download arch has packages built
/// for the device's hello arch. Carries everything a bug report needs:
/// `arch`, `model`, `boot_arch` (`?` without BOOTP), `candidates` and the
/// whole hello (`hello`, fields separated by ` | `). The license key is
/// given only as its length: the message asks to post this in a public
/// issue (the log file has the hello in full).
fn unknown_device_arch(target: &Target, candidates: &[PkgArch]) -> Failure {
    let info = &target.info;
    let key = redacted_key(&info.license_key);
    let hello: Vec<&str> = [
        &info.tag,
        &info.license_id,
        &key,
        &info.model,
        &info.arch,
        &info.min_os,
    ]
    .into_iter()
    .chain(&info.extra)
    .map(String::as_str)
    .collect();
    let candidates: Vec<&str> = candidates.iter().map(|a| a.as_str()).collect();
    Failure::new("unknown_device_arch")
        .with("arch", info.arch.trim())
        .with("model", &info.model)
        .with("boot_arch", target.boot_arch.map_or("?", |a| a.as_str()))
        .with("candidates", candidates.join(", "))
        .with("hello", hello.join(" | "))
}

/// The packages for `target`, verified to be built for its hello arch, and
/// their download arch; the third value is `true` if it had to be
/// resolved.
///
/// The fast path is the remembered resolution ([`Target::known`]) or
/// [`PkgArch::from_device_arch`]. If that names nothing (an arch string we
/// do not know) or its packages do not fit, the candidates are the
/// download arches of the device's boot kernel
/// ([`PkgArch::for_boot_arch`]; all of them without BOOTP): the `routeros`
/// package of each is fetched (and cached) until one says the hello arch
/// inside, then its extra packages are fetched and checked too.
async fn packages_for(
    assets: &Assets,
    version: &Version,
    extras: &[String],
    target: &Target,
) -> Result<(Vec<PlanFile>, PkgArch, bool), Failure> {
    let fail = |e: AssetError| package_failure(&e);
    let hello_arch = target.info.arch.trim();
    let fast = match target.known {
        Some(a) => Some(a),
        None => match PkgArch::from_device_arch(hello_arch) {
            Ok(a) => Some(a),
            Err(e @ ArchError::Unsupported(_)) => return Err(fail(e.into())),
            Err(ArchError::Unknown(_)) => None,
        },
    };
    let mut mismatch = None;
    if let Some(arch) = fast {
        let packages = assets
            .ensure_packages(version, arch, extras)
            .await
            .map_err(fail)?;
        match verify_package_arches(hello_arch, &packages) {
            Ok(()) => return Ok((packages, arch, false)),
            Err(f) => mismatch = Some(f),
        }
    }
    let pool: &[PkgArch] = match target.boot_arch {
        Some(boot) => PkgArch::for_boot_arch(boot),
        None => &PkgArch::ALL,
    };
    let candidates: Vec<PkgArch> = pool.iter().copied().filter(|a| Some(*a) != fast).collect();
    let mut fetch_error = None;
    let mut inspected = fast.is_some();
    for &arch in &candidates {
        let main = match assets.ensure_packages(version, arch, &[]).await {
            Ok(main) => main,
            Err(e) => {
                fetch_error.get_or_insert(e);
                continue;
            }
        };
        inspected = true;
        if verify_package_arches(hello_arch, &main).is_err() {
            continue;
        }
        let packages = assets
            .ensure_packages(version, arch, extras)
            .await
            .map_err(fail)?;
        verify_package_arches(hello_arch, &packages)?;
        return Ok((packages, arch, true));
    }
    if let Some(f) = mismatch {
        return Err(f);
    }
    match fetch_error {
        Some(e) if !inspected => Err(fail(e)),
        _ => Err(unknown_device_arch(target, &candidates)),
    }
}

/// A package download error as a device failure: `package_not_published`
/// (`arch`, `version`) for a main package that does not exist for the
/// device's arch (old v6 releases), `assets_failed` (`cause`) otherwise.
pub fn package_failure(e: &AssetError) -> Failure {
    match e {
        AssetError::PackageNotPublished { arch, version, .. } => {
            Failure::new("package_not_published")
                .with("arch", arch)
                .with("version", version)
                .with("cause", e.code())
                .with("detail", e)
        }
        e => asset_failure("assets_failed", e),
    }
}

/// The registered local packages of `profile`, checked: each must exist
/// (`package_file_missing`: `name`, `cause`) and be built for the hello
/// arch (`package_arch_mismatch`); exactly one must be a `system` package
/// (`missing_system_package`; a second one is `duplicate_package`). The
/// system package comes first, the rest in profile order. Also returns
/// the system package's version, if its header names one.
async fn local_packages(
    assets: &Assets,
    profile: &Profile,
    target: &Target,
) -> Result<(Vec<PlanFile>, Option<Version>), Failure> {
    let mut files = Vec::with_capacity(profile.local_packages.len());
    for name in &profile.local_packages {
        let data = assets.local_file(name).await.map_err(|e| {
            Failure::new("package_file_missing")
                .with("name", name)
                .with("cause", e.code())
        })?;
        files.push(PlanFile {
            name: name.clone(),
            data,
        });
    }
    let systems: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| package_name(&f.data).is_ok_and(|n| n == "system"))
        .map(|(i, _)| i)
        .collect();
    let main = match systems.as_slice() {
        [] => return Err(Failure::new("missing_system_package")),
        [i] => *i,
        [_, second, ..] => {
            return Err(Failure::new("duplicate_package").with("package", &files[*second].name))
        }
    };
    let system = files.remove(main);
    let version = package_version(&system.data);
    // A v6 bundle holds several packages; one of them again would be
    // installed twice.
    let bundled = contained_packages(&system.data);
    if let Some(dup) = files
        .iter()
        .find(|f| package_name(&f.data).is_ok_and(|n| bundled.contains(&n)))
    {
        return Err(Failure::new("duplicate_package").with("package", &dup.name));
    }
    // Packages of different versions do not work together.
    if let Some(v) = &version {
        for f in &files {
            if let Some(other) = package_version(&f.data).filter(|o| o != v) {
                return Err(Failure::new("package_version_mismatch")
                    .with("package", &f.name)
                    .with("package_version", other)
                    .with("version", v));
            }
        }
    } else {
        let known: Vec<(Version, &str)> = files
            .iter()
            .filter_map(|f| package_version(&f.data).map(|v| (v, f.name.as_str())))
            .collect();
        if let Some((first, _)) = known.first() {
            if let Some((other, name)) = known.iter().find(|(v, _)| v != first) {
                return Err(Failure::new("package_version_mismatch")
                    .with("package", name)
                    .with("package_version", other)
                    .with("version", first));
            }
        }
    }
    files.insert(0, system);
    verify_package_arches(&target.info.arch, &files)?;
    Ok((files, version))
}

/// `version_unknown` (`side`: `package` or `device`, `version` (`?` if
/// unknown), `min_os` as the device sent it): the version to install or the
/// device's minimum version cannot be read, so the minimum cannot be
/// checked; refused unless the profile allows going below the minimum.
pub fn version_unknown(side: &str, version: Option<&Version>, min_os: &str) -> Failure {
    Failure::new("version_unknown")
        .with("side", side)
        .with(
            "version",
            version.map_or("?".to_string(), Version::to_string),
        )
        .with("min_os", min_os)
}

/// `version_below_minimum` (`version`, `min_os`): installing a version
/// older than the device's minimum (its hello's `min_os`) can brick it.
pub fn version_below_minimum(version: &Version, min_os: &str) -> Failure {
    Failure::new("version_below_minimum")
        .with("version", version)
        .with("min_os", min_os.trim())
}

/// An asset error as a device/engine failure: `code` with the asset
/// error's code as `cause`.
pub fn asset_failure(code: &str, e: &AssetError) -> Failure {
    Failure::new(code).with("cause", e.code()).with("detail", e)
}

/// Resolve `spec`. If that fails (offline, mirror down) and `spec` is a
/// channel, fall back to the version the channel was last resolved to on
/// this cache ([`Assets::remembered`]) — never to some other cached
/// version, which could silently change what gets installed. The second
/// value is `true` for the fallback.
pub async fn resolve_or_remembered(
    assets: &Assets,
    spec: &VersionSpec,
) -> Result<(Version, bool), AssetError> {
    match assets.resolve(spec).await {
        Ok(v) => Ok((v, false)),
        Err(e) => match spec {
            VersionSpec::Channel(c) => match assets.remembered(*c).await {
                Some(v) => Ok((v, true)),
                None => Err(e),
            },
            VersionSpec::Exact(_) => Err(e),
        },
    }
}

/// Resolve the profile's version (or use `version` if the caller resolved
/// it already; see [`resolve_or_remembered`] for offline), check it against
/// `kernel_version` (the boot kernels the device runs) when the profile
/// follows them ([`follows_kernels`]: v7 downloads), then fetch packages
/// and the scripts. The plan carries the profile's configuration options
/// and sends the system package first. Every package is checked to be
/// built for the device's hello arch before anything is offered to it
/// ([`verify_package_arches`]).
///
/// With [`PackageSource::Local`] the packages are the profile's registered
/// local `.npk` files (see `local_packages`); the version is the one in the
/// system package's header, if readable.
///
/// The download arch: the remembered resolution ([`Target::known`]) or
/// [`PkgArch::from_device_arch`]. If that names nothing (an arch string we
/// do not know) or its packages do not fit, the candidates are the download
/// arches of the device's boot kernel ([`PkgArch::for_boot_arch`]; all of
/// them without BOOTP): the `routeros` package of each is fetched (and
/// cached) until one says the hello arch inside, then its extra packages are
/// fetched and checked too ([`Prepared::resolved`]).
///
/// Before anything is offered: a version below the device's minimum
/// (`min_os`) is refused unless the profile allows it
/// (`version_below_minimum`), and a mode script needs a known version
/// from 7.22 on (`mode_script_needs_7_22`, `version` `?` if unknown).
///
/// Errors: `assets_failed` (`cause`), `package_not_published` (`arch`,
/// `version`), `profile_version_mismatch` (`version`, `kernel_version`),
/// `package_arch_mismatch` (`arch`, `package`, `package_arch`),
/// `unknown_device_arch` (`arch`, `model`, `boot_arch`, `candidates`,
/// `hello`), `package_file_missing` (`name`, `cause`),
/// `missing_system_package`, `duplicate_package` (`package`),
/// `version_below_minimum` (`version`, `min_os`), `mode_script_needs_7_22`
/// (`version`), `empty_script` (`name`), `invalid_plan` (`detail`).
///
/// Why the mismatch is an error: the device already booted the kernel it
/// was served over TFTP, so the kernel cannot be switched for this install.
pub async fn prepare(
    assets: &Assets,
    profile: &Profile,
    version: Option<Version>,
    kernel_version: Option<&Version>,
    target: &Target,
) -> Result<Prepared, Failure> {
    let fail = |e: AssetError| asset_failure("assets_failed", &e);
    if let Err(e @ ArchError::Unsupported(_)) = PkgArch::from_device_arch(&target.info.arch) {
        return Err(fail(e.into()));
    }
    let (packages, version, arch, resolved) = match profile.package_source {
        PackageSource::Local => {
            let (packages, version) = local_packages(assets, profile, target).await?;
            (packages, version, None, false)
        }
        PackageSource::Download => {
            let version = match version {
                Some(v) => v,
                None => {
                    resolve_or_remembered(assets, &profile.version)
                        .await
                        .map_err(fail)?
                        .0
                }
            };
            if follows_kernels(profile) {
                if let Some(k) = kernel_version.filter(|k| **k != version) {
                    return Err(version_mismatch(&version, k));
                }
            }
            let (packages, arch, resolved) =
                packages_for(assets, &version, &profile.extra_packages, target).await?;
            (packages, Some(version), Some(arch), resolved)
        }
    };
    if !profile.allow_below_minimum {
        // Fails closed: what cannot be compared is not installed.
        let min_os = &target.info.min_os;
        match version
            .as_ref()
            .map(|v| (v, v.below_minimum_checked(min_os)))
        {
            Some((v, Some(true))) => return Err(version_below_minimum(v, min_os)),
            Some((_, Some(false))) => {}
            Some((v, None)) => return Err(version_unknown("device", Some(v), min_os)),
            None => return Err(version_unknown("package", None, min_os)),
        }
    }
    if profile.mode_script.is_some() && !version.as_ref().is_some_and(Version::supports_mode_script)
    {
        let shown = version.as_ref().map_or("?".to_string(), Version::to_string);
        return Err(mode_script_needs_7_22(&shown));
    }
    let config = match &profile.config {
        ConfigMode::Keep => DeviceConfig::Keep,
        ConfigMode::Default => DeviceConfig::Default,
        ConfigMode::Empty => DeviceConfig::Empty,
        ConfigMode::Script { local_name } => {
            let script = assets.local_file(local_name).await.map_err(fail)?;
            if script.is_empty() {
                return Err(empty_script(local_name));
            }
            DeviceConfig::Script(script)
        }
    };
    let mode_script = match &profile.mode_script {
        Some(name) => Some(assets.local_file(name).await.map_err(fail)?),
        None => None,
    };
    let mut plan = InstallPlan {
        packages,
        config,
        mode_script,
        remove_branding: profile.remove_branding,
        license_key: profile.license_key.clone().unwrap_or_default(),
        after_install: profile.after_install.into(),
    };
    if profile.package_source == PackageSource::Download {
        // Local packages are in order already (by content, not name).
        plan.system_package_first();
    }
    plan.validate()
        .map_err(|e| Failure::new("invalid_plan").with("detail", e))?;
    Ok(Prepared {
        version,
        source: profile.package_source,
        plan,
        arch,
        resolved: resolved.then_some(arch).flatten(),
    })
}

/// `empty_script` (`name`): the configuration script is empty. An empty
/// `autorun.scr` is what "empty configuration" sends, so that is the choice
/// to make instead (the UI says so).
pub fn empty_script(name: &str) -> Failure {
    Failure::new("empty_script").with("name", name)
}

/// `profile_version_mismatch`: installing `version` on a device that runs
/// boot kernels of `kernel_version`.
pub fn version_mismatch(version: &Version, kernel_version: &Version) -> Failure {
    Failure::new("profile_version_mismatch")
        .with("version", version)
        .with("kernel_version", kernel_version)
}

/// Kernels for `spec`: [`resolve_kernels`], then [`ensure_kernels`].
pub async fn kernels(
    assets: &Assets,
    spec: &VersionSpec,
) -> Result<(Version, KernelSet, bool), Failure> {
    let (version, fallback) = resolve_kernels(assets, spec).await?;
    let set = ensure_kernels(assets, &version).await?;
    Ok((version, set, fallback))
}

/// The version to fetch kernels of for `spec`. If it cannot be resolved
/// (offline), the version the channel was last resolved to is used if its
/// complete kernel set is cached, and the second value is `true`.
/// Otherwise `kernels_unavailable` (`cause`).
pub async fn resolve_kernels(
    assets: &Assets,
    spec: &VersionSpec,
) -> Result<(Version, bool), Failure> {
    let unavailable = |e: &AssetError| asset_failure("kernels_unavailable", e);
    Ok(match assets.resolve(spec).await {
        Ok(v) => (v, false),
        Err(e) => {
            let remembered = match spec {
                VersionSpec::Channel(c) => assets.remembered(*c).await,
                VersionSpec::Exact(_) => None,
            };
            let cached = assets.cached_versions().await.unwrap_or_default();
            match remembered.filter(|v| cached.contains(v)) {
                Some(v) => (v, true),
                None => return Err(unavailable(&e)),
            }
        }
    })
}

/// The kernel set of `version` (downloaded if needed). Error:
/// `kernels_unavailable` (`cause`).
pub async fn ensure_kernels(assets: &Assets, version: &Version) -> Result<KernelSet, Failure> {
    assets
        .ensure_kernels(version)
        .await
        .map_err(|e| asset_failure("kernels_unavailable", &e))
}

/// The name a boot kernel is served under: `kernel-<version>-<arch>.elf`.
/// The version is part of the name so a device can only fetch the kernel
/// its BOOTP reply named: after the kernels are swapped (another active
/// version), the old name is no longer served, and the engine knows from
/// the name alone which version a device booted.
pub fn served_kernel_name(version: &Version, arch: BootArch) -> String {
    format!("kernel-{version}-{arch}.elf")
}

/// The version in a [`served_kernel_name`] (`None` for other names).
pub fn served_kernel_version(name: &str) -> Option<Version> {
    let rest = name.strip_prefix("kernel-")?.strip_suffix(".elf")?;
    let (version, arch) = rest.rsplit_once('-')?;
    if arch.is_empty() {
        return None;
    }
    Version::parse(version).ok()
}

/// The BOOTP boot file map and the TFTP file map for the kernel set of
/// `version` (see [`served_kernel_name`]).
pub fn boot_maps(version: &Version, set: &KernelSet) -> (HashMap<BootArch, String>, FileMap) {
    let mut boot = HashMap::new();
    let mut files = FileMap::new();
    for (&arch, data) in set {
        let name = served_kernel_name(version, arch);
        boot.insert(arch, name.clone());
        files.insert(name, data.clone());
    }
    (boot, files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use ni_assets::testutil::{assets, mirror, tempdir, v, TestServer};
    use ni_assets::Channel;
    use ni_proto::{DeviceInfo, Mac};

    fn hello(arch: &str) -> DeviceInfo {
        DeviceInfo {
            mac: Mac([0x48, 0xa9, 0x8a, 0, 0, 1]),
            tag: "DSCV".into(),
            license_id: "LIC".into(),
            license_key: String::new(),
            model: "RB-test".into(),
            arch: arch.into(),
            min_os: "7.1".into(),
            extra: vec!["f7".into(), "f8".into()],
            hello_dst: Mac::BROADCAST,
        }
    }

    fn target(arch: &str) -> Target {
        Target::new(hello(arch))
    }

    fn profile() -> Profile {
        let mut p = Profile::new("t", VersionSpec::Channel(Channel::Stable));
        p.extra_packages = vec!["wireless".into()];
        p
    }

    fn offline(root: &std::path::Path) -> Assets {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let http = ni_assets::HttpConfig {
            download_base: format!("http://127.0.0.1:{port}/dl/"),
            upgrade_base: format!("http://127.0.0.1:{port}/up"),
            ..Default::default()
        };
        Assets::new(root, http).unwrap()
    }

    #[test]
    fn license_keys_are_never_shown() {
        assert_eq!(redacted_key(""), "");
        assert_eq!(redacted_key("SECRET-KEY"), "<license key, 10 chars>");
    }

    #[tokio::test]
    async fn plan_has_main_package_extras_and_script() {
        let srv = TestServer::start().await;
        let (_, main, wireless) = mirror(&srv);
        let dir = tempdir("prepare");
        let a = assets(&srv, &dir);
        let script = dir.join("Setup.rsc");
        std::fs::write(&script, b"/system identity set name=x\n").unwrap();
        let local = a.register_local_file(&script).await.unwrap();
        let mut p = profile();
        p.config = ConfigMode::Script {
            local_name: local.name,
        };
        let Prepared { version, plan, .. } =
            prepare(&a, &p, None, None, &target("arm64")).await.unwrap();
        assert_eq!(version, Some(v()));
        let names: Vec<&str> = plan.packages.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["routeros-7.25.1-arm64.npk", "wireless-7.25.1-arm64.npk"]
        );
        assert_eq!(plan.packages[0].data.as_ref(), main.as_slice());
        assert_eq!(plan.packages[1].data.as_ref(), wireless.as_slice());
        assert_eq!(
            plan.config,
            DeviceConfig::Script(Bytes::from_static(b"/system identity set name=x\n"))
        );
    }

    #[tokio::test]
    async fn an_empty_custom_script_is_refused() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-empty-script");
        let a = assets(&srv, &dir);
        let script = dir.join("empty.rsc");
        std::fs::write(&script, b"").unwrap();
        let mut p = profile();
        p.config = ConfigMode::Script {
            local_name: a.register_local_file(&script).await.unwrap().name,
        };
        let err = prepare(&a, &p, None, None, &target("arm64"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "empty_script");
        assert_eq!(err.param("name"), Some("empty.rsc"));
    }

    #[tokio::test]
    async fn plan_carries_the_configuration_options() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-options");
        let a = assets(&srv, &dir);
        let mode = dir.join("mode.rsc");
        std::fs::write(&mode, b":log info mode\n").unwrap();
        let mut p = profile();
        p.config = ConfigMode::Empty;
        p.mode_script = Some(a.register_local_file(&mode).await.unwrap().name);
        p.remove_branding = true;
        p.license_key = Some("KEY-1".into());
        p.after_install = crate::profile::AfterInstall::Shutdown;
        let Prepared { plan, .. } = prepare(&a, &p, None, None, &target("arm64")).await.unwrap();
        assert_eq!(plan.config, DeviceConfig::Empty);
        assert_eq!(plan.mode_script.as_deref(), Some(&b":log info mode\n"[..]));
        assert!(plan.remove_branding);
        assert_eq!(plan.license_key, "KEY-1");
        assert_eq!(plan.after_install, ni_proto::AfterInstall::Shutdown);
        let files: Vec<String> = plan.files().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            files,
            [
                "routeros-7.25.1-arm64.npk",
                "wireless-7.25.1-arm64.npk",
                "mode.scr",
                "autorun.scr"
            ]
        );
        p.config = ConfigMode::Keep;
        p.mode_script = None;
        let Prepared { plan, .. } = prepare(&a, &p, None, None, &target("arm64")).await.unwrap();
        assert!(plan.offer().keep_config);
        p.mode_script = Some("missing.rsc".into());
        p.config = ConfigMode::Default;
        let err = prepare(&a, &p, None, None, &target("arm64"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "assets_failed");
    }

    #[tokio::test]
    async fn failures_carry_the_asset_code() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-fail");
        let a = assets(&srv, &dir);
        let err = prepare(&a, &profile(), Some(v()), None, &target("x86"))
            .await
            .unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("cause")),
            ("assets_failed", Some("unsupported_arch"))
        );
        let mut p = profile();
        p.extra_packages = vec!["nosuch".into()];
        let err = prepare(&a, &p, Some(v()), None, &target("arm64"))
            .await
            .unwrap_err();
        assert_eq!(err.param("cause"), Some("unknown_package"));
        let err = prepare(&a, &profile(), Some(v()), None, &target("mmips"))
            .await
            .unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("arch"), err.param("version")),
            ("package_not_published", Some("mmips"), Some("7.25.1"))
        );
    }

    #[tokio::test]
    async fn the_version_must_match_the_boot_kernels() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-mismatch");
        let a = assets(&srv, &dir);
        let other = Version::parse("7.24.6").unwrap();
        let err = prepare(&a, &profile(), None, Some(&other), &target("arm64"))
            .await
            .unwrap_err();
        assert_eq!(err, version_mismatch(&v(), &other));
        assert_eq!(err.param("kernel_version"), Some("7.24.6"));
        let Prepared { version, .. } = prepare(&a, &profile(), None, Some(&v()), &target("arm64"))
            .await
            .unwrap();
        assert_eq!(version, Some(v()));
    }

    #[tokio::test]
    async fn offline_preparation_uses_the_remembered_version_and_cached_packages() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-offline");
        let online = assets(&srv, &dir);
        let mut p = profile();
        p.extra_packages.clear();
        prepare(&online, &p, None, None, &target("arm64"))
            .await
            .unwrap();
        let a = offline(&dir);
        let Prepared { version, plan, .. } =
            prepare(&a, &p, None, None, &target("arm64")).await.unwrap();
        assert_eq!((version, plan.packages.len()), (Some(v()), 1));
        // Packages that were never cached cannot be had offline.
        p.extra_packages = vec!["wireless".into()];
        let err = prepare(&a, &p, None, None, &target("arm64"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "assets_failed");
        // A channel never resolved here: nothing to fall back to.
        p.version = VersionSpec::Channel(Channel::Testing);
        let err = prepare(&a, &p, None, None, &target("arm64"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "assets_failed");
    }

    #[tokio::test]
    async fn kernels_fall_back_only_to_the_remembered_version() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-kernels");
        let a = assets(&srv, &dir);
        let spec = VersionSpec::Channel(Channel::Stable);
        let (version, set, fallback) = kernels(&a, &spec).await.unwrap();
        assert_eq!((version.as_str(), fallback), ("7.25.1", false));
        let (boot, files) = boot_maps(&version, &set);
        assert_eq!(boot[&BootArch::Arm64], "kernel-7.25.1-arm64.elf");
        assert_eq!(files["kernel-7.25.1-arm64.elf"], set[&BootArch::Arm64]);
        assert_eq!(
            served_kernel_version("kernel-7.25.1-arm64.elf"),
            Some(version.clone())
        );
        assert_eq!(files.len(), 8);
        // `testing` cannot be resolved and was never resolved here: no
        // fallback, although 7.25.1 kernels are cached.
        let testing = VersionSpec::Channel(Channel::Testing);
        let err = kernels(&a, &testing).await.unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("cause")),
            ("kernels_unavailable", Some("not_found"))
        );
        // Offline, stable stays on what it last resolved to.
        let off = offline(&dir);
        let (version, _, fallback) = kernels(&off, &spec).await.unwrap();
        assert_eq!((version.as_str(), fallback), ("7.25.1", true));
        // Remembered but without cached kernels: unavailable.
        let bare = tempdir("prepare-kernels-bare");
        assets(&srv, &bare).resolve(&spec).await.unwrap();
        let err = kernels(&offline(&bare), &spec).await.unwrap_err();
        assert_eq!(err.code, "kernels_unavailable");
        let empty = tempdir("prepare-kernels-empty");
        let err = kernels(&offline(&empty), &testing).await.unwrap_err();
        assert_eq!(err.code, "kernels_unavailable");
    }

    #[test]
    fn served_kernel_names_carry_their_version() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert_eq!(
            served_kernel_version("kernel-7.16rc2-mmips.elf"),
            Some(v("7.16rc2"))
        );
        for bad in [
            "kernel-arm64.elf",
            "kernel-7.25.1-arm64.bin",
            "other-7.25.1-arm64.elf",
            "kernel-x-arm64.elf",
            "",
        ] {
            assert_eq!(served_kernel_version(bad), None, "{bad}");
        }
    }
}

#[cfg(test)]
mod arch_tests {
    use super::*;
    use ni_assets::testutil::{assets, mirror, mirror_arch, tempdir, v, TestServer};
    use ni_assets::Channel;
    use ni_proto::{DeviceInfo, Mac};

    fn hello(arch: &str) -> DeviceInfo {
        DeviceInfo {
            mac: Mac([0x48, 0xa9, 0x8a, 0, 0, 1]),
            tag: "DSCV".into(),
            license_id: "LIC".into(),
            license_key: "KEY".into(),
            model: "RB-test".into(),
            arch: arch.into(),
            min_os: "7.1".into(),
            extra: vec!["f7".into(), "f8".into()],
            hello_dst: Mac::BROADCAST,
        }
    }

    fn profile() -> Profile {
        let mut p = Profile::new("t", VersionSpec::Channel(Channel::Stable));
        p.extra_packages = vec!["wireless".into()];
        p
    }

    fn names(plan: &InstallPlan) -> Vec<&str> {
        plan.packages.iter().map(|f| f.name.as_str()).collect()
    }

    /// The fast path: `mips` → the `mipsbe` downloads, whose packages say
    /// `mips` inside. Nothing to resolve.
    #[tokio::test]
    async fn a_known_arch_whose_packages_match_is_not_resolved() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_arch(&srv, "mipsbe", "mips");
        let dir = tempdir("prepare-arch-fast");
        let a = assets(&srv, &dir);
        let t = Target::new(hello("MIPS ")).with_boot_arch(Some(BootArch::Mips));
        let got = prepare(&a, &profile(), Some(v()), None, &t).await.unwrap();
        assert_eq!(
            names(&got.plan),
            ["routeros-7.25.1-mipsbe.npk", "wireless-7.25.1-mipsbe.npk"]
        );
        assert_eq!((got.arch, got.resolved), (Some(PkgArch::Mipsbe), None));
    }

    /// A device saying `smips` whose `smips` downloads say `mips` inside:
    /// refused, naming the first package that does not fit.
    #[tokio::test]
    async fn packages_built_for_another_arch_are_refused() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_arch(&srv, "smips", "mips");
        mirror_arch(&srv, "mipsbe", "mips");
        let dir = tempdir("prepare-arch-mismatch");
        let a = assets(&srv, &dir);
        let t = Target::new(hello("smips")).with_boot_arch(Some(BootArch::Mips));
        let err = prepare(&a, &profile(), Some(v()), None, &t)
            .await
            .unwrap_err();
        assert_eq!(err.code, "package_arch_mismatch");
        assert_eq!(err.param("arch"), Some("smips"));
        assert_eq!(err.param("package"), Some("routeros-7.25.1-smips.npk"));
        assert_eq!(err.param("package_arch"), Some("mips"));
    }

    /// An extra package that does not fit fails the plan as well.
    #[tokio::test]
    async fn every_package_of_the_plan_is_checked() {
        let srv = TestServer::start().await;
        mirror(&srv);
        // arm64 main package fine, but swap in an arm wireless package.
        let (_, wl) = mirror_arch(&srv, "arm", "arm");
        let file = "wireless-7.25.1-arm64.npk";
        srv.route(
            &format!("/dl/7.25.1/{file}"),
            ni_assets::testutil::Reply::Body(wl.clone()),
        );
        srv.route(
            &format!("/dl/7.25.1/{file}.sha256"),
            ni_assets::testutil::sum_file(&wl, file),
        );
        let dir = tempdir("prepare-arch-extra");
        let a = assets(&srv, &dir);
        let t = Target::new(hello("arm64")).with_boot_arch(Some(BootArch::Arm64));
        let err = prepare(&a, &profile(), Some(v()), None, &t)
            .await
            .unwrap_err();
        assert_eq!(err.code, "package_arch_mismatch");
        assert_eq!(err.param("package"), Some(file));
        assert_eq!(err.param("package_arch"), Some("arm"));
    }

    /// An arch string we do not know (`xmips`) from a board booting the
    /// MIPS kernel: of its candidates (mipsbe, smips), the smips download
    /// says `xmips` inside, so that is what gets installed.
    #[tokio::test]
    async fn an_unknown_arch_is_resolved_from_the_package_contents() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_arch(&srv, "mipsbe", "mips");
        mirror_arch(&srv, "smips", "xmips");
        let dir = tempdir("prepare-arch-resolve");
        let a = assets(&srv, &dir);
        let t = Target::new(hello("xmips")).with_boot_arch(Some(BootArch::Mips));
        let got = prepare(&a, &profile(), Some(v()), None, &t).await.unwrap();
        assert_eq!(
            names(&got.plan),
            ["routeros-7.25.1-smips.npk", "wireless-7.25.1-smips.npk"]
        );
        assert_eq!(
            (got.arch, got.resolved),
            (Some(PkgArch::Smips), Some(PkgArch::Smips))
        );
        // Known from then on: no second resolution.
        let t = Target::new(hello("XMIPS")).with_known(Some(PkgArch::Smips));
        let got = prepare(&a, &profile(), Some(v()), None, &t).await.unwrap();
        assert_eq!((got.arch, got.resolved), (Some(PkgArch::Smips), None));
    }

    /// A fast-path choice whose packages do not fit is corrected the same
    /// way (a board saying `mipsbe` whose packages say so in `smips`).
    #[tokio::test]
    async fn a_wrong_fast_path_choice_is_corrected() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_arch(&srv, "mipsbe", "mips");
        mirror_arch(&srv, "smips", "mipsbe");
        let dir = tempdir("prepare-arch-correct");
        let a = assets(&srv, &dir);
        let t = Target::new(hello("mipsbe")).with_boot_arch(Some(BootArch::Mips));
        let got = prepare(&a, &profile(), Some(v()), None, &t).await.unwrap();
        assert_eq!(
            (got.arch, got.resolved),
            (Some(PkgArch::Smips), Some(PkgArch::Smips))
        );
    }

    /// Without BOOTP, every download arch is a candidate.
    #[tokio::test]
    async fn without_a_boot_arch_every_download_arch_is_tried() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_arch(&srv, "tile", "tilegx");
        let dir = tempdir("prepare-arch-all");
        let a = assets(&srv, &dir);
        let t = Target::new(hello("tilegx"));
        let got = prepare(&a, &profile(), Some(v()), None, &t).await.unwrap();
        assert_eq!(
            (got.arch, got.resolved),
            (Some(PkgArch::Tile), Some(PkgArch::Tile))
        );
    }

    /// No candidate's packages say `xmips`: `unknown_device_arch` with
    /// everything needed for a bug report.
    #[tokio::test]
    async fn an_arch_no_package_matches_is_unknown_device_arch() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_arch(&srv, "mipsbe", "mips");
        mirror_arch(&srv, "smips", "smips");
        let dir = tempdir("prepare-arch-unknown");
        let a = assets(&srv, &dir);
        let t = Target::new(hello("xmips")).with_boot_arch(Some(BootArch::Mips));
        let err = prepare(&a, &profile(), Some(v()), None, &t)
            .await
            .unwrap_err();
        assert_eq!(err.code, "unknown_device_arch");
        assert_eq!(err.param("arch"), Some("xmips"));
        assert_eq!(err.param("model"), Some("RB-test"));
        assert_eq!(err.param("boot_arch"), Some("mips"));
        assert_eq!(err.param("candidates"), Some("mipsbe, smips"));
        assert_eq!(
            err.param("hello"),
            Some("DSCV | LIC | <license key, 3 chars> | RB-test | xmips | 7.1 | f7 | f8")
        );
    }

    /// No candidate could be fetched at all (offline): that is the error,
    /// not an unknown arch.
    #[tokio::test]
    async fn unreachable_candidates_report_the_download_error() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-arch-offline");
        let a = assets(&srv, &dir);
        let t = Target::new(hello("xmips")).with_boot_arch(Some(BootArch::Mips));
        let err = prepare(&a, &profile(), Some(v()), None, &t)
            .await
            .unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("cause")),
            ("package_not_published", Some("package_not_published"))
        );
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use crate::profile::PackageSource;
    use ni_assets::testutil::{
        assets, mirror, mirror_v6, npk_package, npk_package_version, tempdir, TestServer, V6,
    };
    use ni_proto::{DeviceInfo, Mac};

    fn hello(arch: &str, min_os: &str) -> Target {
        Target::new(DeviceInfo {
            mac: Mac([0x48, 0xa9, 0x8a, 0, 0, 2]),
            tag: "DSCV".into(),
            license_id: "LIC".into(),
            license_key: String::new(),
            model: "hEX".into(),
            arch: arch.into(),
            min_os: min_os.into(),
            extra: vec!["f7".into(), "f8".into()],
            hello_dst: Mac::BROADCAST,
        })
    }

    fn kernels() -> Version {
        Version::parse("7.25.1").unwrap()
    }

    fn names(p: &Prepared) -> Vec<&str> {
        p.plan.packages.iter().map(|f| f.name.as_str()).collect()
    }

    /// A v6 install boots the 7.x kernels: no version mismatch, the bundle
    /// first, bundled extras dropped.
    #[tokio::test]
    async fn v6_extras_already_in_the_downloaded_bundle_are_dropped() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_v6(&srv, V6, "arm", "arm", true);
        // This bundle holds `gps` too (not one of the usual parts).
        let bundle =
            ni_assets::testutil::npk_bundle("arm", "arm", (6, 49, 23), &["system", "gps"], 2000);
        let file = "routeros-arm-6.49.23.npk";
        srv.route(
            &format!("/dl/{V6}/{file}"),
            ni_assets::testutil::Reply::Body(bundle.clone()),
        );
        srv.route(
            &format!("/dl/{V6}/{file}.sha256"),
            ni_assets::testutil::sum_file(&bundle, file),
        );
        let dir = tempdir("prepare-v6-contained");
        let a = assets(&srv, &dir);
        let mut p = Profile::new("v6", VersionSpec::Exact(Version::parse(V6).unwrap()));
        p.extra_packages = vec!["gps".into(), "calea".into()];
        let got = prepare(&a, &p, None, None, &hello("arm", "6.40"))
            .await
            .unwrap();
        assert_eq!(names(&got), [file, "calea-6.49.23-arm.npk"]);
    }

    #[tokio::test]
    async fn v6_downloads_install_with_the_v7_kernels() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let (bundle, _) = mirror_v6(&srv, V6, "mmips", "mmips", true);
        let dir = tempdir("prepare-v6");
        let a = assets(&srv, &dir);
        let mut p = Profile::new("v6", "v6-long-term".parse().unwrap());
        p.extra_packages = vec!["wireless".into(), "calea".into()];
        let got = prepare(&a, &p, None, Some(&kernels()), &hello("mmips", "6.40"))
            .await
            .unwrap();
        assert_eq!(got.version.as_ref().map(Version::as_str), Some(V6));
        assert_eq!(got.source, PackageSource::Download);
        assert_eq!(
            names(&got),
            ["routeros-mmips-6.49.23.npk", "calea-6.49.23-mmips.npk"]
        );
        assert_eq!(got.plan.packages[0].data.as_ref(), bundle.as_slice());
        // The hello's arch is checked in v6 packages too.
        let err = prepare(&a, &p, None, Some(&kernels()), &hello("smips", "6.40"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "package_not_published");
        assert_eq!(err.param("arch"), Some("smips"));
        assert_eq!(err.param("version"), Some(V6));
    }

    /// Below the device's minimum version: refused before anything is
    /// offered, unless the profile allows it.
    #[tokio::test]
    async fn versions_below_the_device_minimum_are_refused_unless_allowed() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_v6(&srv, V6, "arm", "arm", true);
        let dir = tempdir("prepare-min-os");
        let a = assets(&srv, &dir);
        let mut p = Profile::new("v6", VersionSpec::Exact(Version::parse(V6).unwrap()));
        let err = prepare(&a, &p, None, Some(&kernels()), &hello("arm", "7.1"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "version_below_minimum");
        assert_eq!(err.param("version"), Some(V6));
        assert_eq!(err.param("min_os"), Some("7.1"));
        p.allow_below_minimum = true;
        prepare(&a, &p, None, Some(&kernels()), &hello("arm", "7.1"))
            .await
            .unwrap();
        // An unreadable minimum fails closed: it cannot be checked.
        p.allow_below_minimum = false;
        for raw in ["n/a", ""] {
            let err = prepare(&a, &p, None, Some(&kernels()), &hello("arm", raw))
                .await
                .unwrap_err();
            assert_eq!(err.code, "version_unknown", "{raw:?}");
            assert_eq!(err.param("side"), Some("device"));
            assert_eq!(err.param("min_os"), Some(raw));
            assert_eq!(err.param("version"), Some(V6));
        }
        p.allow_below_minimum = true;
        prepare(&a, &p, None, Some(&kernels()), &hello("arm", "n/a"))
            .await
            .unwrap();
    }

    async fn register(a: &Assets, dir: &std::path::Path, name: &str, data: &[u8]) -> String {
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        a.register_local_file(&path).await.unwrap().name
    }

    fn local(names: &[String]) -> Profile {
        let mut p = Profile::new("local", VersionSpec::Channel(ni_assets::Channel::Stable));
        p.package_source = PackageSource::Local;
        p.local_packages = names.to_vec();
        p
    }

    /// Local packages: no download, the system package first, its version
    /// read from its header, every file checked against the hello arch.
    #[tokio::test]
    async fn local_packages_are_installed_system_first() {
        let srv = TestServer::start().await;
        let dir = tempdir("prepare-local");
        let a = assets(&srv, &dir);
        let wifi = register(
            &a,
            &dir,
            "Wifi.npk",
            &npk_package_version("wifi-qcom", "arm", (7, 24, 6), 900, 2),
        )
        .await;
        let main = register(
            &a,
            &dir,
            "my-routeros.npk",
            &npk_package_version("system", "arm", (7, 24, 6), 1200, 1),
        )
        .await;
        let p = local(&[wifi.clone(), main.clone()]);
        // Any kernel version: local packages do not follow the kernels.
        let got = prepare(&a, &p, None, Some(&kernels()), &hello("arm", "7.1"))
            .await
            .unwrap();
        assert_eq!(names(&got), ["my-routeros.npk", "wifi.npk"]);
        assert_eq!(got.version.as_ref().map(Version::as_str), Some("7.24.6"));
        assert_eq!(got.source, PackageSource::Local);
        assert!(srv.paths().is_empty(), "nothing downloaded");
        // A device of another arch: refused before formatting.
        let err = prepare(&a, &p, None, Some(&kernels()), &hello("mmips", "7.1"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "package_arch_mismatch");
        assert_eq!(err.param("package"), Some("my-routeros.npk"));
        // Extras only: no system package.
        let err = prepare(
            &a,
            &local(std::slice::from_ref(&wifi)),
            None,
            None,
            &hello("arm", "7.1"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "missing_system_package");
        // A file that is gone from the cache.
        let err = prepare(
            &a,
            &local(&[main.clone(), "gone.npk".into()]),
            None,
            None,
            &hello("arm", "7.1"),
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("name")),
            ("package_file_missing", Some("gone.npk"))
        );
        // The device minimum applies to the header's version.
        let err = prepare(&a, &p, None, None, &hello("arm", "7.25"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "version_below_minimum");
    }

    /// The mode script needs 7.22: checked against the local system
    /// package's version; an unknown version is refused too.
    #[tokio::test]
    async fn the_mode_script_needs_a_known_version_from_7_22() {
        let srv = TestServer::start().await;
        let dir = tempdir("prepare-local-mode");
        let a = assets(&srv, &dir);
        let mode = register(&a, &dir, "mode.rsc", b":log info x\n").await;
        let old = register(
            &a,
            &dir,
            "old.npk",
            &npk_package_version("system", "arm", (7, 21, 3), 900, 1),
        )
        .await;
        let new = register(
            &a,
            &dir,
            "new.npk",
            &npk_package_version("system", "arm", (7, 22, 0), 900, 1),
        )
        .await;
        let odd = register(&a, &dir, "odd.npk", &npk_package("system", "arm", 900, 1)).await;
        for (file, want) in [(&old, Some("7.21.3")), (&odd, Some("?")), (&new, None)] {
            let mut p = local(std::slice::from_ref(file));
            p.mode_script = Some(mode.clone());
            p.allow_below_minimum = true;
            let got = prepare(&a, &p, None, None, &hello("arm", "7.1")).await;
            match want {
                None => assert!(got.is_ok(), "{file}: {got:?}"),
                Some(v) => {
                    let err = got.unwrap_err();
                    assert_eq!(
                        (err.code.as_str(), err.param("version")),
                        ("mode_script_needs_7_22", Some(v)),
                        "{file}"
                    );
                }
            }
        }
        // An unknown version cannot be checked against the device's
        // minimum: refused unless allowed, then shown as unknown.
        let err = prepare(
            &a,
            &local(std::slice::from_ref(&odd)),
            None,
            None,
            &hello("arm", "7.1"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "version_unknown");
        assert_eq!(
            (err.param("side"), err.param("version"), err.param("min_os")),
            (Some("package"), Some("?"), Some("7.1"))
        );
        let mut allowed = local(&[odd]);
        allowed.allow_below_minimum = true;
        let got = prepare(&a, &allowed, None, None, &hello("arm", "7.1"))
            .await
            .unwrap();
        assert_eq!(got.version, None);
    }

    /// Local files: a bundle's own parts are not installed again, and all
    /// known versions must agree.
    #[tokio::test]
    async fn local_bundles_and_versions_must_be_consistent() {
        let srv = TestServer::start().await;
        let dir = tempdir("prepare-local-consistent");
        let a = assets(&srv, &dir);
        let bundle = ni_assets::testutil::npk_bundle(
            "arm",
            "arm",
            (6, 49, 23),
            &["system", "wireless@", "ppp"],
            2000,
        );
        let main = register(&a, &dir, "routeros-arm-6.49.23.npk", &bundle).await;
        let wl = register(
            &a,
            &dir,
            "wireless-6.49.23-arm.npk",
            &npk_package_version("wireless", "arm", (6, 49, 23), 500, 2),
        )
        .await;
        let calea = register(
            &a,
            &dir,
            "calea-6.49.23-arm.npk",
            &npk_package_version("calea", "arm", (6, 49, 23), 500, 3),
        )
        .await;
        let new_calea = register(
            &a,
            &dir,
            "calea-6.49.22-arm.npk",
            &npk_package_version("calea", "arm", (6, 49, 22), 500, 4),
        )
        .await;
        let odd = register(&a, &dir, "gps.npk", &npk_package("gps", "arm", 500, 5)).await;
        let target = || hello("arm", "6.40");
        let err = prepare(&a, &local(&[main.clone(), wl]), None, None, &target())
            .await
            .unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("package")),
            ("duplicate_package", Some("wireless-6.49.23-arm.npk"))
        );
        let err = prepare(
            &a,
            &local(&[main.clone(), new_calea]),
            None,
            None,
            &target(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "package_version_mismatch");
        assert_eq!(
            (
                err.param("package"),
                err.param("package_version"),
                err.param("version")
            ),
            (
                Some("calea-6.49.22-arm.npk"),
                Some("6.49.22"),
                Some("6.49.23")
            )
        );
        // Unknown versions do not count against the others.
        let got = prepare(&a, &local(&[main, calea, odd]), None, None, &target())
            .await
            .unwrap();
        assert_eq!(names(&got).len(), 3);
    }
}
