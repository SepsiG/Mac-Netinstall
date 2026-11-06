//! Sans-IO handling of the first datagram on the listening port:
//! request validation, file lookup and option negotiation.

use crate::packet::{ErrorCode, Packet, Request, DEFAULT_BLKSIZE};
use crate::transfer::Transfer;
use bytes::Bytes;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Read-only files served, keyed by name (no leading `/`).
pub type FileMap = HashMap<String, Bytes>;

/// Server-side transfer settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferConfig {
    /// Wait this long for an ACK before retransmitting.
    pub timeout: Duration,
    /// Retransmits of one packet before the transfer fails.
    pub max_retransmits: u32,
    /// Honor the client's `blksize` option (RFC 2348). Off by default:
    /// RouterBOOT is known to misbehave with blksize negotiation (the
    /// pyNetinstall docs configure dnsmasq with `tftp-no-blocksize`).
    pub honor_blksize: bool,
    /// Upper bound for a negotiated `blksize` (1468 fits a 1500 MTU).
    pub max_blksize: usize,
    /// Honor the client's `timeout` option (RFC 2349).
    pub honor_timeout: bool,
}

impl Default for TransferConfig {
    fn default() -> Self {
        TransferConfig {
            timeout: Duration::from_secs(1),
            max_retransmits: 5,
            honor_blksize: false,
            max_blksize: 1468,
            honor_timeout: true,
        }
    }
}

/// A request we answer with an ERROR packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub code: ErrorCode,
    pub message: &'static str,
    /// Requested file name, when the request was parseable.
    pub file: Option<String>,
}

impl Rejection {
    fn new(code: ErrorCode, message: &'static str, file: Option<&str>) -> Self {
        Rejection {
            code,
            message,
            file: file.map(str::to_string),
        }
    }

    /// The ERROR datagram to send back.
    pub fn packet(&self) -> Vec<u8> {
        Packet::Error {
            code: self.code,
            message: self.message.to_string(),
        }
        .encode()
    }
}

/// What to do with a datagram received on the listening port.
#[derive(Debug)]
pub enum Accept {
    /// Start this transfer (on a new socket).
    Start(Transfer),
    /// Answer with [`Rejection::packet`].
    Reject(Rejection),
    /// Do not answer (e.g. a stray ERROR packet).
    Ignore,
}

/// Decide how to answer `datagram`, received on the listening port.
pub fn accept(datagram: &[u8], files: &FileMap, cfg: &TransferConfig, now: Instant) -> Accept {
    let req = match Packet::parse(datagram) {
        Ok(Packet::Rrq(req)) => req,
        Ok(Packet::Wrq(req)) => {
            return Accept::Reject(Rejection::new(
                ErrorCode::IllegalOperation,
                "illegal operation",
                Some(&req.filename),
            ))
        }
        Ok(Packet::Error { .. }) => return Accept::Ignore,
        Ok(_) | Err(_) => {
            return Accept::Reject(Rejection::new(
                ErrorCode::IllegalOperation,
                "illegal operation",
                None,
            ))
        }
    };
    match start(&req, files, cfg, now) {
        Ok(t) => Accept::Start(t),
        Err(r) => Accept::Reject(r),
    }
}

