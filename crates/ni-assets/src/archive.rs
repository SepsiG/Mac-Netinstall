//! Archive readers: `netinstall-cli` from the netinstall `.tar.gz`, and
//! entry names from a ZIP central directory.

use flate2::read::GzDecoder;
use std::io::{self, Read};
use thiserror::Error;

/// Name of the Linux Netinstall binary inside `netinstall-<ver>.tar.gz`.
pub const NETINSTALL_CLI: &str = "netinstall-cli";
/// Upper bound for the extracted `netinstall-cli` (59 MB in 7.25.1).
pub const MAX_CLI_SIZE: u64 = 256 << 20;

const EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const CDH_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
const EOCD_LEN: usize = 22;

/// Problems reading an archive.
#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("cannot read the netinstall archive")]
    Read(#[source] io::Error),
    #[error("the netinstall archive has no {NETINSTALL_CLI}")]
    MissingCli,
    #[error("{NETINSTALL_CLI} is larger than {MAX_CLI_SIZE} bytes")]
    CliTooLarge,
    #[error("malformed package archive: {0}")]
    BadZip(&'static str),
    /// The central directory starts before the fetched tail; fetch at
    /// least `needed` bytes from the end and retry.
    #[error("package archive directory needs {needed} bytes from the end")]
    NeedMore { needed: u64 },
}

/// Read `netinstall-cli` (any directory) out of a gzip-compressed tarball.
pub fn netinstall_cli_from_tar_gz<R: Read>(reader: R) -> Result<Vec<u8>, ArchiveError> {
    cli_from_tar_gz(reader, MAX_CLI_SIZE)
}

/// [`netinstall_cli_from_tar_gz`] with a size limit: the header size is
/// checked first, and the read itself stops after `max + 1` bytes.
fn cli_from_tar_gz<R: Read>(reader: R, max: u64) -> Result<Vec<u8>, ArchiveError> {
    let mut archive = tar::Archive::new(GzDecoder::new(reader));
    for entry in archive.entries().map_err(ArchiveError::Read)? {
        let entry = entry.map_err(ArchiveError::Read)?;
        let is_cli = entry.header().entry_type().is_file()
            && entry
                .path()
                .map_err(ArchiveError::Read)?
                .file_name()
                .is_some_and(|n| n == NETINSTALL_CLI);
        if !is_cli {
            continue;
        }
        if entry.size() > max {
            return Err(ArchiveError::CliTooLarge);
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry
            .take(max + 1)
            .read_to_end(&mut data)
            .map_err(ArchiveError::Read)?;
        if data.len() as u64 > max {
            return Err(ArchiveError::CliTooLarge);
        }
        return Ok(data);
    }
    Err(ArchiveError::MissingCli)
}

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn u32_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u32::from_le_bytes(
        b.get(at..at + 4)?.try_into().ok()?,
    )))
}

