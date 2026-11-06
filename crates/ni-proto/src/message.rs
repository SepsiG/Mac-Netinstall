//! Payload constants and helpers.
//!
//! Server → device payloads and the device's replies, as the official
//! `netinstall-cli` 7.25.1 sends and expects them (captured in a VM; see
//! `docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md`).
//! The server's text payloads (`OFFR`, `FILE` headers, `TERM`) end with a
//! NUL byte that is counted in the frame's length field; the device's
//! replies are bare words.

/// Offer to install (server → device), without the fields; see [`offer`].
/// Kept for pyNetinstall-era tests and the simulator's description of
/// frames.
pub const OFFR: &[u8] = b"OFFR\n\n";
pub const YACK: &[u8] = b"YACK\n";
/// Negative acknowledgement prefix (device → server). The official server
/// ignores a `NACK` to its offer and offers again; see the session.
pub const NACK: &[u8] = b"NACK";
/// Device finished formatting.
pub const STRT: &[u8] = b"STRT";
/// Device ready for the next frame.
pub const RETR: &[u8] = b"RETR";
/// Device waiting for terminate.
pub const WTRM: &[u8] = b"WTRM";
/// pyNetinstall's "no more files" marker (server → device). The official
/// server never sends it; the session only falls back to it for devices
/// that never answer a poll with [`WTRM`].
pub const FILE_END: &[u8] = b"FILE\n";
/// Reboot command without a message (server → device). No reply.
pub const TERM: &[u8] = b"TERM\n\0";
/// Magic bytes every RouterOS `.npk` package starts with (pyNetinstall `verify_npk`).
pub const NPK_MAGIC: [u8; 4] = [0x1E, 0xF1, 0xD0, 0xBA];
/// File name RouterOS expects for the configuration script.
pub const CONFIG_SCRIPT_NAME: &str = "autorun.scr";
/// File name of the official tool's "mode script" (`-sm`).
pub const MODE_SCRIPT_NAME: &str = "mode.scr";
/// Longest license key put in an offer, in bytes.
pub const MAX_LICENSE_KEY: usize = 4096;

/// The fields of an `OFFR` payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Offer {
    /// Software license key (`-k <keyfile>`, line breaks removed), or empty.
    pub license_key: String,
    /// Keep the existing configuration (the official tool's default; off
    /// with `-r`, `-e` or `-s`).
    pub keep_config: bool,
    /// The official tool's `-b`. On the wire the field reads
    /// `removeBranding` when this is **off** and is empty when it is on
    /// (polarity as captured; what the device does with it is unverified).
    pub remove_branding: bool,
}

/// `OFFR\n<license key>\n<keep|>\n<removeBranding|>\n\0`.
pub fn offer(o: &Offer) -> Vec<u8> {
    let keep = if o.keep_config { "keep" } else { "" };
    let brand = if o.remove_branding {
        ""
    } else {
        "removeBranding"
    };
    format!("OFFR\n{}\n{keep}\n{brand}\n\0", o.license_key).into_bytes()
}

/// Parse an [`offer`] payload (the trailing NUL is optional). `None` for
/// anything else, including pyNetinstall's field-less `OFFR\n\n`.
pub fn parse_offer(payload: &[u8]) -> Option<Offer> {
    let text = std::str::from_utf8(payload.strip_suffix(b"\0").unwrap_or(payload)).ok()?;
    let fields: Vec<&str> = text.split('\n').collect();
    let [tag, key, keep, brand, ""] = fields[..] else {
        return None;
    };
    if tag != "OFFR" || !matches!(keep, "keep" | "") || !matches!(brand, "removeBranding" | "") {
        return None;
    }
    Some(Offer {
        license_key: key.to_string(),
        keep_config: keep == "keep",
        remove_branding: brand.is_empty(),
    })
}

/// What the device does once the files are installed (the `TERM` frame).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AfterInstall {
    /// Reboot (`TERM\n\0`, the official tool's default).
    #[default]
    Reboot,
    /// Reboot with the message `Installation successful` (`--reboot`).
    RebootWithMessage,
    /// Shut down (`--shutdown`).
    Shutdown,
}

