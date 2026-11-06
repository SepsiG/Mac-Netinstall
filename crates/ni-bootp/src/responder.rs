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
    #[error("server IP {0} is the subnet's network or broadcast address")]
    ServerIpNotHost(Ipv4Addr),
    #[error("pool {start}..={end} has no usable address")]
    EmptyPool { start: Ipv4Addr, end: Ipv4Addr },
    #[error("netmask {0} is not a contiguous prefix")]
    InvalidNetmask(Ipv4Addr),
    #[error("pool {start}..={end} is not inside the server subnet {network}/{netmask}")]
    PoolOutsideSubnet {
        start: Ipv4Addr,
        end: Ipv4Addr,
        network: Ipv4Addr,
        netmask: Ipv4Addr,
    },
}

/// A reply to send to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub bytes: Vec<u8>,
    pub kind: ReplyKind,
    pub client_mac: Mac,
    pub arch: BootArch,
    /// Address leased to the client. For [`ReplyKind::Nak`] this is the
    /// address we hold for it, which the NAK itself does not carry.
    pub assigned_ip: Ipv4Addr,
}

/// Why a well-formed request got no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreReason {
    /// DHCP message we do not answer (RELEASE, INFORM, ...).
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
    /// DHCPDECLINE without option 54: it cannot be attributed to us.
    NoServerId,
    /// DHCPDECLINE from a MAC that holds no lease from us.
    NoLease,
    /// Relayed request (`giaddr` or `hops` non-zero). Only directly
    /// attached clients are served: replies go out as link broadcasts on
    /// the boot interface, never back through a relay agent.
    Relayed,
}