/// Entry names from the central directory of a ZIP file, given its last
/// `tail.len()` bytes and its total size. ZIP64 archives are rejected.
pub fn zip_entry_names(tail: &[u8], total_len: u64) -> Result<Vec<String>, ArchiveError> {
    let bad = ArchiveError::BadZip;
    if tail.len() < EOCD_LEN || tail.len() as u64 > total_len {
        return Err(bad("too short"));
    }
    // The end-of-central-directory record is followed only by its comment.
    let eocd = (0..=tail.len() - EOCD_LEN)
        .rev()
        .find(|&i| {
            tail[i..].starts_with(&EOCD_SIG)
                && u16_at(tail, i + 20).is_some_and(|c| i + EOCD_LEN + c == tail.len())
        })
        .ok_or(bad("no end of central directory"))?;
    let count = u16_at(tail, eocd + 10).ok_or(bad("eocd"))?;
    let cd_size = u32_at(tail, eocd + 12).ok_or(bad("eocd"))?;
    let cd_offset = u32_at(tail, eocd + 16).ok_or(bad("eocd"))?;
    if count == 0xffff || cd_size == 0xffff_ffff || cd_offset == 0xffff_ffff {
        return Err(bad("zip64 is not supported"));
    }
    let tail_start = total_len - tail.len() as u64;
    let eocd_abs = tail_start + eocd as u64;
    if cd_offset + cd_size > eocd_abs {
        return Err(bad("central directory overlaps its end record"));
    }
    if cd_offset < tail_start {
        return Err(ArchiveError::NeedMore {
            needed: total_len - cd_offset,
        });
    }
    let mut p = (cd_offset - tail_start) as usize;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        if !tail.get(p..).is_some_and(|t| t.starts_with(&CDH_SIG)) {
            return Err(bad("bad central directory entry"));
        }
        let name_len = u16_at(tail, p + 28).ok_or(bad("entry"))?;
        let extra_len = u16_at(tail, p + 30).ok_or(bad("entry"))?;
        let comment_len = u16_at(tail, p + 32).ok_or(bad("entry"))?;
        let name = tail
            .get(p + 46..p + 46 + name_len)
            .ok_or(bad("entry name"))?;
        names.push(String::from_utf8_lossy(name).into_owned());
        p += 46 + name_len + extra_len + comment_len;
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{tar_gz, zip};
    use proptest::prelude::*;

    #[test]
    fn finds_netinstall_cli_in_a_tarball() {
        let tgz = tar_gz(&[("netinstall-cli", b"\x7fELF cli")]);
        assert_eq!(
            netinstall_cli_from_tar_gz(&tgz[..]).unwrap(),
            b"\x7fELF cli"
        );
        let nested = tar_gz(&[("netinstall-7.1/netinstall-cli", b"nested")]);
        assert_eq!(netinstall_cli_from_tar_gz(&nested[..]).unwrap(), b"nested");
    }

    #[test]
    fn an_oversized_cli_is_rejected() {
        let tgz = tar_gz(&[("netinstall-cli", &[1; 11])]);
        assert!(matches!(
            cli_from_tar_gz(&tgz[..], 10),
            Err(ArchiveError::CliTooLarge)
        ));
        assert_eq!(cli_from_tar_gz(&tgz[..], 11).unwrap(), [1; 11]);
    }

    #[test]
    fn zip64_markers_are_rejected() {
        for (field, len) in [(10, 2), (12, 4), (16, 4)] {
            let mut z = zip(&[("a.npk", b"x")]);
            let eocd = z.len() - 22;
            z[eocd + field..eocd + field + len].fill(0xff);
            let total = z.len() as u64;
            assert!(
                matches!(zip_entry_names(&z, total), Err(ArchiveError::BadZip(r)) if r.contains("zip64")),
                "field {field}"
            );
        }
    }

    #[test]
    fn tarball_without_cli_or_garbage_is_an_error() {
        let tgz = tar_gz(&[("netinstall-cli.txt", b"no")]);
        assert!(matches!(
            netinstall_cli_from_tar_gz(&tgz[..]),
            Err(ArchiveError::MissingCli)
        ));
        assert!(matches!(
            netinstall_cli_from_tar_gz(&b"not gzip at all"[..]),
            Err(ArchiveError::Read(_))
        ));
        assert!(matches!(
            netinstall_cli_from_tar_gz(&tgz[..tgz.len() / 2]),
            Err(ArchiveError::Read(_)) | Err(ArchiveError::MissingCli)
        ));
    }

    #[test]
    fn lists_zip_entries_from_the_whole_file_or_a_tail() {
        let z = zip(&[("a-7.1-arm.npk", &[1; 300]), ("b-7.1-arm.npk", &[2; 10])]);
        let total = z.len() as u64;
        let want = ["a-7.1-arm.npk", "b-7.1-arm.npk"];
        assert_eq!(zip_entry_names(&z, total).unwrap(), want);
        let tail = &z[z.len() - 150..];
        assert_eq!(zip_entry_names(tail, total).unwrap(), want);
    }

    #[test]
    fn asks_for_more_when_the_directory_starts_before_the_tail() {
        let z = zip(&[("a-7.1-arm.npk", &[1; 300]), ("b-7.1-arm.npk", &[2; 10])]);
        let total = z.len() as u64;
        let tail = &z[z.len() - 60..];
        let Err(ArchiveError::NeedMore { needed }) = zip_entry_names(tail, total) else {
            panic!("expected NeedMore");
        };
        let tail = &z[z.len() - needed as usize..];
        assert_eq!(zip_entry_names(tail, total).unwrap().len(), 2);
    }

    #[test]
    fn rejects_malformed_zips() {
        assert!(matches!(
            zip_entry_names(b"short", 5),
            Err(ArchiveError::BadZip(_))
        ));
        assert!(matches!(
            zip_entry_names(&[0u8; 100], 100),
            Err(ArchiveError::BadZip(_))
        ));
        let mut z = zip(&[("a.npk", b"x")]);
        let cd = z.len() - 22 - (46 + 5);
        z[cd] = b'X'; // break the central directory signature
        let total = z.len() as u64;
        assert!(matches!(
            zip_entry_names(&z, total),
            Err(ArchiveError::BadZip(_))
        ));
    }

    proptest! {
        #[test]
        fn zip_reader_never_panics(idx in 0usize..400, byte in any::<u8>(), cut in 22usize..400) {
            let mut z = zip(&[("a-7.1-arm.npk", &[1; 100]), ("bb-7.1-arm.npk", &[2; 50])]);
            let i = idx % z.len();
            z[i] = byte;
            let total = z.len() as u64;
            let cut = cut.min(z.len());
            let _ = zip_entry_names(&z[z.len() - cut..], total);
        }
    }
}
