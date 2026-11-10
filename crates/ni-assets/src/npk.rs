//! RouterOS package (`.npk`) checks and checksums.
//!
//! An NPK starts with the magic `1E F1 D0 BA` followed by a little-endian
//! `u32` holding the length of the rest of the file, so a complete package
//! is exactly `8 + length` bytes (verified on 7.1, 7.12.1, 7.16.2 and
//! 7.25.1 packages).
//!
//! The rest is a sequence of parts, each `u16 type, u32 length, data`
//! (little-endian). Part type 1 is the package header (its name,
//! NUL-padded to 16 bytes, then version data); the first type-16 part after
//! it is the architecture the package is built for, which is exactly the
//! arch a device sends in its Netinstall hello (`routeros-*-mipsbe.npk`
//! says `mips`, `-mmips` says `mmips`, `-arm` says `arm`). See
//! [`package_header`].

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
    /// The part list has no readable package name or architecture.
    #[error("malformed RouterOS package: {reason}")]
    Malformed { reason: &'static str },
}

/// Part type of the package header (name and version).
pub const PART_HEADER: u16 = 1;
/// Part type of the architecture (the first one after the header).
pub const PART_ARCH: u16 = 16;

/// What [`package_header`] reads from an NPK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpkHeader {
    /// The package name (`system` for `routeros-*.npk`, `wireless`, ...).
    pub name: String,
    /// The architecture as the device names it in its hello (`mips` for
    /// the `mipsbe` downloads).
    pub arch: String,
}

/// Read the package name (part type 1) and architecture (the first part of
/// type 16 after it). Every length is bounds-checked: hostile input is an
/// error, never a panic. The NPK length header itself is [`check_npk`]'s
/// business and not checked here.
pub fn package_header(data: &[u8]) -> Result<NpkHeader, NpkError> {
    let malformed = |reason| NpkError::Malformed { reason };
    if data.len() < 8 || !data.starts_with(&NPK_MAGIC) {
        return Err(malformed("missing NPK magic"));
    }
    let mut name: Option<String> = None;
    let mut off = 8usize;
    while data.len().saturating_sub(off) >= 6 {
        let kind = u16::from_le_bytes([data[off], data[off + 1]]);
        let len = u32::from_le_bytes([data[off + 2], data[off + 3], data[off + 4], data[off + 5]]);
        let start = off + 6;
        let end = usize::try_from(len)
            .ok()
            .and_then(|l| start.checked_add(l))
            .filter(|&e| e <= data.len())
            .ok_or(malformed("a part runs past the end of the file"))?;
        let body = &data[start..end];
        match (kind, &name) {
            (PART_HEADER, None) => {
                let raw = &body[..body.len().min(16)];
                let raw = raw.split(|&b| b == 0).next().unwrap_or_default();
                name = Some(text(raw).ok_or(malformed("unreadable package name"))?);
            }
            (PART_ARCH, Some(name)) => {
                let arch = text(body).ok_or(malformed("unreadable architecture"))?;
                return Ok(NpkHeader {
                    name: name.clone(),
                    arch,
                });
            }
            _ => {}
        }
        off = end;
    }
    Err(malformed(if name.is_some() {
        "no architecture part"
    } else {
        "no package header"
    }))
}

/// The architecture an NPK is built for (see [`package_header`]).
pub fn package_arch(data: &[u8]) -> Result<String, NpkError> {
    package_header(data).map(|h| h.arch)
}

/// The package name in an NPK's header (see [`package_header`]).
pub fn package_name(data: &[u8]) -> Result<String, NpkError> {
    package_header(data).map(|h| h.name)
}

