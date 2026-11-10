//! MikroTik download URL scheme (verified 2026-11-06 with HEAD requests).
//!
//! - `<upgrade>/NEWESTa7.<channel>` → `<version> <unix time>\n`
//!   (`NEWEST6.long-term` for the newest v6)
//! - `<download>/<ver>/netinstall-<ver>.tar.gz`
//! - `<download>/<ver>/<name>-<ver>-<arch>.npk` (main package: `routeros`)
//! - v6 main package: `<download>/<ver>/routeros-<arch6>-<ver>.npk` (arch
//!   *before* the version; `powerpc` for `ppc`), see [`main_package_file_name`]
//! - `<download>/<ver>/all_packages-<arch>-<ver>.zip` (extra packages; v6
//!   too, `ppc` stays `ppc`)
//! - `<download>/<ver>/CHANGELOG`: exists for every published version
//! - `<file URL>.sha256` (`sha256sum` format; required from 7.15 on, may be
//!   missing for older versions, verified whenever present — see
//!   [`Version::has_published_checksums`])
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
    format!("{upgrade_base}/{}", channel.newest_file())
}

pub fn tarball_name(version: &Version) -> String {
    format!("netinstall-{version}.tar.gz")
}

pub fn package_file_name(name: &str, version: &Version, arch: PkgArch) -> String {
    format!("{name}-{version}-{arch}.npk")
}

/// The packages a RouterOS v6 main package bundles (`routeros-<arch>-<ver>.npk`
/// holds them as flat parts; verified on 6.49.23). They are listed in the v6
/// `all_packages` zips too, but are never extra packages.
pub const V6_BUNDLE: &[&str] = &[
    "system",
    "ipv6",
    "wireless",
    "hotspot",
    "mpls",
    "routing",
    "ppp",
    "dhcp",
    "security",
    "advanced-tools",
];

/// The main package's file name: `routeros-<ver>-<arch>.npk`, or for v6
/// `routeros-<arch6>-<ver>.npk` where `arch6` is `powerpc` for `ppc` (the
/// v7 order answers 404 for v6).
pub fn main_package_file_name(version: &Version, arch: PkgArch) -> String {
    if version.is_v6() {
        let arch6 = match arch {
            PkgArch::Ppc => "powerpc",
            a => a.as_str(),
        };
        format!("{MAIN_PACKAGE}-{arch6}-{version}.npk")
    } else {
        package_file_name(MAIN_PACKAGE, version, arch)
    }
}

/// URL of a version's `CHANGELOG`: present for every published version,
/// so it tells whether a typed version exists.
pub fn changelog(download_base: &str, version: &Version) -> String {
    file(download_base, version, "CHANGELOG")
}

/// MikroTik's changelog page for one channel (`stable` or `longTerm`):
/// HTML with one `data-changelog-version` element per release.
pub fn changelogs_page(changelog_base: &str, channel_filter: &str) -> String {
    format!("{changelog_base}?channelFilter={channel_filter}")
}

/// Default changelog page base.
pub const CHANGELOG_BASE: &str = "https://mikrotik.com/download/changelogs";

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
        assert_eq!(
            main_package_file_name(&v, PkgArch::Ppc),
            "routeros-7.25.1-ppc.npk"
        );
    }

    /// Verified 2026-11-10 (see the v6 research note).
    #[test]
    fn v6_urls_put_the_arch_before_the_version() {
        let v6 = Version::parse("6.49.23").unwrap();
        assert_eq!(
            newest(UPGRADE_BASE, Channel::V6LongTerm),
            "https://upgrade.mikrotik.com/routeros/NEWEST6.long-term"
        );
        assert_eq!(
            file(
                DOWNLOAD_BASE,
                &v6,
                &main_package_file_name(&v6, PkgArch::Mipsbe)
            ),
            "https://download.mikrotik.com/routeros/6.49.23/routeros-mipsbe-6.49.23.npk"
        );
        assert_eq!(
            main_package_file_name(&v6, PkgArch::Ppc),
            "routeros-powerpc-6.49.23.npk"
        );
        assert_eq!(
            package_file_name("calea", &v6, PkgArch::Ppc),
            "calea-6.49.23-ppc.npk"
        );
        assert_eq!(
            all_packages_name(&v6, PkgArch::Ppc),
            "all_packages-ppc-6.49.23.zip"
        );
        assert_eq!(
            changelog(DOWNLOAD_BASE, &v6),
            "https://download.mikrotik.com/routeros/6.49.23/CHANGELOG"
        );
        assert_eq!(
            changelogs_page(CHANGELOG_BASE, "longTerm"),
            "https://mikrotik.com/download/changelogs?channelFilter=longTerm"
        );
    }
}
