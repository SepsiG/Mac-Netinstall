//! MikroTik download URL scheme (verified 2026-11-06 with HEAD requests).
//!
//! - `<upgrade>/NEWESTa7.<channel>` → `<version> <unix time>\n`
//! - `<download>/<ver>/netinstall-<ver>.tar.gz`
//! - `<download>/<ver>/<name>-<ver>-<arch>.npk` (main package: `routeros`)
//! - `<download>/<ver>/all_packages-<arch>-<ver>.zip` (extra packages)
//! - `<file URL>.sha256` (`sha256sum` format; published since about 7.13,
//!   missing for 7.12.1 and older)
//!
//! Bases have no trailing `/`; see [`crate::HttpConfig`].

use crate::arch::PkgArch;
use crate::version::{Channel, Version};

/// Default download base.
pub const DOWNLOAD_BASE: &str = "https://download.mikrotik.com/routeros";
/// Default version-check base.
pub const UPGRADE_BASE: &str = "https://upgrade.mikrotik.com/routeros";
/// The main RouterOS package name.
pub const MAIN_PACKAGE: &str = "routeros";

pub fn newest(upgrade_base: &str, channel: Channel) -> String {
    format!("{upgrade_base}/NEWESTa7.{channel}")
}

pub fn tarball_name(version: &Version) -> String {
    format!("netinstall-{version}.tar.gz")
}

pub fn package_file_name(name: &str, version: &Version, arch: PkgArch) -> String {
    format!("{name}-{version}-{arch}.npk")
}

pub fn all_packages_name(version: &Version, arch: PkgArch) -> String {
    format!("all_packages-{arch}-{version}.zip")
}

/// URL of `file` in the version's download directory.
pub fn file(download_base: &str, version: &Version, file: &str) -> String {
    format!("{download_base}/{version}/{file}")
}

/// URL of the published checksum for `file_url`.
pub fn checksum(file_url: &str) -> String {
    format!("{file_url}.sha256")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_follow_the_verified_scheme() {
        let v = Version::parse("7.25.1").unwrap();
        assert_eq!(
            newest(UPGRADE_BASE, Channel::LongTerm),
            "https://upgrade.mikrotik.com/routeros/NEWESTa7.long-term"
        );
        assert_eq!(
            file(DOWNLOAD_BASE, &v, &tarball_name(&v)),
            "https://download.mikrotik.com/routeros/7.25.1/netinstall-7.25.1.tar.gz"
        );
        assert_eq!(
            file(
                DOWNLOAD_BASE,
                &v,
                &package_file_name(MAIN_PACKAGE, &v, PkgArch::Arm64)
            ),
            "https://download.mikrotik.com/routeros/7.25.1/routeros-7.25.1-arm64.npk"
        );
        assert_eq!(
            file(DOWNLOAD_BASE, &v, &all_packages_name(&v, PkgArch::Smips)),
            "https://download.mikrotik.com/routeros/7.25.1/all_packages-smips-7.25.1.zip"
        );
        assert_eq!(checksum("https://x/a.npk"), "https://x/a.npk.sha256");
    }
}