/// The parts of an NPK (`type`, `data`), in order, until the end of the
/// file or the first part that runs past it. Hostile input ends the list,
/// never panics.
fn parts(data: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut off = if data.len() >= 8 && data.starts_with(&NPK_MAGIC) {
        8usize
    } else {
        data.len()
    };
    std::iter::from_fn(move || {
        if data.len().saturating_sub(off) < 6 {
            return None;
        }
        let kind = u16::from_le_bytes([data[off], data[off + 1]]);
        let len = u32::from_le_bytes([data[off + 2], data[off + 3], data[off + 4], data[off + 5]]);
        let start = off + 6;
        let end = usize::try_from(len)
            .ok()
            .and_then(|l| start.checked_add(l))
            .filter(|&e| e <= data.len())?;
        off = end;
        Some((kind, &data[start..end]))
    })
}

/// Part type of a v6 bundle's own header (`routeros-<arch>` and version).
pub const PART_BUNDLE: u16 = 18;

/// The version in an NPK's header (the first type-1 or type-18 part: 16
/// bytes of name, then `patch, kind, minor, major`). Kinds: `f` final
/// (`7.25.1`, `7.25` when the patch is 0, `6.49.23`), `b` beta and `c` rc
/// (the patch byte is their number: `7.26beta5`, `7.25rc3`; verified on
/// 2026-11-10 packages). Other kinds, or a version this app does not
/// accept, are `None`.
pub fn package_version(data: &[u8]) -> Option<crate::Version> {
    let body = parts(data)
        .find(|(kind, _)| *kind == PART_HEADER || *kind == PART_BUNDLE)
        .map(|(_, body)| body)?;
    let [patch, kind, minor, major] = <[u8; 4]>::try_from(body.get(16..20)?).ok()?;
    let text = match kind {
        b'f' if patch == 0 => format!("{major}.{minor}"),
        b'f' => format!("{major}.{minor}.{patch}"),
        // Pre-releases: the "patch" byte is the beta/rc number.
        b'b' => format!("{major}.{minor}beta{patch}"),
        b'c' => format!("{major}.{minor}rc{patch}"),
        _ => return None,
    };
    crate::Version::parse(&text).ok()
}

