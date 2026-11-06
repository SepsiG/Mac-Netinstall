//! Sans-IO BOOTP/DHCP responder: request bytes in, reply bytes out.

use crate::arch::{BootArch, VendorClass};
use crate::packet::{
    build_reply, MessageType, PacketError, ReplyKind, ReplyParams, Request, MAX_FILE_LEN,
};
use crate::pool::{LeasePool, PoolError};
use ni_proto::Mac;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use thiserror::Error;

/// Responder settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootpConfig {
    /// Our IP on the selected interface: next-server and DHCP server id.
    pub server_ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    /// First address handed out (inclusive).
    pub pool_start: Ipv4Addr,
    /// Last address handed out (inclusive).
    pub pool_end: Ipv4Addr,
    /// TFTP file name of the boot kernel per architecture.
    pub boot_files: HashMap<BootArch, String>,
    /// DHCP lease time (option 51).
    pub lease_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error(transparent)]
    Pool(#[from] PoolError),
    #[error("boot file name for {arch} is {len} bytes, max is {MAX_FILE_LEN}")]
    FileNameTooLong { arch: BootArch, len: usize },
    #[error("boot file name for {arch} is empty")]
    EmptyFileName { arch: BootArch },
}

/// A reply to send to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub bytes: Vec<u8>,
    pub kind: ReplyKind,
    pub client_mac: Mac,
    pub arch: BootArch,
    pub assigned_ip: Ipv4Addr,
}

/// Why a well-formed request got no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreReason {
    /// DHCP message we do not answer (RELEASE, INFORM, DECLINE, ...).
    MessageType(MessageType),
    /// DHCPREQUEST addressed to another server (option 54).
    OtherServer(Ipv4Addr),
    /// No option 60: not a RouterBOOT client.
    NoVendorClass,
    /// Option 60 present but not a MikroTik boot loader.
    UnknownVendorClass(String),
    /// A MikroTik boot loader we cannot serve yet (x86 PXE, ARM64 UEFI).
    UnsupportedDevice(String),
    /// No boot kernel configured for this architecture.
    NoBootFile(BootArch),
    /// All pool addresses are leased.
    PoolExhausted(BootArch),
}

/// Result of [`BootpResponder::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Reply(Reply),
    Ignored { mac: Mac, reason: IgnoreReason },
}

/// Answers RouterBOOT BOOTP/DHCP requests.
#[derive(Debug, Clone)]
pub struct BootpResponder {
    cfg: BootpConfig,
    pool: LeasePool,
}

impl BootpResponder {
    pub fn new(cfg: BootpConfig) -> Result<Self, ConfigError> {
        for (&arch, name) in &cfg.boot_files {
            if name.is_empty() {
                return Err(ConfigError::EmptyFileName { arch });
            }
            if name.len() > MAX_FILE_LEN {
                return Err(ConfigError::FileNameTooLong {
                    arch,
                    len: name.len(),
                });
            }
        }
        let pool = LeasePool::new(cfg.pool_start, cfg.pool_end, cfg.server_ip)?;
        Ok(BootpResponder { cfg, pool })
    }

    pub fn config(&self) -> &BootpConfig {
        &self.cfg
    }

    /// Address leased to `mac`, if any.
    pub fn lease_of(&self, mac: &Mac) -> Option<Ipv4Addr> {
        self.pool.get(mac)
    }

