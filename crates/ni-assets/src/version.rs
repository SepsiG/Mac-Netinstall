//! RouterOS versions and release channels.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// A RouterOS version string such as `7.25.1`, `7.20rc3`, `7.26beta5` or
/// `6.49.23`.
///
/// Validated so it is safe in URLs and file names: 1–32 characters of ASCII
/// digits, lowercase letters and dots, starting with a major version ≥ 7, or
/// 6 from 6.30 on ([`V6_FLOOR`]; v6 packages are named and bundled
/// differently, see [`crate::urls`]).
///
/// Ordering is semantic: numeric components compare as numbers (`7.9 <
/// 7.25`), and a pre-release sorts before its release (`7.26beta5 <
/// 7.26rc1 < 7.26`). Spellings that compare equal numerically (`7.25` and
/// `7.25.0`) are ordered by their text, so `Ord` stays consistent with `Eq`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version(String);

/// Why a version string was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VersionError {
    #[error("invalid RouterOS version {0:?}")]
    Invalid(String),
    #[error("RouterOS {0} is not supported (6.30 or later is required)")]
    TooOld(String),
}

impl Version {
    pub fn parse(s: &str) -> Result<Self, VersionError> {
        let s = s.trim();
        let invalid = || VersionError::Invalid(s.to_string());
        let well_formed = (1..=32).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'.')
            && s.starts_with(|c: char| c.is_ascii_digit())
            && s.contains('.')
            && !s.contains("..")
            && !s.ends_with('.');
        if !well_formed {
            return Err(invalid());
        }
        let major: u32 = s
            .split('.')
            .next()
            .and_then(|m| m.parse().ok())
            .ok_or_else(invalid)?;
        let minor: u32 = s
            .split('.')
            .nth(1)
            .map(|m| m.split(|c: char| !c.is_ascii_digit()).next().unwrap_or(""))
            .and_then(|m| m.parse().ok())
            .ok_or_else(invalid)?;
        if major < 6 || (major == 6 && minor < V6_FLOOR.1) {
            return Err(VersionError::TooOld(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }

    /// The major version (6, 7, ...).
    pub fn major(&self) -> u64 {
        self.sort_key().0.first().copied().unwrap_or(0)
    }

    /// A RouterOS v6 version (bundled main package, other file names).
    pub fn is_v6(&self) -> bool {
        self.major() == 6
    }

    /// Whether this version runs a mode script (`mode.scr`): RouterOS 7.22
    /// and later (its betas and rcs count as older).
    pub fn supports_mode_script(&self) -> bool {
        *self >= Version("7.22".into())
    }

    /// Whether this version is older than a device's minimum version
    /// (`min_os` of its Netinstall hello, e.g. `7.1`). The minimum is read
    /// loosely (`7` is 7.0); one that cannot be read is never a reason to
    /// refuse, so this is `false` then.
    pub fn below_minimum(&self, min_os: &str) -> bool {
        let min = min_os.trim();
        let ok = !min.is_empty()
            && min.len() <= 32
            && min
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        if !ok {
            return false;
        }
        let Some(mut min_nums) = min
            .split('.')
            .map(|p| p.parse::<u64>().ok())
            .collect::<Option<Vec<u64>>>()
        else {
            return false;
        };
        while min_nums.last() == Some(&0) {
            min_nums.pop();
        }
        // A minimum is a release: the same numbers with a pre-release tag
        // are still below it.
        self.sort_key() < (min_nums, 3, 0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `(numeric components, pre-release rank, pre-release number)`; a
    /// release ranks above `alpha` < `beta` < `rc` (unknown tags rank as
    /// `alpha`).
    fn sort_key(&self) -> (Vec<u64>, u8, u64) {
        let s = self.0.as_str();
        let split = s.find(|c: char| c.is_ascii_lowercase()).unwrap_or(s.len());
        let (head, tail) = s.split_at(split);
        let mut nums: Vec<u64> = head.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        while nums.last() == Some(&0) {
            nums.pop();
        }
        if tail.is_empty() {
            return (nums, 3, 0);
        }
        let digits = tail
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(tail.len());
        let (tag, num) = tail.split_at(digits);
        let rank = match tag {
            "beta" => 1,
            "rc" => 2,
            _ => 0,
        };
        let num = num
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (nums, rank, num)
    }

    /// Whether a published `<file>.sha256` is *required* for this version:
    /// from 7.15 on (7.15 final included; its betas and rcs count as
    /// older), where MikroTik publishes one for every file. Older versions
    /// may or may not have one (e.g. 7.14.3 does, 7.12.1 does not); a
    /// checksum that is present is verified for every version.
    ///
    /// For v6 that is 6.49.13 and later (6.49.10 has none).
    pub fn has_published_checksums(&self) -> bool {
        if self.is_v6() {
            *self >= Version("6.49.13".into())
        } else {
            *self >= Version("7.15".into())
        }
    }
}

/// The oldest RouterOS v6 accepted: 6.30 (older releases lack most of
/// today's architectures; `mipsle` ended in 6.3x and is not supported).
pub const V6_FLOOR: (u32, u32) = (6, 30);

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key()
            .cmp(&other.sort_key())
            .then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl FromStr for Version {
    type Err = VersionError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for Version {
    type Error = VersionError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<Version> for String {
    fn from(v: Version) -> Self {
        v.0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// MikroTik release channels
/// (`https://upgrade.mikrotik.com/routeros/NEWESTa7.<channel>`), plus the
/// pseudo-channel [`Channel::V6LongTerm`]: the newest RouterOS v6
/// (`NEWEST6.long-term`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    Stable,
    LongTerm,
    Testing,
    Development,
    /// The newest RouterOS v6 (end of life; long-term fixes only).
    #[serde(rename = "v6-long-term")]
    V6LongTerm,
}

impl Channel {
    pub const ALL: [Channel; 5] = [
        Channel::Stable,
        Channel::LongTerm,
        Channel::Testing,
        Channel::Development,
        Channel::V6LongTerm,
    ];

    /// The channel's name in profiles (`stable`, ..., `v6-long-term`).
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::LongTerm => "long-term",
            Channel::Testing => "testing",
            Channel::Development => "development",
            Channel::V6LongTerm => "v6-long-term",
        }
    }

    /// The file on the upgrade server naming the channel's newest version:
    /// `NEWESTa7.<channel>`, or `NEWEST6.long-term` for v6 (not
    /// `NEWEST6.testing` or `LATEST.6rc`, which are stale).
    pub fn newest_file(self) -> String {
        match self {
            Channel::V6LongTerm => "NEWEST6.long-term".to_string(),
            c => format!("NEWESTa7.{}", c.as_str()),
        }
    }

    /// The v6 pseudo-channel.
    pub fn is_v6(self) -> bool {
        self == Channel::V6LongTerm
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Channel {
    type Err = VersionError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Channel::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| VersionError::Invalid(s.to_string()))
    }
}

/// What a profile asks for: the newest version of a channel, or an exact one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VersionSpec {
    Channel(Channel),
    Exact(Version),
}

impl FromStr for VersionSpec {
    type Err = VersionError;
    /// A channel name (`stable`, `long-term`, `testing`, `development`,
    /// `v6-long-term`) or a version.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse::<Channel>() {
            Ok(c) => Ok(VersionSpec::Channel(c)),
            Err(_) => Version::parse(s).map(VersionSpec::Exact),
        }
    }
}

impl VersionSpec {
    /// Whether this asks for RouterOS v6 (an exact 6.x or `v6-long-term`).
    pub fn is_v6(&self) -> bool {
        match self {
            VersionSpec::Channel(c) => c.is_v6(),
            VersionSpec::Exact(v) => v.is_v6(),
        }
    }
}

/// The newest version of a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// Release time, seconds since the Unix epoch.
    pub released_unix: u64,
}

/// Parse a `NEWESTa7.<channel>` body: `<version> <unix timestamp>\n`.
pub fn parse_newest(body: &str) -> Option<Release> {
    let mut parts = body.split_whitespace();
    let version = Version::parse(parts.next()?).ok()?;
    let released_unix = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Release {
        version,
        released_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_release_beta_and_rc_versions() {
        for v in ["7.25.1", "7.1", "7.26beta5", "7.20rc3", "8.0", " 7.25.1\n"] {
            assert_eq!(Version::parse(v).unwrap().as_str(), v.trim(), "{v:?}");
        }
    }

    #[test]
    fn rejects_unsafe_or_malformed_versions() {
        for v in [
            "",
            "7",
            "7.",
            ".7",
            "7..1",
            "7.25/../x",
            "7.25 1",
            "v7.25",
            "7.25.1-arm",
            "7.25.X",
            "77777777777777777777777777777.1",
        ] {
            assert!(
                matches!(Version::parse(v), Err(VersionError::Invalid(_))),
                "{v:?}"
            );
        }
    }

    #[test]
    fn versions_order_semantically() {
        let p = |s: &str| Version::parse(s).unwrap();
        assert!(p("7.9") < p("7.25"));
        assert!(p("7.26beta5") < p("7.26"));
        assert!(p("7.26beta5") < p("7.26beta10"));
        assert!(p("7.26beta9") < p("7.26rc1"));
        assert!(p("7.26rc1") < p("7.26"));
        assert!(p("7.25.1") < p("7.26beta1"));
        assert!(p("7.25") < p("7.25.1"));
        assert!(p("7.25.10") > p("7.25.9"));
        assert!(p("8.0") > p("7.99.99"));
        // Consistent with Eq: different spellings are not equal.
        assert_ne!(p("7.25").cmp(&p("7.25.0")), std::cmp::Ordering::Equal);
        let mut all: Vec<Version> = ["7.26", "7.9", "7.26beta5", "7.25.1", "7.10"]
            .into_iter()
            .map(p)
            .collect();
        all.sort();
        let sorted: Vec<&str> = all.iter().map(Version::as_str).collect();
        assert_eq!(sorted, ["7.9", "7.10", "7.25.1", "7.26beta5", "7.26"]);
    }

    #[test]
    fn checksums_are_published_from_7_15() {
        let p = |s: &str| Version::parse(s).unwrap().has_published_checksums();
        assert!(p("7.15") && p("7.15.3") && p("7.25.1") && p("8.0"));
        assert!(!p("7.14") && !p("7.14.3") && !p("7.12.1") && !p("7.15beta4"));
    }

    #[test]
    fn accepts_routeros_6_from_6_30() {
        for v in ["6.49.23", "6.30", "6.30.4", "6.48.7", "6.49rc1"] {
            let parsed = Version::parse(v).unwrap();
            assert_eq!((parsed.major(), parsed.is_v6()), (6, true), "{v}");
        }
        for v in ["6.29.1", "6.0", "6.3", "5.26", "4.17", "0.1"] {
            assert_eq!(
                Version::parse(v),
                Err(VersionError::TooOld(v.into())),
                "{v}"
            );
        }
        let p = |s: &str| Version::parse(s).unwrap();
        assert!(p("6.49.23") < p("7.1"));
        assert!(p("6.30") < p("6.49") && p("6.49") < p("6.49.23"));
        assert_eq!(p("7.25.1").major(), 7);
        assert!(!p("7.25.1").is_v6());
    }

    #[test]
    fn v6_checksums_are_published_from_6_49_13() {
        let p = |s: &str| Version::parse(s).unwrap().has_published_checksums();
        assert!(p("6.49.13") && p("6.49.23"));
        assert!(!p("6.49.10") && !p("6.48.7") && !p("6.30"));
    }

    #[test]
    fn the_mode_script_needs_7_22() {
        let p = |s: &str| Version::parse(s).unwrap().supports_mode_script();
        assert!(p("7.22") && p("7.22.1") && p("7.25.1") && p("8.0"));
        assert!(!p("7.21.3") && !p("7.22rc1") && !p("6.49.23"));
    }

    #[test]
    fn minimum_versions_compare_loosely() {
        let p = |s: &str| Version::parse(s).unwrap();
        assert!(p("6.49.23").below_minimum("7.1"));
        assert!(p("6.49.23").below_minimum("7"));
        assert!(p("7.1").below_minimum("7.12"));
        assert!(!p("7.12").below_minimum("7.12"));
        assert!(!p("7.25.1").below_minimum("7.1"));
        assert!(!p("6.49.23").below_minimum("6.44"));
        assert!(!p("6.49.23").below_minimum("6.20"));
        assert!(p("7.20rc1").below_minimum("7.20"));
        // Unreadable minimums are not a reason to refuse.
        for odd in [
            "",
            "x",
            "7.a",
            "-1",
            "v7",
            "7..1",
            "99999999999999999999999",
        ] {
            assert!(!p("6.49.23").below_minimum(odd), "{odd:?}");
        }
    }

    #[test]
    fn the_v6_pseudo_channel_reads_newest6() {
        assert_eq!(Channel::V6LongTerm.as_str(), "v6-long-term");
        assert_eq!(Channel::V6LongTerm.newest_file(), "NEWEST6.long-term");
        assert_eq!(Channel::Stable.newest_file(), "NEWESTa7.stable");
        assert!(Channel::V6LongTerm.is_v6() && !Channel::LongTerm.is_v6());
        assert_eq!(
            "v6-long-term".parse::<VersionSpec>(),
            Ok(VersionSpec::Channel(Channel::V6LongTerm))
        );
        assert_eq!(
            serde_json::to_string(&Channel::V6LongTerm).unwrap(),
            "\"v6-long-term\""
        );
    }

    #[test]
    fn channel_names_match_the_upgrade_server() {
        let names: Vec<&str> = Channel::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "stable",
                "long-term",
                "testing",
                "development",
                "v6-long-term"
            ]
        );
        assert_eq!("long-term".parse::<Channel>(), Ok(Channel::LongTerm));
        assert!("lts".parse::<Channel>().is_err());
    }

