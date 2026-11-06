//! RouterOS versions and release channels.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// A RouterOS v7+ version string such as `7.25.1`, `7.20rc3` or `7.26beta5`.
///
/// Validated so it is safe in URLs and file names: 1–32 characters of ASCII
/// digits, lowercase letters and dots, starting with a major version ≥ 7
/// (v6 packages are named and bundled differently).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version(String);

/// Why a version string was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VersionError {
    #[error("invalid RouterOS version {0:?}")]
    Invalid(String),
    #[error("RouterOS {0} is not supported (v7 or later is required)")]
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
        if major < 7 {
            return Err(VersionError::TooOld(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
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

/// MikroTik release channels (`https://upgrade.mikrotik.com/routeros/NEWESTa7.<channel>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    Stable,
    LongTerm,
    Testing,
    Development,
}

impl Channel {
    pub const ALL: [Channel; 4] = [
        Channel::Stable,
        Channel::LongTerm,
        Channel::Testing,
        Channel::Development,
    ];

    /// The channel name used in the `NEWESTa7.<channel>` URL.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::LongTerm => "long-term",
            Channel::Testing => "testing",
            Channel::Development => "development",
        }
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
    /// A channel name (`stable`, `long-term`, `testing`, `development`) or a version.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse::<Channel>() {
            Ok(c) => Ok(VersionSpec::Channel(c)),
            Err(_) => Version::parse(s).map(VersionSpec::Exact),
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
    fn rejects_routeros_6() {
        assert_eq!(
            Version::parse("6.49.10"),
            Err(VersionError::TooOld("6.49.10".into()))
        );
    }

    #[test]
    fn channel_names_match_the_upgrade_server() {
        let names: Vec<&str> = Channel::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(names, ["stable", "long-term", "testing", "development"]);
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