    /// Process one datagram received on port 67.
    pub fn handle(&mut self, datagram: &[u8]) -> Result<Outcome, PacketError> {
        let req = Request::parse(datagram)?;
        let mac = req.chaddr;
        let ignore = |reason| Ok(Outcome::Ignored { mac, reason });
        let kind = match req.message_type {
            Some(t) if req.is_dhcp() => match t {
                MessageType::Discover => ReplyKind::Offer,
                MessageType::Request => match req.server_id {
                    Some(id) if id != self.cfg.server_ip => {
                        return ignore(IgnoreReason::OtherServer(id))
                    }
                    _ => ReplyKind::Ack,
                },
                other => return ignore(IgnoreReason::MessageType(other)),
            },
            _ => ReplyKind::Bootp,
        };
        let arch = match req.vendor_class.as_deref().map(BootArch::from_vendor_class) {
            None => return ignore(IgnoreReason::NoVendorClass),
            Some(VendorClass::Supported(arch)) => arch,
            Some(VendorClass::Unsupported(class)) => {
                return ignore(IgnoreReason::UnsupportedDevice(class))
            }
            Some(VendorClass::Unknown(class)) => {
                return ignore(IgnoreReason::UnknownVendorClass(class))
            }
        };
        let Some(file) = self.cfg.boot_files.get(&arch) else {
            return ignore(IgnoreReason::NoBootFile(arch));
        };
        let Some(ip) = self.pool.lease(mac) else {
            return ignore(IgnoreReason::PoolExhausted(arch));
        };
        let bytes = build_reply(
            &req,
            &ReplyParams {
                kind,
                your_ip: ip,
                server_ip: self.cfg.server_ip,
                netmask: self.cfg.netmask,
                lease_secs: self.cfg.lease_secs,
                file,
            },
        );
        Ok(Outcome::Reply(Reply {
            bytes,
            kind,
            client_mac: mac,
            arch,
            assigned_ip: ip,
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{opt, sample_request, HEADER_LEN, MAGIC_COOKIE};

    const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 1]);
    const SERVER: Ipv4Addr = Ipv4Addr::new(192, 168, 88, 1);

    fn config() -> BootpConfig {
        BootpConfig {
            server_ip: SERVER,
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            pool_start: Ipv4Addr::new(192, 168, 88, 10),
            pool_end: Ipv4Addr::new(192, 168, 88, 11),
            boot_files: HashMap::from([
                (BootArch::MMips, "mmips.elf".to_string()),
                (BootArch::Arm64, "arm64.elf".to_string()),
            ]),
            lease_secs: 600,
        }
    }

    fn responder() -> BootpResponder {
        BootpResponder::new(config()).unwrap()
    }

    fn reply(outcome: Outcome) -> Reply {
        match outcome {
            Outcome::Reply(r) => r,
            other => panic!("expected reply, got {other:?}"),
        }
    }

    fn ignored(outcome: Outcome) -> IgnoreReason {
        match outcome {
            Outcome::Ignored { mac, reason } => {
                assert_eq!(mac, MAC);
                reason
            }
            other => panic!("expected ignore, got {other:?}"),
        }
    }

    #[test]
    fn discover_gets_offer_with_boot_file() {
        let mut r = responder();
        let rep = reply(
            r.handle(&sample_request(MAC, Some(1), Some("MMipsBoot")))
                .unwrap(),
        );
        assert_eq!(rep.kind, ReplyKind::Offer);
        assert_eq!(rep.arch, BootArch::MMips);
        assert_eq!(rep.client_mac, MAC);
        assert_eq!(rep.assigned_ip, Ipv4Addr::new(192, 168, 88, 10));
        let parsed = {
            let mut b = rep.bytes.clone();
            b[0] = 1;
            Request::parse(&b).unwrap()
        };
        assert_eq!(parsed.file, "mmips.elf");
        assert_eq!(parsed.message_type, Some(MessageType::Offer));
        assert_eq!(&rep.bytes[20..24], &SERVER.octets());
        assert_eq!(r.lease_of(&MAC), Some(rep.assigned_ip));
    }

    #[test]
    fn request_gets_ack_with_same_ip() {
        let mut r = responder();
        let offer = reply(
            r.handle(&sample_request(MAC, Some(1), Some("MMipsBoot")))
                .unwrap(),
        );
        let ack = reply(
            r.handle(&sample_request(MAC, Some(3), Some("MMipsBoot")))
                .unwrap(),
        );
        assert_eq!(ack.kind, ReplyKind::Ack);
        assert_eq!(ack.assigned_ip, offer.assigned_ip);
        assert_eq!(&ack.bytes[240..243], &[53, 1, 5]);
    }

    #[test]
    fn plain_bootp_gets_bootp_reply() {
        let mut r = responder();
        let rep = reply(
            r.handle(&sample_request(MAC, None, Some("ARM64__boot")))
                .unwrap(),
        );
        assert_eq!(rep.kind, ReplyKind::Bootp);
        assert_eq!(rep.arch, BootArch::Arm64);
        assert_eq!(&rep.bytes[240..242], &[opt::SUBNET_MASK, 4]);
    }

    #[test]
    fn request_for_other_server_is_ignored() {
        let mut r = responder();
        let mut buf = sample_request(MAC, Some(3), Some("MMipsBoot"));
        // Overwrite the END option (after cookie + 53 + 60) with option 54.
        let end = HEADER_LEN + 4 + 3 + 2 + "MMipsBoot".len();
        buf[end..end + 7].copy_from_slice(&[opt::SERVER_ID, 4, 10, 0, 0, 1, opt::END]);
        assert_eq!(
            ignored(r.handle(&buf).unwrap()),
            IgnoreReason::OtherServer(Ipv4Addr::new(10, 0, 0, 1))
        );
        assert_eq!(r.lease_of(&MAC), None, "no lease for ignored requests");
    }

    #[test]
    fn request_naming_us_is_acked() {
        let mut r = responder();
        let mut buf = sample_request(MAC, Some(3), Some("MMipsBoot"));
        let end = HEADER_LEN + 4 + 3 + 2 + "MMipsBoot".len();
        buf[end..end + 7].copy_from_slice(&[opt::SERVER_ID, 4, 192, 168, 88, 1, opt::END]);
        assert_eq!(reply(r.handle(&buf).unwrap()).kind, ReplyKind::Ack);
    }

    #[test]
    fn other_dhcp_messages_are_ignored() {
        let mut r = responder();
        let reason = ignored(
            r.handle(&sample_request(MAC, Some(7), Some("MMipsBoot")))
                .unwrap(),
        );
        assert_eq!(reason, IgnoreReason::MessageType(MessageType::Release));
    }

    #[test]
    fn missing_unknown_and_unsupported_vendor_classes_are_ignored() {
        let mut r = responder();
        assert_eq!(
            ignored(r.handle(&sample_request(MAC, Some(1), None)).unwrap()),
            IgnoreReason::NoVendorClass
        );
        assert_eq!(
            ignored(
                r.handle(&sample_request(MAC, Some(1), Some("MSFT 5.0")))
                    .unwrap()
            ),
            IgnoreReason::UnknownVendorClass("MSFT 5.0".into())
        );
        assert_eq!(
            ignored(
                r.handle(&sample_request(MAC, Some(1), Some("X86_64__boot")))
                    .unwrap()
            ),
            IgnoreReason::UnsupportedDevice("X86_64__boot".into())
        );
    }

    #[test]
    fn arch_without_boot_file_is_ignored() {
        let mut r = responder();
        assert_eq!(
            ignored(
                r.handle(&sample_request(MAC, Some(1), Some("Mips_boot")))
                    .unwrap()
            ),
            IgnoreReason::NoBootFile(BootArch::Mips)
        );
    }

    #[test]
    fn pool_exhaustion_is_reported() {
        let mut r = responder();
        for n in 2..=3 {
            let other = Mac([0x48, 0xa9, 0x8a, 0, 0, n]);
            reply(
                r.handle(&sample_request(other, Some(1), Some("MMipsBoot")))
                    .unwrap(),
            );
        }
        assert_eq!(
            ignored(
                r.handle(&sample_request(MAC, Some(1), Some("MMipsBoot")))
                    .unwrap()
            ),
            IgnoreReason::PoolExhausted(BootArch::MMips)
        );
    }

    #[test]
    fn malformed_packets_are_errors() {
        let mut r = responder();
        assert_eq!(r.handle(&[0u8; 10]), Err(PacketError::TooShort(10)));
        let mut buf = sample_request(MAC, Some(1), Some("MMipsBoot"));
        buf[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&MAGIC_COOKIE);
        buf.truncate(HEADER_LEN + 5);
        buf[HEADER_LEN + 4] = opt::VENDOR_CLASS;
        assert_eq!(
            r.handle(&buf),
            Err(PacketError::TruncatedOption { code: 60 })
        );
    }

    #[test]
    fn config_validation() {
        let mut cfg = config();
        cfg.boot_files.insert(BootArch::Arm, "a".repeat(128));
        assert_eq!(
            BootpResponder::new(cfg).unwrap_err(),
            ConfigError::FileNameTooLong {
                arch: BootArch::Arm,
                len: 128
            }
        );
        let mut cfg = config();
        cfg.boot_files.insert(BootArch::Arm, String::new());
        assert_eq!(
            BootpResponder::new(cfg).unwrap_err(),
            ConfigError::EmptyFileName {
                arch: BootArch::Arm
            }
        );
        let mut cfg = config();
        cfg.pool_start = Ipv4Addr::new(192, 168, 88, 200);
        assert!(matches!(
            BootpResponder::new(cfg).unwrap_err(),
            ConfigError::Pool(PoolError::Inverted { .. })
        ));
    }
}