    #[test]
    fn version_spec_parses_channels_and_versions() {
        assert_eq!(
            "stable".parse::<VersionSpec>(),
            Ok(VersionSpec::Channel(Channel::Stable))
        );
        assert_eq!(
            "7.24.6".parse::<VersionSpec>(),
            Ok(VersionSpec::Exact(Version::parse("7.24.6").unwrap()))
        );
        assert!("newest".parse::<VersionSpec>().is_err());
    }

    #[test]
    fn parses_newest_responses() {
        // Real responses captured on 2026-11-06.
        assert_eq!(
            parse_newest("7.25.1 1793623193\n"),
            Some(Release {
                version: Version::parse("7.25.1").unwrap(),
                released_unix: 1_793_623_193,
            })
        );
        assert_eq!(
            parse_newest("7.26beta5 1793358835\n").map(|r| r.version.to_string()),
            Some("7.26beta5".into())
        );
        for bad in ["", "7.25.1", "7.25.1 soon", "7.25.1 1 2", "<html> 1"] {
            assert_eq!(parse_newest(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn serde_round_trip_validates() {
        let spec = VersionSpec::Channel(Channel::LongTerm);
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(json, r#"{"channel":"long-term"}"#);
        assert_eq!(serde_json::from_str::<VersionSpec>(&json).unwrap(), spec);
        let exact: VersionSpec = serde_json::from_str(r#"{"exact":"7.25.1"}"#).unwrap();
        assert_eq!(exact, VersionSpec::Exact(Version::parse("7.25.1").unwrap()));
        assert!(serde_json::from_str::<Version>(r#""../etc""#).is_err());
    }
}
