//! Turning a profile and a device's arch into an [`InstallPlan`], and
//! fetching the boot kernels.

use crate::event::Failure;
use crate::profile::{ConfigMode, Profile};
use ni_assets::{AssetError, Assets, KernelSet, PkgArch, Version, VersionSpec};
use ni_bootp::BootArch;
use ni_proto::InstallPlan;
use ni_tftp::FileMap;
use std::collections::HashMap;

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
/// and the script. Returns the version installed and the plan. Errors:
/// `assets_failed` (`cause`), `profile_version_mismatch` (`version`,
/// `kernel_version`), `invalid_plan` (`detail`).
///
/// Why the mismatch is an error: the device already booted the kernel it
/// was served over TFTP, so the kernel cannot be switched for this install.
pub async fn prepare(
    assets: &Assets,
    profile: &Profile,
    version: Option<Version>,
    kernel_version: Option<&Version>,
    device_arch: &str,
) -> Result<(Version, InstallPlan), Failure> {
    let fail = |e: AssetError| asset_failure("assets_failed", &e);
    let arch = PkgArch::from_device_arch(device_arch).map_err(|e| fail(e.into()))?;
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
    let packages = assets
        .ensure_packages(&version, arch, &profile.extra_packages)
        .await
        .map_err(fail)?;
    let config_script = match &profile.config {
        ConfigMode::Default => None,
        ConfigMode::CustomScript { local_name } => {
            Some(assets.local_file(local_name).await.map_err(fail)?)
        }
    };
    let plan = InstallPlan {
        packages,
        config_script,
    };
    plan.validate()
        .map_err(|e| Failure::new("invalid_plan").with("detail", e))?;
    Ok((version, plan))
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
    use ni_assets::testutil::{assets, mirror, tempdir, v, TestServer};
    use ni_assets::Channel;

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
        p.config = ConfigMode::CustomScript {
            local_name: local.name,
        };
        let (version, plan) = prepare(&a, &p, None, None, "arm64").await.unwrap();
        assert_eq!(version, v());
        let names: Vec<&str> = plan.packages.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["routeros-7.25.1-arm64.npk", "wireless-7.25.1-arm64.npk"]
        );
        assert_eq!(plan.packages[0].data.as_ref(), main.as_slice());
        assert_eq!(plan.packages[1].data.as_ref(), wireless.as_slice());
        assert_eq!(
            plan.config_script.as_deref(),
            Some(&b"/system identity set name=x\n"[..])
        );
    }

    #[tokio::test]
    async fn failures_carry_the_asset_code() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-fail");
        let a = assets(&srv, &dir);
        let err = prepare(&a, &profile(), Some(v()), None, "x86")
            .await
            .unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("cause")),
            ("assets_failed", Some("unsupported_arch"))
        );
        let mut p = profile();
        p.extra_packages = vec!["nosuch".into()];
        let err = prepare(&a, &p, Some(v()), None, "arm64").await.unwrap_err();
        assert_eq!(err.param("cause"), Some("unknown_package"));
        let err = prepare(&a, &profile(), Some(v()), None, "mmips")
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
        let err = prepare(&a, &profile(), None, Some(&other), "arm64")
            .await
            .unwrap_err();
        assert_eq!(err, version_mismatch(&v(), &other));
        assert_eq!(err.param("kernel_version"), Some("7.24.6"));
        let (version, _) = prepare(&a, &profile(), None, Some(&v()), "arm64")
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
        prepare(&online, &p, None, None, "arm64").await.unwrap();
        let a = offline(&dir);
        let (version, plan) = prepare(&a, &p, None, None, "arm64").await.unwrap();
        assert_eq!((version, plan.packages.len()), (v(), 1));
        // Packages that were never cached cannot be had offline.
        p.extra_packages = vec!["wireless".into()];
        let err = prepare(&a, &p, None, None, "arm64").await.unwrap_err();
        assert_eq!(err.code, "assets_failed");
        // A channel never resolved here: nothing to fall back to.
        p.version = VersionSpec::Channel(Channel::Testing);
        let err = prepare(&a, &p, None, None, "arm64").await.unwrap_err();
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