/// The `TERM` payload for `after`.
pub fn term(after: AfterInstall) -> &'static [u8] {
    match after {
        AfterInstall::Reboot => TERM,
        AfterInstall::RebootWithMessage => b"TERM\nInstallation successful\n\0",
        AfterInstall::Shutdown => b"TERM\nshutdown\n\0",
    }
}

/// Which [`term`] payload this is (`None` for anything else).
pub fn parse_term(payload: &[u8]) -> Option<AfterInstall> {
    [
        AfterInstall::Reboot,
        AfterInstall::RebootWithMessage,
        AfterInstall::Shutdown,
    ]
    .into_iter()
    .find(|a| term(*a) == payload)
}

/// `FILE\n<name>\n<size>\n\0` header announcing a file upload.
pub fn file_header(name: &str, size: u64) -> Vec<u8> {
    format!("FILE\n{name}\n{size}\n\0").into_bytes()
}

/// Parse a [`file_header`] payload (the trailing NUL is optional, as
/// pyNetinstall sends none). Returns `None` for [`FILE_END`] or garbage.
pub fn parse_file_header(payload: &[u8]) -> Option<(String, u64)> {
    let payload = payload.strip_suffix(b"\0").unwrap_or(payload);
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
        assert_eq!(h, b"FILE\nrouteros-7.16-arm64.npk\n12345678\n\0");
        assert_eq!(
            parse_file_header(&h),
            Some(("routeros-7.16-arm64.npk".to_string(), 12_345_678))
        );
        assert_eq!(
            parse_file_header(b"FILE\nx.npk\n3\n"),
            Some(("x.npk".to_string(), 3)),
            "pyNetinstall's header has no NUL"
        );
    }

    #[test]
    fn file_end_is_not_a_header() {
        assert_eq!(parse_file_header(FILE_END), None);
        assert_eq!(parse_file_header(b"RETR"), None);
        assert_eq!(parse_file_header(b"FILE\nx\nnotanumber\n"), None);
    }

    /// The captured payloads (research doc §0).
    #[test]
    fn offer_matches_the_official_captures() {
        let hex = |o: &Offer| {
            offer(o)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let keep = Offer {
            keep_config: true,
            ..Offer::default()
        };
        assert_eq!(
            hex(&keep),
            "4f4646520a0a6b6565700a72656d6f76654272616e64696e670a00"
        );
        assert_eq!(
            hex(&Offer::default()),
            "4f4646520a0a0a72656d6f76654272616e64696e670a00"
        );
        let b = Offer {
            keep_config: true,
            remove_branding: true,
            ..Offer::default()
        };
        assert_eq!(hex(&b), "4f4646520a0a6b6565700a0a00");
        let rb = Offer {
            remove_branding: true,
            ..Offer::default()
        };
        assert_eq!(hex(&rb), "4f4646520a0a0a0a00");
        let k = Offer {
            license_key: "KEY".into(),
            keep_config: true,
            remove_branding: false,
        };
        assert_eq!(offer(&k), b"OFFR\nKEY\nkeep\nremoveBranding\n\0");
    }

    #[test]
    fn offer_roundtrip_and_rejects() {
        for keep_config in [false, true] {
            for remove_branding in [false, true] {
                let o = Offer {
                    license_key: "ABC".into(),
                    keep_config,
                    remove_branding,
                };
                assert_eq!(parse_offer(&offer(&o)), Some(o));
            }
        }
        assert_eq!(parse_offer(OFFR), None);
        assert_eq!(parse_offer(b"OFFR\n\nmaybe\n\n\0"), None);
        assert_eq!(parse_offer(b"OFFR\n\n\n\nextra\n\0"), None);
        assert_eq!(parse_offer(b"YACK\n"), None);
    }

    #[test]
    fn term_payloads() {
        assert_eq!(term(AfterInstall::Reboot), b"TERM\n\0");
        assert_eq!(
            term(AfterInstall::RebootWithMessage),
            b"TERM\nInstallation successful\n\0"
        );
        assert_eq!(term(AfterInstall::Shutdown), b"TERM\nshutdown\n\0");
        for a in [
            AfterInstall::Reboot,
            AfterInstall::RebootWithMessage,
            AfterInstall::Shutdown,
        ] {
            assert_eq!(parse_term(term(a)), Some(a));
        }
        assert_eq!(
            parse_term(b"TERM\n"),
            None,
            "pyNetinstall's TERM has no NUL"
        );
    }
}
