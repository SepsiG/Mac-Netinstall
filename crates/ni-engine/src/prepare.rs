//! Turning a profile and a device's arch into an [`InstallPlan`], and
//! fetching the boot kernels.

use crate::event::Failure;
use crate::profile::{ConfigMode, Profile};
use ni_assets::npk::package_arch;
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
    /// The RouterOS version installed.
    pub version: Version,
    pub plan: InstallPlan,
    /// The download architecture of the plan's packages.
    pub arch: PkgArch,
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

/// `unknown_device_arch`: no candidate download arch has packages built
/// for the device's hello arch. Carries everything a bug report needs:
/// `arch`, `model`, `boot_arch` (`?` without BOOTP), `candidates` and the
/// whole hello (`hello`, fields separated by ` | `). The license key is
/// given only as its length: the message asks to post this in a public
/// issue (the log file has the hello in full).
fn unknown_device_arch(target: &Target, candidates: &[PkgArch]) -> Failure {
    let info = &target.info;
    let key = match info.license_key.len() {
        0 => String::new(),
        n => format!("<license key, {n} chars>"),
    };
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
    let fail = |e: AssetError| asset_failure("assets_failed", &e);
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
/// `kernel_version` (the boot kernels the device runs), then fetch packages
/// and the scripts. The plan carries the profile's configuration options
/// and sends the system package first. Returns the version installed and
/// the plan. Every package is checked to be built for the device's hello
/// arch before anything is offered to it ([`verify_package_arches`]).
///
/// The download arch: the remembered resolution ([`Target::known`]) or
/// [`PkgArch::from_device_arch`]. If that names nothing (an arch string we
/// do not know) or its packages do not fit, the candidates are the download
/// arches of the device's boot kernel ([`PkgArch::for_boot_arch`]; all of
/// them without BOOTP): the `routeros` package of each is fetched (and
/// cached) until one says the hello arch inside, then its extra packages are
/// fetched and checked too ([`Prepared::resolved`]). Errors:
/// `assets_failed` (`cause`), `profile_version_mismatch` (`version`,
/// `kernel_version`), `package_arch_mismatch` (`arch`, `package`,
/// `package_arch`), `unknown_device_arch` (`arch`, `model`, `boot_arch`,
/// `candidates`, `hello`), `empty_script` (`name`), `invalid_plan`
/// (`detail`).
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
    let version = match version {
        Some(v) => v,
        None => {
            resolve_or_remembered(assets, &profile.version)
                .await
                .map_err(fail)?
                .0
        }
    };
    if let Some(k) = kernel_version.filter(|k| **k != version) {
        return Err(version_mismatch(&version, k));
    }
    let (packages, arch, resolved) =
        packages_for(assets, &version, &profile.extra_packages, target).await?;
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
    plan.system_package_first();
    plan.validate()
        .map_err(|e| Failure::new("invalid_plan").with("detail", e))?;
    Ok(Prepared {
        version,
        plan,
        arch,
        resolved: resolved.then_some(arch),
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
        assert_eq!(version, v());
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
        assert_eq!(err.param("cause"), Some("not_found"));
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
        assert_eq!(version, v());
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
        assert_eq!((version, plan.packages.len()), (v(), 1));
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
        assert_eq!((got.arch, got.resolved), (PkgArch::Mipsbe, None));
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
            (PkgArch::Smips, Some(PkgArch::Smips))
        );
        // Known from then on: no second resolution.
        let t = Target::new(hello("XMIPS")).with_known(Some(PkgArch::Smips));
        let got = prepare(&a, &profile(), Some(v()), None, &t).await.unwrap();
        assert_eq!((got.arch, got.resolved), (PkgArch::Smips, None));
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
            (PkgArch::Smips, Some(PkgArch::Smips))
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
            (PkgArch::Tile, Some(PkgArch::Tile))
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
            ("assets_failed", Some("not_found"))
        );
    }
}