/// Result of [`BootpResponder::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Reply(Reply),
    Ignored {
        mac: Mac,
        reason: IgnoreReason,
    },
    /// DHCPDECLINE: the client found its address in use. Its lease was
    /// dropped and `ip` (that lease) is quarantined for the responder's
    /// lifetime. No reply is sent.
    Declined {
        mac: Mac,
        ip: Ipv4Addr,
    },
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
        let mask = u32::from(cfg.netmask);
        if mask.leading_ones() + mask.trailing_zeros() != 32 {
            return Err(ConfigError::InvalidNetmask(cfg.netmask));
        }
        let network = u32::from(cfg.server_ip) & mask;
        let in_subnet = |ip: Ipv4Addr| u32::from(ip) & mask == network;
        if !in_subnet(cfg.pool_start) || !in_subnet(cfg.pool_end) {
            return Err(ConfigError::PoolOutsideSubnet {
                start: cfg.pool_start,
                end: cfg.pool_end,
                network: network.into(),
                netmask: cfg.netmask,
            });
        }
        let mut exclude = vec![cfg.server_ip];
        // /31 (RFC 3021) and /32 have no network or broadcast address.
        if mask.leading_ones() <= 30 {
            let (net, bcast) = (Ipv4Addr::from(network), Ipv4Addr::from(network | !mask));
            if cfg.server_ip == net || cfg.server_ip == bcast {
                return Err(ConfigError::ServerIpNotHost(cfg.server_ip));
            }
            exclude.extend([net, bcast]);
        }
        let pool = LeasePool::new(cfg.pool_start, cfg.pool_end, &exclude)?;
        if pool.usable() == 0 {
            return Err(ConfigError::EmptyPool {
                start: cfg.pool_start,
                end: cfg.pool_end,
            });
        }
        Ok(BootpResponder { cfg, pool })
    }

    pub fn config(&self) -> &BootpConfig {
        &self.cfg
    }

    /// Address leased to `mac`, if any.
    pub fn lease_of(&self, mac: &Mac) -> Option<Ipv4Addr> {
        self.pool.get(mac)
    }

    /// DHCPDECLINE (option 54 already checked to be us): drop
    /// `req.chaddr`'s lease and quarantine that address. Option 50 is not
    /// trusted beyond that: only the MAC's own lease is ever quarantined
    /// (if option 50 names another address, the lease is still dropped,
    /// since the pool never hands addresses back). A MAC without a lease
    /// quarantines nothing, so stray DECLINEs cannot drain the pool.
    fn decline(&mut self, req: &Request) -> Outcome {
        let mac = req.chaddr;
        match self.pool.decline(&mac) {
            Some(ip) => Outcome::Declined { mac, ip },
            None => Outcome::Ignored {
                mac,
                reason: IgnoreReason::NoLease,
            },
        }
    }

    /// Process one datagram received on port 67.
    pub fn handle(&mut self, datagram: &[u8]) -> Result<Outcome, PacketError> {
        let req = Request::parse(datagram)?;
        let mac = req.chaddr;
        let ignore = |reason| Ok(Outcome::Ignored { mac, reason });
        if req.giaddr != Ipv4Addr::UNSPECIFIED || req.hops != 0 {
            return ignore(IgnoreReason::Relayed);
        }
        let kind = match req.message_type {
            Some(t) if req.is_dhcp() => {
                if matches!(t, MessageType::Request | MessageType::Decline) {
                    if let Some(id) = req.server_id.filter(|id| *id != self.cfg.server_ip) {
                        return ignore(IgnoreReason::OtherServer(id));
                    }
                }
                match t {
                    MessageType::Discover => ReplyKind::Offer,
                    MessageType::Request => ReplyKind::Ack,
                    MessageType::Decline if req.server_id.is_none() => {
                        return ignore(IgnoreReason::NoServerId)
                    }
                    MessageType::Decline => return Ok(self.decline(&req)),
                    other => return ignore(IgnoreReason::MessageType(other)),
                }
            }
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
        // A DHCPREQUEST for an address other than the one we hold for this
        // MAC (e.g. remembered from another network) is refused, so the
        // client restarts with DISCOVER. No option 50 (RENEWING) is ACKed.
        // The NAK path has allocated (and keeps) a lease for this MAC on
        // purpose: it is harmless, since leases are per MAC and the very
        // same address is offered when the client DISCOVERs next.
        let kind = match (kind, req.requested_ip) {
            (ReplyKind::Ack, Some(requested)) if requested != ip => ReplyKind::Nak,
            (kind, _) => kind,
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
    use crate::packet::{opt, sample_request, sample_request_with, HEADER_LEN, MAGIC_COOKIE};

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

    fn ip(last: u8) -> Ipv4Addr {
        Ipv4Addr::new(192, 168, 88, last)
    }

    fn requesting(msg_type: u8, requested: Option<Ipv4Addr>) -> Vec<u8> {
        let extra = requested
            .map(|r| {
                let mut o = vec![opt::REQUESTED_IP, 4];
                o.extend_from_slice(&r.octets());
                o
            })
            .unwrap_or_default();
        sample_request_with(MAC, Some(msg_type), Some("MMipsBoot"), &extra)
    }

    #[test]
    fn request_for_our_lease_is_acked() {
        let mut r = responder();
        let offer = reply(r.handle(&requesting(1, None)).unwrap());
        let ack = reply(r.handle(&requesting(3, Some(offer.assigned_ip))).unwrap());
        assert_eq!(ack.kind, ReplyKind::Ack);
        assert_eq!(&ack.bytes[16..20], &offer.assigned_ip.octets());
    }

    #[test]
    fn request_for_another_address_is_naked() {
        let mut r = responder();
        let offer = reply(r.handle(&requesting(1, None)).unwrap());
        let nak = reply(r.handle(&requesting(3, Some(ip(77)))).unwrap());
        assert_eq!(nak.kind, ReplyKind::Nak);
        assert_eq!(nak.assigned_ip, offer.assigned_ip, "the lease we hold");
        assert_eq!(&nak.bytes[16..20], &[0, 0, 0, 0], "no yiaddr");
        assert!(nak.bytes[108..236].iter().all(|&b| b == 0), "no file");
        assert_eq!(&nak.bytes[240..249], &[53, 1, 6, 54, 4, 192, 168, 88, 1]);
        assert_eq!(r.lease_of(&MAC), Some(offer.assigned_ip), "lease kept");
    }

    #[test]
    fn init_reboot_request_without_prior_discover() {
        // No lease yet: the request is compared with what we would offer.
        let mut r = responder();
        assert_eq!(
            reply(r.handle(&requesting(3, Some(ip(50)))).unwrap()).kind,
            ReplyKind::Nak
        );
        let mut r = responder();
        assert_eq!(
            reply(r.handle(&requesting(3, Some(ip(10)))).unwrap()).kind,
            ReplyKind::Ack
        );
    }

    fn decline(mac: Mac, server_id: Option<Ipv4Addr>, requested: Option<Ipv4Addr>) -> Vec<u8> {
        let mut extra = Vec::new();
        if let Some(id) = server_id {
            extra.extend_from_slice(&[opt::SERVER_ID, 4]);
            extra.extend_from_slice(&id.octets());
        }
        if let Some(r) = requested {
            extra.extend_from_slice(&[opt::REQUESTED_IP, 4]);
            extra.extend_from_slice(&r.octets());
        }
        sample_request_with(mac, Some(4), Some("MMipsBoot"), &extra)
    }

    #[test]
    fn decline_releases_and_quarantines_the_address() {
        let mut r = responder();
        let offer = reply(r.handle(&requesting(1, None)).unwrap());
        assert_eq!(offer.assigned_ip, ip(10));
        assert_eq!(
            r.handle(&decline(MAC, Some(SERVER), Some(ip(10)))).unwrap(),
            Outcome::Declined {
                mac: MAC,
                ip: ip(10)
            }
        );
        assert_eq!(r.lease_of(&MAC), None);
        // Same MAC asks again: a fresh address, never .10.
        let again = reply(r.handle(&requesting(1, None)).unwrap());
        assert_eq!(again.assigned_ip, ip(11));
        let other = Mac([0x48, 0xa9, 0x8a, 0, 0, 9]);
        assert_eq!(
            r.handle(&sample_request(other, Some(1), Some("MMipsBoot")))
                .unwrap(),
            Outcome::Ignored {
                mac: other,
                reason: IgnoreReason::PoolExhausted(BootArch::MMips)
            },
            ".10 stays quarantined"
        );
    }

    #[test]
    fn decline_with_other_option_50_quarantines_only_the_lease() {
        let mut r = responder();
        reply(r.handle(&requesting(1, None)).unwrap());
        assert_eq!(
            r.handle(&decline(MAC, Some(SERVER), Some(ip(11)))).unwrap(),
            Outcome::Declined {
                mac: MAC,
                ip: ip(10)
            }
        );
        assert_eq!(
            reply(r.handle(&requesting(1, None)).unwrap()).assigned_ip,
            ip(11)
        );
    }

    #[test]
    fn decline_without_lease_or_server_id_quarantines_nothing() {
        let mut r = responder();
        assert_eq!(
            ignored(r.handle(&decline(MAC, Some(SERVER), Some(ip(10)))).unwrap()),
            IgnoreReason::NoLease
        );
        reply(r.handle(&requesting(1, None)).unwrap());
        assert_eq!(
            ignored(r.handle(&decline(MAC, None, Some(ip(10)))).unwrap()),
            IgnoreReason::NoServerId
        );
        assert_eq!(r.lease_of(&MAC), Some(ip(10)), "lease kept");
    }

    #[test]
    fn stray_declines_do_not_exhaust_the_pool() {
        let mut r = responder();
        for n in 0..=255u8 {
            let stranger = Mac([2, 0, 0, 0, 7, n]);
            let foreign = Ipv4Addr::new(10, 9, 8, n);
            for buf in [
                decline(stranger, None, Some(ip(10))),
                decline(stranger, None, Some(ip(11))),
                decline(stranger, Some(SERVER), Some(ip(10))),
                decline(stranger, Some(SERVER), Some(foreign)),
                decline(stranger, Some(Ipv4Addr::new(10, 0, 0, 1)), Some(ip(11))),
            ] {
                assert!(matches!(r.handle(&buf).unwrap(), Outcome::Ignored { .. }));
            }
        }
        assert_eq!(
            reply(r.handle(&requesting(1, None)).unwrap()).assigned_ip,
            ip(10)
        );
        let other = Mac([0x48, 0xa9, 0x8a, 0, 0, 9]);
        assert_eq!(
            reply(
                r.handle(&sample_request(other, Some(1), Some("MMipsBoot")))
                    .unwrap()
            )
            .assigned_ip,
            ip(11)
        );
    }

    #[test]
    fn decline_for_another_server_is_ignored() {
        let mut r = responder();
        reply(r.handle(&requesting(1, None)).unwrap());
        let buf = sample_request_with(
            MAC,
            Some(4),
            Some("MMipsBoot"),
            &[opt::SERVER_ID, 4, 10, 0, 0, 1],
        );
        assert_eq!(
            ignored(r.handle(&buf).unwrap()),
            IgnoreReason::OtherServer(Ipv4Addr::new(10, 0, 0, 1))
        );
        assert_eq!(r.lease_of(&MAC), Some(ip(10)));
    }

    #[test]
    fn relayed_requests_are_ignored() {
        let mut r = responder();
        let mut buf = sample_request(MAC, Some(1), Some("MMipsBoot"));
        buf[24..28].copy_from_slice(&[10, 1, 1, 1]);
        assert_eq!(ignored(r.handle(&buf).unwrap()), IgnoreReason::Relayed);
        let mut buf = sample_request(MAC, None, Some("MMipsBoot"));
        buf[3] = 1;
        assert_eq!(ignored(r.handle(&buf).unwrap()), IgnoreReason::Relayed);
        assert_eq!(r.lease_of(&MAC), None);
    }

    #[test]
    fn whole_subnet_pool_skips_network_broadcast_and_server() {
        let mut cfg = config();
        cfg.pool_start = ip(0);
        cfg.pool_end = ip(255);
        let mut r = BootpResponder::new(cfg).unwrap();
        let mut handed = Vec::new();
        for n in 0..=255u8 {
            let mac = Mac([2, 0, 0, 0, 1, n]);
            match r
                .handle(&sample_request(mac, Some(1), Some("MMipsBoot")))
                .unwrap()
            {
                Outcome::Reply(rep) => handed.push(rep.assigned_ip),
                Outcome::Ignored {
                    reason: IgnoreReason::PoolExhausted(_),
                    ..
                } => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(handed.first(), Some(&ip(2)));
        assert_eq!(handed.last(), Some(&ip(254)));
        assert_eq!(handed.len(), 253);
        for bad in [ip(0), ip(1), ip(255)] {
            assert!(!handed.contains(&bad), "{bad}");
        }
    }

    #[test]
    fn pool_outside_server_subnet_is_rejected() {
        for (start, end) in [
            (Ipv4Addr::new(10, 0, 0, 10), Ipv4Addr::new(10, 0, 0, 20)),
            (ip(10), Ipv4Addr::new(192, 168, 89, 5)),
            (Ipv4Addr::new(192, 168, 87, 250), ip(5)),
        ] {
            let mut cfg = config();
            cfg.pool_start = start;
            cfg.pool_end = end;
            assert_eq!(
                BootpResponder::new(cfg).unwrap_err(),
                ConfigError::PoolOutsideSubnet {
                    start,
                    end,
                    network: ip(0),
                    netmask: Ipv4Addr::new(255, 255, 255, 0),
                }
            );
        }
    }

    #[test]
    fn server_ip_must_be_a_host_address() {
        for server in [ip(0), ip(255)] {
            let mut cfg = config();
            cfg.server_ip = server;
            assert_eq!(
                BootpResponder::new(cfg).unwrap_err(),
                ConfigError::ServerIpNotHost(server)
            );
        }
        // /31 has no network or broadcast address: both ends are hosts.
        let mut cfg = config();
        cfg.server_ip = ip(10);
        cfg.netmask = Ipv4Addr::new(255, 255, 255, 254);
        cfg.pool_start = ip(11);
        cfg.pool_end = ip(11);
        assert!(BootpResponder::new(cfg).is_ok());
    }

    #[test]
    fn pool_without_usable_addresses_is_rejected() {
        for (start, end) in [(ip(1), ip(1)), (ip(0), ip(1)), (ip(255), ip(255))] {
            let mut cfg = config();
            cfg.pool_start = start;
            cfg.pool_end = end;
            assert_eq!(
                BootpResponder::new(cfg).unwrap_err(),
                ConfigError::EmptyPool { start, end },
                "{start}..={end}"
            );
        }
        let mut cfg = config();
        cfg.pool_start = ip(0);
        cfg.pool_end = ip(2);
        assert!(BootpResponder::new(cfg).is_ok(), ".2 is usable");
    }

    #[test]
    fn non_contiguous_netmask_is_rejected() {
        let mut cfg = config();
        cfg.netmask = Ipv4Addr::new(255, 0, 255, 0);
        assert_eq!(
            BootpResponder::new(cfg).unwrap_err(),
            ConfigError::InvalidNetmask(Ipv4Addr::new(255, 0, 255, 0))
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
