//! Payload constants and helpers.
//!
//! Server → device payloads and expected device replies, as used by pyNetinstall.

/// Offer to install (server → device). Device answers [`YACK`].
pub const OFFR: &[u8] = b"OFFR\n\n";
pub const YACK: &[u8] = b"YACK\n";
/// Negative acknowledgement prefix (device → server).
pub const NACK: &[u8] = b"NACK";
/// Device finished formatting.
pub const STRT: &[u8] = b"STRT";
/// Device ready for the next frame.
pub const RETR: &[u8] = b"RETR";
/// Device waiting for terminate.
pub const WTRM: &[u8] = b"WTRM";
/// "No more files" marker (server → device). Device answers [`WTRM`].
pub const FILE_END: &[u8] = b"FILE\n";
/// Reboot command (server → device). No reply.
pub const TERM: &[u8] = b"TERM\n";
/// File name RouterOS expects for the configuration script.
pub const CONFIG_SCRIPT_NAME: &str = "autorun.scr";

/// `FILE\n<name>\n<size>\n` header announcing a file upload.
pub fn file_header(name: &str, size: u64) -> Vec<u8> {
    format!("FILE\n{name}\n{size}\n").into_bytes()
}

/// Parse a [`file_header`] payload. Returns `None` for [`FILE_END`] or garbage.
pub fn parse_file_header(payload: &[u8]) -> Option<(String, u64)> {
    let text = std::str::from_utf8(payload).ok()?;
    let mut parts = text.split('\n');
    if parts.next()? != "FILE" {
        return None;
    }
    let name = parts.next()?;
    if name.is_empty() {
        return None;
    }
    let size = parts.next()?.parse().ok()?;
    Some((name.to_string(), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_header_roundtrip() {
        let h = file_header("routeros-7.16-arm64.npk", 12_345_678);
        assert_eq!(h, b"FILE\nrouteros-7.16-arm64.npk\n12345678\n");
        assert_eq!(
            parse_file_header(&h),
            Some(("routeros-7.16-arm64.npk".to_string(), 12_345_678))
        );
    }

    #[test]
    fn file_end_is_not_a_header() {
        assert_eq!(parse_file_header(FILE_END), None);
        assert_eq!(parse_file_header(b"RETR"), None);
        assert_eq!(parse_file_header(b"FILE\nx\nnotanumber\n"), None);
    }
}
