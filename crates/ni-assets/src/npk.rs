//! RouterOS package (`.npk`) checks and checksums.
//!
//! An NPK starts with the magic `1E F1 D0 BA` followed by a little-endian
//! `u32` holding the length of the rest of the file, so a complete package
//! is exactly `8 + length` bytes (verified on 7.1, 7.12.1, 7.16.2 and
//! 7.25.1 packages).

use ni_proto::message::NPK_MAGIC;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Why a file is not a complete RouterOS package.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NpkError {
    #[error("{name} is not a RouterOS package (missing NPK magic)")]
    NotAnNpk { name: String },
    #[error("{name} is incomplete or damaged: header says {expected} bytes, file has {actual}")]
    LengthMismatch {
        name: String,
        expected: u64,
        actual: u64,
    },
}

/// Check `data` is a complete NPK (magic and header length).
pub fn check_npk(name: &str, data: &[u8]) -> Result<(), NpkError> {
    if data.len() < 8 || !data.starts_with(&NPK_MAGIC) {
        return Err(NpkError::NotAnNpk { name: name.into() });
    }
    let len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let expected = 8 + u64::from(len);
    let actual = data.len() as u64;
    if expected != actual {
        return Err(NpkError::LengthMismatch {
            name: name.into(),
            expected,
            actual,
        });
    }
    Ok(())
}

/// Lowercase hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Parse a MikroTik `<file>.sha256` body (`<64 hex digits>  <file name>\n`,
/// the `sha256sum` format). Returns the lowercase digest if the line names
/// `file_name`.
pub fn parse_sha256_file(body: &str, file_name: &str) -> Option<String> {
    let line = body.lines().next()?;
    let (digest, name) = line.split_once(char::is_whitespace)?;
    let name = name.trim_start().trim_start_matches('*').trim_end();
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) || name != file_name {
        return None;
    }
    Some(digest.to_ascii_lowercase())
}

/// Package names are lowercase ASCII letters, digits and `-`, starting with
/// a letter, at most 64 characters (`routeros`, `wifi-qcom-be`, …).
pub fn valid_package_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Package names from `all_packages-<arch>-<ver>.zip` entry names
/// (`<name>-<ver>-<arch>.npk`), sorted and de-duplicated. Other entries are
/// ignored.
pub fn package_names(entries: &[String], version: &str, arch: &str) -> Vec<String> {
    let suffix = format!("-{version}-{arch}.npk");
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|e| e.strip_suffix(&suffix))
        .filter(|n| valid_package_name(n))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::npk;

    #[test]
    fn accepts_a_complete_npk() {
        assert_eq!(check_npk("a.npk", &npk(100, 1)), Ok(()));
        assert_eq!(check_npk("a.npk", &npk(8, 1)), Ok(()));
    }

    #[test]
    fn rejects_missing_magic_and_short_files() {
        for data in [
            &b""[..],
            b"\x1e\xf1\xd0",
            b"PK\x03\x04\0\0\0\0",
            b"<html>404</html>",
        ] {
            assert_eq!(
                check_npk("x.npk", data),
                Err(NpkError::NotAnNpk {
                    name: "x.npk".into()
                })
            );
        }
    }

    #[test]
    fn rejects_truncated_and_padded_packages() {
        let full = npk(100, 1);
        assert_eq!(
            check_npk("x.npk", &full[..99]),
            Err(NpkError::LengthMismatch {
                name: "x.npk".into(),
                expected: 100,
                actual: 99
            })
        );
        let mut padded = full.clone();
        padded.push(0);
        assert!(matches!(
            check_npk("x.npk", &padded),
            Err(NpkError::LengthMismatch { actual: 101, .. })
        ));
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parses_mikrotik_checksum_files() {
        // Real body of routeros-7.25.1-arm64.npk.sha256.
        let body = "61d12f91c13185464e77696f3babc19dba65f58c2cc2605fcbe6fa83f8078e0b  routeros-7.25.1-arm64.npk\n";
        assert_eq!(
            parse_sha256_file(body, "routeros-7.25.1-arm64.npk").as_deref(),
            Some("61d12f91c13185464e77696f3babc19dba65f58c2cc2605fcbe6fa83f8078e0b")
        );
        let upper = body
            .to_ascii_uppercase()
            .replace("ROUTEROS-7.25.1-ARM64.NPK", "a.npk");
        assert_eq!(
            parse_sha256_file(&upper, "a.npk").as_deref(),
            Some("61d12f91c13185464e77696f3babc19dba65f58c2cc2605fcbe6fa83f8078e0b")
        );
        assert_eq!(parse_sha256_file(body, "other.npk"), None);
        assert_eq!(parse_sha256_file("abc  a.npk\n", "a.npk"), None);
        assert_eq!(parse_sha256_file("", "a.npk"), None);
        assert_eq!(parse_sha256_file("<?xml version=\"1.0\"?>", "a.npk"), None);
    }

    #[test]
    fn package_name_rules() {
        for ok in ["routeros", "wifi-qcom-be", "tr069-client", "iot-bt-extra"] {
            assert!(valid_package_name(ok), "{ok}");
        }
        for bad in ["", "Routeros", "../x", "a/b", "a.npk", "-x", "1x", "a b"] {
            assert!(!valid_package_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn package_names_from_zip_entries() {
        // Entry names as in all_packages-smips-7.25.1.zip, plus noise.
        let entries: Vec<String> = [
            "wireless-7.25.1-smips.npk",
            "hotspot-7.25.1-smips.npk",
            "tr069-client-7.25.1-smips.npk",
            "readme.txt",
            "gps-7.25.1-arm.npk",
            "dir/../evil-7.25.1-smips.npk",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            package_names(&entries, "7.25.1", "smips"),
            ["hotspot", "tr069-client", "wireless"]
        );
    }
}