/// The names of the packages an NPK holds: its type-1 headers. A plain
/// package holds itself; a RouterOS v6 main package is a bundle of
/// `system`, `ipv6`, `wireless`, ... A name is read up to the first byte
/// that cannot be in a package name (real bundles have `wireless@`);
/// unreadable names are skipped.
pub fn contained_packages(data: &[u8]) -> Vec<String> {
    parts(data)
        .filter(|(kind, _)| *kind == PART_HEADER)
        .filter_map(|(_, body)| {
            let raw = &body[..body.len().min(16)];
            let end = raw
                .iter()
                .position(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-'))
                .unwrap_or(raw.len());
            let name = std::str::from_utf8(&raw[..end]).ok()?;
            valid_package_name(name).then(|| name.to_string())
        })
        .collect()
}

/// Printable ASCII with surrounding NULs and whitespace removed; `None` if
/// empty or anything else is in it.
fn text(raw: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(raw).ok()?;
    let s = s.trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace());
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_graphic())).then(|| s.to_string())
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
    use crate::testutil::{npk, npk_package};
    use proptest::prelude::*;

    fn part(v: &mut Vec<u8>, kind: u16, data: &[u8]) {
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
    }

    /// Magic, length and the given parts.
    fn with_parts(parts: &[(u16, &[u8])]) -> Vec<u8> {
        let mut v = NPK_MAGIC.to_vec();
        v.extend_from_slice(&[0; 4]);
        for (kind, data) in parts {
            part(&mut v, *kind, data);
        }
        let size = (v.len() - 8) as u32;
        v[4..8].copy_from_slice(&size.to_le_bytes());
        v
    }

    #[test]
    fn reads_name_and_arch_of_a_synthetic_package() {
        let data = npk_package("system", "mips", 4000, 0xaa);
        assert_eq!(data.len(), 4000);
        assert_eq!(check_npk("a.npk", &data), Ok(()));
        assert_eq!(package_arch(&data).as_deref(), Ok("mips"));
        assert_eq!(package_name(&data).as_deref(), Ok("system"));
        assert_eq!(
            package_header(&data),
            Ok(NpkHeader {
                name: "system".into(),
                arch: "mips".into()
            })
        );
        let wl = npk_package("wireless", "arm64", 200, 0);
        assert_eq!(package_arch(&wl).as_deref(), Ok("arm64"));
        assert_eq!(package_name(&wl).as_deref(), Ok("wireless"));
    }

    /// Only the first type-16 part after the type-1 header is the
    /// architecture: real packages end with a 1-byte type-16 part (`I`).
    #[test]
    fn the_arch_is_the_first_type_16_part_after_the_header() {
        let mut name = b"system".to_vec();
        name.resize(36, 0);
        let data = with_parts(&[
            (16, b"I"),
            (25, b""),
            (1, &name),
            (2, b"Main package"),
            (16, b"mmips"),
            (16, b"I"),
        ]);
        assert_eq!(package_arch(&data).as_deref(), Ok("mmips"));
        // Surrounding NULs and whitespace are not part of it.
        let data = with_parts(&[(1, &name), (16, b" smips\0\0")]);
        assert_eq!(package_arch(&data).as_deref(), Ok("smips"));
    }

    #[test]
    fn malformed_packages_are_errors() {
        let mut name = b"system".to_vec();
        name.resize(36, 0);
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"PK\x03\x04\0\0\0\0".to_vec(),
            npk(100, 0),                          // no parts
            with_parts(&[(16, b"arm")]),          // arch without a header
            with_parts(&[(1, &name)]),            // header without an arch
            with_parts(&[(1, &name), (16, b"")]), // empty arch
            with_parts(&[(1, &name), (16, b"a\xffb")]),
            with_parts(&[(1, b"\0\0\0"), (16, b"arm")]), // empty name
        ];
        for data in cases {
            assert!(package_arch(&data).is_err(), "{data:?}");
        }
        // A part running past the end of the file.
        let mut cut = with_parts(&[(1, &name), (16, b"arm")]);
        cut.truncate(cut.len() - 1);
        assert!(package_arch(&cut).is_err());
        assert!(package_name(&npk(8, 0)).is_err());
    }

    /// The packages in the user's cache, if there are any (read-only).
    /// Official 7.25.1 packages: `routeros-*-mipsbe.npk` says `mips`, the
    /// arch a CRS125 sends in its hello.
    #[test]
    #[ignore = "reads official packages from the local cache, if present"]
    fn real_cached_packages_carry_the_hello_arch() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let dir = std::path::Path::new(&home)
            .join("Library/Caches/io.github.sepsig.mac-netinstall/7.25.1");
        let expect = [
            ("routeros-7.25.1-mmips.npk", "system", "mmips"),
            ("routeros-7.25.1-arm.npk", "system", "arm"),
            ("routeros-7.25.1-mipsbe.npk", "system", "mips"),
            ("wireless-7.25.1-arm.npk", "wireless", "arm"),
            ("wireless-7.25.1-mipsbe.npk", "wireless", "mips"),
        ];
        for (file, name, arch) in expect {
            let Ok(data) = std::fs::read(dir.join(file)) else {
                eprintln!("skipped {file}: not cached");
                continue;
            };
            let header = package_header(&data).unwrap();
            assert_eq!((header.name.as_str(), header.arch.as_str()), (name, arch));
        }
    }

    proptest! {
        #[test]
        fn header_parser_never_panics(
            data in proptest::collection::vec(any::<u8>(), 0..256),
            idx in 0usize..300,
            byte in any::<u8>(),
            cut in 0usize..300,
        ) {
            let _ = package_header(&data);
            let mut v = npk_package("system", "arm", 120, 1);
            let i = idx % v.len();
            v[i] = byte;
            let _ = package_header(&v);
            v.truncate(cut.min(v.len()));
            let _ = package_header(&v);
        }
    }

    #[test]
    fn reads_the_version_of_final_releases() {
        let v7 = crate::testutil::npk_package_version("system", "arm", (7, 25, 1), 400, 0);
        assert_eq!(
            package_version(&v7).map(|v| v.to_string()),
            Some("7.25.1".into())
        );
        let v725 = crate::testutil::npk_package_version("system", "arm", (7, 25, 0), 400, 0);
        assert_eq!(
            package_version(&v725).map(|v| v.to_string()),
            Some("7.25".into())
        );
        let v6 = crate::testutil::npk_package_version("calea", "arm", (6, 49, 23), 400, 0);
        assert_eq!(
            package_version(&v6).map(|v| v.to_string()),
            Some("6.49.23".into())
        );
        // Real header bytes (routeros-mipsbe-6.49.23.npk's system part).
        let mut name = b"system".to_vec();
        name.resize(16, 0);
        name.extend_from_slice(&[0x17, 0x66, 0x31, 0x06, 0xbc, 0x6d]);
        name.resize(36, 0);
        let real = with_parts(&[(1, &name), (16, b"mips")]);
        assert_eq!(
            package_version(&real).map(|v| v.to_string()),
            Some("6.49.23".into())
        );
        // Betas and rcs: real header bytes (fetched 2026-11-10).
        for (bytes, want) in [
            ([0x05, 0x62, 0x1a, 0x07], "7.26beta5"),
            ([0x03, 0x63, 0x19, 0x07], "7.25rc3"),
            ([0x01, 0x63, 0x18, 0x07], "7.24rc1"),
            ([0x02, 0x63, 0x31, 0x06], "6.49rc2"),
        ] {
            let mut name = b"system".to_vec();
            name.resize(16, 0);
            name.extend_from_slice(&bytes);
            name.resize(36, 0);
            let data = with_parts(&[(1, &name), (16, b"arm")]);
            assert_eq!(
                package_version(&data).map(|v| v.to_string()).as_deref(),
                Some(want)
            );
        }
        // Unknown kinds and odd bytes are not decoded.
        let synthetic = npk_package("system", "arm", 300, 0);
        assert_eq!(package_version(&synthetic), None);
        let old = crate::testutil::npk_package_version("system", "arm", (5, 26, 0), 400, 0);
        assert_eq!(package_version(&old), None, "below the floor");
        assert_eq!(package_version(b"junk"), None);
        assert_eq!(package_version(&with_parts(&[(1, b"short")])), None);
    }

    #[test]
    fn lists_the_packages_of_a_v6_bundle() {
        let bundle = crate::testutil::npk_bundle(
            "smips",
            "smips",
            (6, 49, 23),
            &["system", "ipv6", "wireless@", "hotspot", "advanced-tools"],
            3000,
        );
        assert_eq!(check_npk("b.npk", &bundle), Ok(()));
        assert_eq!(
            package_header(&bundle),
            Ok(NpkHeader {
                name: "system".into(),
                arch: "smips".into()
            })
        );
        assert_eq!(
            contained_packages(&bundle),
            ["system", "ipv6", "wireless", "hotspot", "advanced-tools"]
        );
        assert_eq!(
            package_version(&bundle).map(|v| v.to_string()),
            Some("6.49.23".into())
        );
        // A plain package contains itself.
        let wl = npk_package("wireless", "arm", 300, 0);
        assert_eq!(contained_packages(&wl), ["wireless"]);
        assert!(contained_packages(b"junk").is_empty());
    }

    proptest! {
        #[test]
        fn bundle_readers_never_panic(
            data in proptest::collection::vec(any::<u8>(), 0..256),
            idx in 0usize..600,
            byte in any::<u8>(),
        ) {
            let _ = contained_packages(&data);
            let _ = package_version(&data);
            let mut v = crate::testutil::npk_bundle("arm", "arm", (6, 49, 23), &["system", "ppp"], 500);
            let i = idx % v.len();
            v[i] = byte;
            let _ = contained_packages(&v);
            let _ = package_version(&v);
        }
    }

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
