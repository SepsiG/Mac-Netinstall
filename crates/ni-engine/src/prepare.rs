//! Turning a profile and a device's arch into an [`InstallPlan`], and
//! fetching the boot kernels.

use crate::event::Failure;
use crate::profile::{ConfigMode, Profile};
use ni_assets::{kernel_file_name, AssetError, Assets, KernelSet, PkgArch, Version, VersionSpec};
use ni_bootp::BootArch;
use ni_proto::InstallPlan;
use ni_tftp::FileMap;
use std::collections::HashMap;

/// An asset error as a device/engine failure: `code` with the asset
/// error's code as `cause`.
pub fn asset_failure(code: &str, e: &AssetError) -> Failure {
    Failure::new(code).with("cause", e.code()).with("detail", e)
}

/// Resolve the profile's version (or use `version` if the caller resolved
/// it already), then fetch packages and the script. Errors:
/// `assets_failed` (`cause`), `invalid_plan` (`detail`).
pub async fn prepare(
    assets: &Assets,
    profile: &Profile,
    version: Option<Version>,
    device_arch: &str,
) -> Result<InstallPlan, Failure> {
    let fail = |e: AssetError| asset_failure("assets_failed", &e);
    let arch = PkgArch::from_device_arch(device_arch).map_err(|e| fail(e.into()))?;
    let version = match version {
        Some(v) => v,
        None => assets.resolve(&profile.version).await.map_err(fail)?,
    };
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
    Ok(plan)
}

/// Kernels for `spec`. If the version cannot be resolved (offline), the
/// newest version with a complete cached kernel set is used instead, and
/// the second value is `true`.
pub async fn kernels(
    assets: &Assets,
    spec: &VersionSpec,
) -> Result<(Version, KernelSet, bool), Failure> {
    let (version, fallback) = match assets.resolve(spec).await {
        Ok(v) => (v, false),
        Err(e) => {
            let cached = assets.cached_versions().await.unwrap_or_default();
            match cached.into_iter().max() {
                Some(v) => (v, true),
                None => return Err(asset_failure("kernels_unavailable", &e)),
            }
        }
    };
    let set = assets
        .ensure_kernels(&version)
        .await
        .map_err(|e| asset_failure("kernels_unavailable", &e))?;
    Ok((version, set, fallback))
}

/// The BOOTP boot file map and the TFTP file map for a kernel set.
pub fn boot_maps(set: &KernelSet) -> (HashMap<BootArch, String>, FileMap) {
    let mut boot = HashMap::new();
    let mut files = FileMap::new();
    for (&arch, data) in set {
        let name = kernel_file_name(arch);
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
        let plan = prepare(&a, &p, None, "arm64").await.unwrap();
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
        let err = prepare(&a, &profile(), Some(v()), "x86").await.unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("cause")),
            ("assets_failed", Some("unsupported_arch"))
        );
        let mut p = profile();
        p.extra_packages = vec!["nosuch".into()];
        let err = prepare(&a, &p, Some(v()), "arm64").await.unwrap_err();
        assert_eq!(err.param("cause"), Some("unknown_package"));
        let err = prepare(&a, &profile(), Some(v()), "mmips")
            .await
            .unwrap_err();
        assert_eq!(err.param("cause"), Some("not_found"));
    }

    #[tokio::test]
    async fn kernels_fall_back_to_the_cache_when_offline() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-kernels");
        let a = assets(&srv, &dir);
        let spec = VersionSpec::Channel(Channel::Stable);
        let (version, set, fallback) = kernels(&a, &spec).await.unwrap();
        assert_eq!((version.as_str(), fallback), ("7.25.1", false));
        let (boot, files) = boot_maps(&set);
        assert_eq!(boot[&BootArch::Arm64], "kernel-arm64.elf");
        assert_eq!(files["kernel-arm64.elf"], set[&BootArch::Arm64]);
        assert_eq!(files.len(), 8);
        let testing = VersionSpec::Channel(Channel::Testing);
        let (version, _, fallback) = kernels(&a, &testing).await.unwrap();
        assert_eq!((version.as_str(), fallback), ("7.25.1", true));
        let empty = tempdir("prepare-kernels-empty");
        let err = kernels(&assets(&srv, &empty), &testing).await.unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("cause")),
            ("kernels_unavailable", Some("not_found"))
        );
    }
}