fn start(
    req: &Request,
    files: &FileMap,
    cfg: &TransferConfig,
    now: Instant,
) -> Result<Transfer, Rejection> {
    let name = req.filename.strip_prefix('/').unwrap_or(&req.filename);
    let reject = |code, message| Rejection::new(code, message, Some(&req.filename));
    if req.mode != "octet" && req.mode != "netascii" {
        return Err(reject(ErrorCode::IllegalOperation, "unsupported mode"));
    }
    if name.contains('/') || name.contains("..") {
        return Err(reject(ErrorCode::AccessViolation, "access violation"));
    }
    let Some(data) = files.get(name) else {
        return Err(reject(ErrorCode::FileNotFound, "file not found"));
    };
    let mut blksize = DEFAULT_BLKSIZE;
    let mut timeout = cfg.timeout;
    let mut acked = Vec::new();
    for (opt, value) in &req.options {
        match opt.as_str() {
            "tsize" if value.parse::<u64>().is_ok() => {
                acked.push((opt.clone(), data.len().to_string()));
            }
            "blksize" if cfg.honor_blksize => {
                if let Ok(v @ 8..=65464) = value.parse::<usize>() {
                    blksize = v.min(cfg.max_blksize);
                    acked.push((opt.clone(), blksize.to_string()));
                }
            }
            "timeout" if cfg.honor_timeout => {
                if let Ok(v @ 1..=255) = value.parse::<u64>() {
                    timeout = Duration::from_secs(v);
                    acked.push((opt.clone(), v.to_string()));
                }
            }
            _ => {}
        }
    }
    let oack = (!acked.is_empty()).then_some(acked);
    Ok(Transfer::new(
        name.to_string(),
        data.clone(),
        blksize,
        timeout,
        cfg.max_retransmits,
        oack,
        now,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> FileMap {
        FileMap::from([
            ("boot.elf".to_string(), Bytes::from(vec![7u8; 1500])),
            ("empty".to_string(), Bytes::new()),
        ])
    }

    fn rrq(name: &str, mode: &str, opts: &[(&str, &str)]) -> Vec<u8> {
        Packet::Rrq(Request {
            filename: name.into(),
            mode: mode.into(),
            options: opts
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        })
        .encode()
    }

    fn started(a: Accept) -> Transfer {
        match a {
            Accept::Start(t) => t,
            other => panic!("expected start, got {other:?}"),
        }
    }

    fn rejected(a: Accept) -> Rejection {
        match a {
            Accept::Reject(r) => r,
            other => panic!("expected reject, got {other:?}"),
        }
    }

    fn go(buf: &[u8], cfg: &TransferConfig) -> Accept {
        accept(buf, &files(), cfg, Instant::now())
    }

    #[test]
    fn plain_rrq_starts_with_data_1() {
        let mut t = started(go(&rrq("boot.elf", "octet", &[]), &Default::default()));
        assert_eq!(t.file(), "boot.elf");
        assert_eq!(t.size(), 1500);
        assert_eq!(t.blksize(), 512);
        let first = t.poll_transmit(Instant::now()).unwrap();
        assert_eq!(&first[..4], b"\x00\x03\x00\x01");
    }

    #[test]
    fn leading_slash_and_netascii_are_accepted() {
        let t = started(go(&rrq("/boot.elf", "netascii", &[]), &Default::default()));
        assert_eq!(t.file(), "boot.elf");
    }

    #[test]
    fn tsize_is_answered_with_actual_size() {
        let mut t = started(go(
            &rrq("boot.elf", "octet", &[("tsize", "0")]),
            &Default::default(),
        ));
        let first = t.poll_transmit(Instant::now()).unwrap();
        assert_eq!(first, b"\x00\x06tsize\x001500\x00");
    }

    #[test]
    fn blksize_is_ignored_by_default() {
        let mut t = started(go(
            &rrq("boot.elf", "octet", &[("blksize", "1468")]),
            &Default::default(),
        ));
        assert_eq!(t.blksize(), 512);
        let first = t.poll_transmit(Instant::now()).unwrap();
        assert_eq!(&first[..2], b"\x00\x03", "no OACK, straight to DATA");
    }

    #[test]
    fn blksize_is_negotiated_and_capped_when_enabled() {
        let cfg = TransferConfig {
            honor_blksize: true,
            ..Default::default()
        };
        let mut t = started(go(
            &rrq("boot.elf", "octet", &[("blksize", "9000"), ("tsize", "0")]),
            &cfg,
        ));
        assert_eq!(t.blksize(), 1468);
        let first = t.poll_transmit(Instant::now()).unwrap();
        assert_eq!(first, b"\x00\x06blksize\x001468\x00tsize\x001500\x00");
        let t = started(go(&rrq("boot.elf", "octet", &[("blksize", "4")]), &cfg));
        assert_eq!(t.blksize(), 512, "out-of-range blksize ignored");
    }

    #[test]
    fn timeout_option_is_echoed() {
        let mut t = started(go(
            &rrq("boot.elf", "octet", &[("timeout", "3"), ("bogus", "1")]),
            &Default::default(),
        ));
        let now = Instant::now();
        let first = t.poll_transmit(now).unwrap();
        assert_eq!(first, b"\x00\x06timeout\x003\x00");
        assert_eq!(t.poll_timeout(), Some(now + Duration::from_secs(3)));
        let cfg = TransferConfig {
            honor_timeout: false,
            ..Default::default()
        };
        let mut t = started(go(&rrq("boot.elf", "octet", &[("timeout", "3")]), &cfg));
        let first = t.poll_transmit(Instant::now()).unwrap();
        assert_eq!(&first[..2], b"\x00\x03");
    }

    #[test]
    fn invalid_tsize_value_is_ignored() {
        let mut t = started(go(
            &rrq("boot.elf", "octet", &[("tsize", "abc")]),
            &Default::default(),
        ));
        let first = t.poll_transmit(Instant::now()).unwrap();
        assert_eq!(&first[..2], b"\x00\x03");
    }

    #[test]
    fn missing_file_is_error_1() {
        let r = rejected(go(&rrq("nope", "octet", &[]), &Default::default()));
        assert_eq!(r.code, ErrorCode::FileNotFound);
        assert_eq!(r.file.as_deref(), Some("nope"));
        assert_eq!(r.packet(), b"\x00\x05\x00\x01file not found\x00");
    }

    #[test]
    fn traversal_is_error_2() {
        for name in ["../boot.elf", "a/boot.elf", "//boot.elf", "..", "x..y"] {
            let r = rejected(go(&rrq(name, "octet", &[]), &Default::default()));
            assert_eq!(r.code, ErrorCode::AccessViolation, "{name}");
        }
    }

    #[test]
    fn wrq_and_bad_mode_are_error_4() {
        let wrq = Packet::Wrq(Request {
            filename: "boot.elf".into(),
            mode: "octet".into(),
            options: vec![],
        })
        .encode();
        let r = rejected(go(&wrq, &Default::default()));
        assert_eq!(r.code, ErrorCode::IllegalOperation);
        assert_eq!(r.packet(), b"\x00\x05\x00\x04illegal operation\x00");
        let r = rejected(go(&rrq("boot.elf", "mail", &[]), &Default::default()));
        assert_eq!(r.code, ErrorCode::IllegalOperation);
    }

    #[test]
    fn garbage_and_non_requests_are_error_4_but_errors_are_ignored() {
        let r = rejected(go(b"\x00", &Default::default()));
        assert_eq!((r.code, r.file), (ErrorCode::IllegalOperation, None));
        let r = rejected(go(&Packet::Ack(1).encode(), &Default::default()));
        assert_eq!(r.code, ErrorCode::IllegalOperation);
        assert!(matches!(
            go(b"\x00\x05\x00\x00x\x00", &Default::default()),
            Accept::Ignore
        ));
    }

    #[test]
    fn empty_file_is_served() {
        let t = started(go(&rrq("empty", "octet", &[]), &Default::default()));
        assert_eq!(t.size(), 0);
    }
}
