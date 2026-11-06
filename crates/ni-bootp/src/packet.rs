//! BOOTP (RFC 951) / DHCP (RFC 2131, 2132) packet codec.
//!
//! Fixed header (236 bytes), then optionally the RFC 1048 magic cookie
//! `99.130.83.99` and TLV options:
//!
//! | offset | size | field                                |
//! |--------|------|--------------------------------------|
//! | 0      | 1    | `op`: 1 = request, 2 = reply         |
//! | 1      | 1    | `htype`: 1 = Ethernet                |
//! | 2      | 1    | `hlen`: 6                            |
//! | 3      | 1    | `hops`                               |
//! | 4      | 4    | `xid` transaction id                 |
//! | 8      | 2    | `secs`                               |
//! | 10     | 2    | `flags` (bit 15 = broadcast)         |
//! | 12     | 4    | `ciaddr` client IP (if it has one)   |
//! | 16     | 4    | `yiaddr` "your" IP (assigned)        |
//! | 20     | 4    | `siaddr` next server (TFTP)          |
//! | 24     | 4    | `giaddr` relay agent                 |
//! | 28     | 16   | `chaddr` client hardware address     |
//! | 44     | 64   | `sname` server host name             |
//! | 108    | 128  | `file` boot file name                |
//! | 236    | 4    | magic cookie (optional)              |
//! | 240    | ..   | options                              |

use ni_proto::Mac;
use std::net::Ipv4Addr;
use thiserror::Error;

/// BOOTP server port (requests arrive here).
pub const SERVER_PORT: u16 = 67;
/// BOOTP client port (replies are sent here).
pub const CLIENT_PORT: u16 = 68;
/// RFC 1048 magic cookie.
pub const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];
/// Length of the fixed header, without cookie and options.
pub const HEADER_LEN: usize = 236;
/// Minimum BOOTP message length; replies are zero-padded up to it.
pub const MIN_LEN: usize = 300;
/// Longest boot file name that fits `file` with a terminating NUL.
pub const MAX_FILE_LEN: usize = 127;

const OP_REQUEST: u8 = 1;
const OP_REPLY: u8 = 2;

/// DHCP option codes used here (RFC 2132, RFC 4578).
pub mod opt {
    pub const PAD: u8 = 0;
    pub const SUBNET_MASK: u8 = 1;
    pub const REQUESTED_IP: u8 = 50;
    pub const LEASE_TIME: u8 = 51;
    pub const MESSAGE_TYPE: u8 = 53;
    pub const SERVER_ID: u8 = 54;
    pub const PARAM_REQUEST_LIST: u8 = 55;
    pub const VENDOR_CLASS: u8 = 60;
    pub const CLIENT_ARCH: u8 = 93;
    pub const END: u8 = 255;
}

/// DHCP message type (option 53).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Discover,
    Offer,
    Request,
    Decline,
    Ack,
    Nak,
    Release,
    Inform,
    Other(u8),
}

impl MessageType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => MessageType::Discover,
            2 => MessageType::Offer,
            3 => MessageType::Request,
            4 => MessageType::Decline,
            5 => MessageType::Ack,
            6 => MessageType::Nak,
            7 => MessageType::Release,
            8 => MessageType::Inform,
            other => MessageType::Other(other),
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            MessageType::Discover => 1,
            MessageType::Offer => 2,
            MessageType::Request => 3,
            MessageType::Decline => 4,
            MessageType::Ack => 5,
            MessageType::Nak => 6,
            MessageType::Release => 7,
            MessageType::Inform => 8,
            MessageType::Other(v) => v,
        }
    }
}

/// Why a datagram could not be parsed as a BOOTP request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PacketError {
    #[error("packet too short: {0} bytes")]
    TooShort(usize),
    #[error("not a BOOTP request (op {0})")]
    NotRequest(u8),
    #[error("unsupported hardware type {htype} / length {hlen}")]
    UnsupportedHardware { htype: u8, hlen: u8 },
    #[error("option {code} truncated")]
    TruncatedOption { code: u8 },
}

/// A parsed BOOTP/DHCP client request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Relay hop count; non-zero means the request was relayed.
    pub hops: u8,
    pub xid: u32,
    pub secs: u16,
    pub flags: u16,
    pub ciaddr: Ipv4Addr,
    pub giaddr: Ipv4Addr,
    pub chaddr: Mac,
    /// Server host name requested by the client (usually empty).
    pub sname: String,
    /// Boot file name requested by the client (usually empty).
    pub file: String,
    /// `true` if the RFC 1048 magic cookie was present.
    pub has_cookie: bool,
    /// Option 53; `None` for plain BOOTP.
    pub message_type: Option<MessageType>,
    /// Option 60.
    pub vendor_class: Option<String>,
    /// Option 93 (first entry).
    pub client_arch: Option<u16>,
    /// Option 50.
    pub requested_ip: Option<Ipv4Addr>,
    /// Option 54.
    pub server_id: Option<Ipv4Addr>,
    /// Option 55.
    pub param_request_list: Vec<u8>,
}

impl Request {
    /// `true` for DHCP (cookie and option 53), `false` for plain BOOTP.
    pub fn is_dhcp(&self) -> bool {
        self.has_cookie && self.message_type.is_some()
    }

    /// Parse a datagram received on port 67.
    pub fn parse(buf: &[u8]) -> Result<Request, PacketError> {
        if buf.len() < HEADER_LEN {
            return Err(PacketError::TooShort(buf.len()));
        }
        if buf[0] != OP_REQUEST {
            return Err(PacketError::NotRequest(buf[0]));
        }
        let (htype, hlen) = (buf[1], buf[2]);
        if htype != 1 || hlen != 6 {
            return Err(PacketError::UnsupportedHardware { htype, hlen });
        }
        let mut chaddr = [0u8; 6];
        chaddr.copy_from_slice(&buf[28..34]);
        let mut req = Request {
            hops: buf[3],
            xid: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            secs: u16::from_be_bytes([buf[8], buf[9]]),
            flags: u16::from_be_bytes([buf[10], buf[11]]),
            ciaddr: ip_at(buf, 12),
            giaddr: ip_at(buf, 24),
            chaddr: Mac(chaddr),
            sname: c_string(&buf[44..108]),
            file: c_string(&buf[108..236]),
            has_cookie: false,
            message_type: None,
            vendor_class: None,
            client_arch: None,
            requested_ip: None,
            server_id: None,
            param_request_list: Vec::new(),
        };
        if buf.len() >= HEADER_LEN + 4 && buf[HEADER_LEN..HEADER_LEN + 4] == MAGIC_COOKIE {
            req.has_cookie = true;
            req.parse_options(&buf[HEADER_LEN + 4..])?;
        }
        Ok(req)
    }

    fn parse_options(&mut self, mut rest: &[u8]) -> Result<(), PacketError> {
        while let Some((&code, tail)) = rest.split_first() {
            match code {
                opt::PAD => {
                    rest = tail;
                    continue;
                }
                opt::END => break,
                _ => {}
            }
            let (&len, tail) = tail
                .split_first()
                .ok_or(PacketError::TruncatedOption { code })?;
            let len = len as usize;
            if tail.len() < len {
                return Err(PacketError::TruncatedOption { code });
            }
            let (data, tail) = tail.split_at(len);
            rest = tail;
            match code {
                opt::MESSAGE_TYPE if len == 1 => {
                    self.message_type = Some(MessageType::from_u8(data[0]))
                }
                opt::VENDOR_CLASS => {
                    self.vendor_class = Some(String::from_utf8_lossy(data).into_owned())
                }
                opt::CLIENT_ARCH if len >= 2 => {
                    self.client_arch = Some(u16::from_be_bytes([data[0], data[1]]))
                }
                opt::REQUESTED_IP if len == 4 => self.requested_ip = Some(ip_at(data, 0)),
                opt::SERVER_ID if len == 4 => self.server_id = Some(ip_at(data, 0)),
                opt::PARAM_REQUEST_LIST => self.param_request_list = data.to_vec(),
                _ => {}
            }
        }
        Ok(())
    }
}

/// What kind of reply to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    /// Plain BOOTP reply (no option 53).
    Bootp,
    /// DHCPOFFER, answering DHCPDISCOVER.
    Offer,
    /// DHCPACK, answering DHCPREQUEST.
    Ack,
    /// DHCPNAK, refusing a DHCPREQUEST for an address we did not lease
    /// to that client. Carries only options 53 and 54: no `yiaddr`,
    /// `siaddr`, `ciaddr`, boot file, lease time or netmask.
    Nak,
}

/// Values for [`build_reply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyParams<'a> {
    pub kind: ReplyKind,
    /// Address handed to the client (`yiaddr`).
    pub your_ip: Ipv4Addr,
    /// Our address: `siaddr` (TFTP next-server) and option 54.
    pub server_ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub lease_secs: u32,
    /// Boot file name; truncated to [`MAX_FILE_LEN`] bytes.
    pub file: &'a str,
}

/// Build a reply to `req`. The result is at least [`MIN_LEN`] bytes.
///
/// `ciaddr` is copied from the request only for ACK and plain BOOTP
/// replies (RFC 2131 table 3: OFFER and NAK carry 0). A NAK ignores
/// `your_ip`, `netmask`, `lease_secs` and `file`.
pub fn build_reply(req: &Request, p: &ReplyParams<'_>) -> Vec<u8> {
    let nak = p.kind == ReplyKind::Nak;
    let mut out = vec![0u8; HEADER_LEN];
    out[0] = OP_REPLY;
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&req.xid.to_be_bytes());
    out[10..12].copy_from_slice(&req.flags.to_be_bytes());
    if matches!(p.kind, ReplyKind::Ack | ReplyKind::Bootp) {
        out[12..16].copy_from_slice(&req.ciaddr.octets());
    }
    if !nak {
        out[16..20].copy_from_slice(&p.your_ip.octets());
        out[20..24].copy_from_slice(&p.server_ip.octets());
    }
    out[24..28].copy_from_slice(&req.giaddr.octets());
    out[28..34].copy_from_slice(&req.chaddr.0);
    if !nak {
        let file = p.file.as_bytes();
        let n = file.len().min(MAX_FILE_LEN);
        out[108..108 + n].copy_from_slice(&file[..n]);
    }
    out.extend_from_slice(&MAGIC_COOKIE);
    let msg_type = match p.kind {
        ReplyKind::Bootp => None,
        ReplyKind::Offer => Some(MessageType::Offer),
        ReplyKind::Ack => Some(MessageType::Ack),
        ReplyKind::Nak => Some(MessageType::Nak),
    };
    if let Some(t) = msg_type {
        out.extend_from_slice(&[opt::MESSAGE_TYPE, 1, t.to_u8()]);
        out.extend_from_slice(&[opt::SERVER_ID, 4]);
        out.extend_from_slice(&p.server_ip.octets());
    }
    if !nak {
        if msg_type.is_some() {
            out.extend_from_slice(&[opt::LEASE_TIME, 4]);
            out.extend_from_slice(&p.lease_secs.to_be_bytes());
        }
        out.extend_from_slice(&[opt::SUBNET_MASK, 4]);
        out.extend_from_slice(&p.netmask.octets());
    }
    out.push(opt::END);
    if out.len() < MIN_LEN {
        out.resize(MIN_LEN, 0);
    }
    out
}

fn ip_at(buf: &[u8], at: usize) -> Ipv4Addr {
    Ipv4Addr::new(buf[at], buf[at + 1], buf[at + 2], buf[at + 3])
}

fn c_string(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Test helper: build a client request like RouterBOOT would send.
#[cfg(test)]
pub(crate) fn sample_request(
    mac: Mac,
    msg_type: Option<u8>,
    vendor_class: Option<&str>,
) -> Vec<u8> {
    sample_request_with(mac, msg_type, vendor_class, &[])
}

/// Test helper: [`sample_request`] with raw option bytes `extra` before END.
#[cfg(test)]
pub(crate) fn sample_request_with(
    mac: Mac,
    msg_type: Option<u8>,
    vendor_class: Option<&str>,
    extra: &[u8],
) -> Vec<u8> {
    let mut b = vec![0u8; HEADER_LEN];
    b[0] = OP_REQUEST;
    b[1] = 1;
    b[2] = 6;
    b[4..8].copy_from_slice(&0xdead_beef_u32.to_be_bytes());
    b[8..10].copy_from_slice(&3u16.to_be_bytes());
    b[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
    b[28..34].copy_from_slice(&mac.0);
    b.extend_from_slice(&MAGIC_COOKIE);
    if let Some(t) = msg_type {
        b.extend_from_slice(&[opt::MESSAGE_TYPE, 1, t]);
    }
    if let Some(vc) = vendor_class {
        b.push(opt::VENDOR_CLASS);
        b.push(vc.len() as u8);
        b.extend_from_slice(vc.as_bytes());
    }
    b.extend_from_slice(extra);
    b.push(opt::END);
    b.resize(MIN_LEN, 0);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 0x01, 0x02, 0x03]);

    #[test]
    fn parses_dhcp_discover_with_vendor_class() {
        let buf = sample_request(MAC, Some(1), Some("MMipsBoot"));
        let req = Request::parse(&buf).unwrap();
        assert_eq!(req.xid, 0xdead_beef);
        assert_eq!(req.secs, 3);
        assert_eq!(req.flags, 0x8000);
        assert_eq!(req.chaddr, MAC);
        assert_eq!(req.ciaddr, Ipv4Addr::UNSPECIFIED);
        assert!(req.has_cookie);
        assert!(req.is_dhcp());
        assert_eq!(req.message_type, Some(MessageType::Discover));
        assert_eq!(req.vendor_class.as_deref(), Some("MMipsBoot"));
    }

    #[test]
    fn cookie_without_option_53_is_plain_bootp() {
        let buf = sample_request(MAC, None, Some("Mips_boot"));
        let req = Request::parse(&buf).unwrap();
        assert!(req.has_cookie);
        assert!(!req.is_dhcp());
        assert_eq!(req.vendor_class.as_deref(), Some("Mips_boot"));
    }

    #[test]
    fn header_only_request_has_no_options() {
        let mut buf = sample_request(MAC, None, None);
        buf.truncate(HEADER_LEN);
        let req = Request::parse(&buf).unwrap();
        assert!(!req.has_cookie);
        assert_eq!(req.vendor_class, None);
    }

    #[test]
    fn non_rfc1048_vendor_area_is_ignored() {
        let mut buf = sample_request(MAC, None, None);
        buf[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(b"junk");
        let req = Request::parse(&buf).unwrap();
        assert!(!req.has_cookie);
    }

    #[test]
    fn parses_other_options_and_skips_pad() {
        let mut buf = sample_request(MAC, None, None);
        buf.truncate(HEADER_LEN + 4);
        buf[12..16].copy_from_slice(&[10, 0, 0, 9]);
        buf[44..48].copy_from_slice(b"srv\0");
        buf[108..115].copy_from_slice(b"kernel\0");
        buf.extend_from_slice(&[opt::PAD, opt::PAD]);
        buf.extend_from_slice(&[opt::MESSAGE_TYPE, 1, 3]);
        buf.extend_from_slice(&[opt::REQUESTED_IP, 4, 192, 168, 88, 10]);
        buf.extend_from_slice(&[opt::SERVER_ID, 4, 192, 168, 88, 1]);
        buf.extend_from_slice(&[opt::PARAM_REQUEST_LIST, 3, 1, 3, 6]);
        buf.extend_from_slice(&[opt::CLIENT_ARCH, 2, 0x00, 0x0b]);
        buf.extend_from_slice(&[200, 2, 0xaa, 0xbb]); // unknown option
                                                      // No END option: tolerated.
        let req = Request::parse(&buf).unwrap();
        assert_eq!(req.ciaddr, Ipv4Addr::new(10, 0, 0, 9));
        assert_eq!(req.sname, "srv");
        assert_eq!(req.file, "kernel");
        assert_eq!(req.message_type, Some(MessageType::Request));
        assert_eq!(req.requested_ip, Some(Ipv4Addr::new(192, 168, 88, 10)));
        assert_eq!(req.server_id, Some(Ipv4Addr::new(192, 168, 88, 1)));
        assert_eq!(req.param_request_list, vec![1, 3, 6]);
        assert_eq!(req.client_arch, Some(11));
    }

    #[test]
    fn bytes_after_end_are_ignored() {
        let mut buf = sample_request(MAC, Some(1), None);
        buf.truncate(HEADER_LEN + 4 + 3);
        buf.extend_from_slice(&[opt::END, opt::VENDOR_CLASS, 50]);
        assert!(Request::parse(&buf).is_ok());
    }

    #[test]
    fn rejects_short_packet() {
        assert_eq!(Request::parse(&[1u8; 235]), Err(PacketError::TooShort(235)));
    }

    #[test]
    fn rejects_reply_op() {
        let mut buf = sample_request(MAC, Some(1), None);
        buf[0] = 2;
        assert_eq!(Request::parse(&buf), Err(PacketError::NotRequest(2)));
    }

    #[test]
    fn rejects_non_ethernet() {
        let mut buf = sample_request(MAC, Some(1), None);
        buf[1] = 6;
        assert_eq!(
            Request::parse(&buf),
            Err(PacketError::UnsupportedHardware { htype: 6, hlen: 6 })
        );
    }

    #[test]
    fn rejects_truncated_option() {
        let mut buf = sample_request(MAC, None, None);
        buf.truncate(HEADER_LEN + 4);
        buf.extend_from_slice(&[opt::VENDOR_CLASS, 9, b'M', b'i']);
        assert_eq!(
            Request::parse(&buf),
            Err(PacketError::TruncatedOption { code: 60 })
        );
        buf.truncate(HEADER_LEN + 4);
        buf.push(opt::VENDOR_CLASS);
        assert_eq!(
            Request::parse(&buf),
            Err(PacketError::TruncatedOption { code: 60 })
        );
    }

    fn params(kind: ReplyKind) -> ReplyParams<'static> {
        ReplyParams {
            kind,
            your_ip: Ipv4Addr::new(192, 168, 88, 10),
            server_ip: Ipv4Addr::new(192, 168, 88, 1),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            lease_secs: 3600,
            file: "mmips.elf",
        }
    }

    #[test]
    fn dhcp_offer_layout() {
        let req = Request::parse(&sample_request(MAC, Some(1), Some("MMipsBoot"))).unwrap();
        let out = build_reply(&req, &params(ReplyKind::Offer));
        assert_eq!(out.len(), MIN_LEN);
        assert_eq!(&out[0..4], &[2, 1, 6, 0]);
        assert_eq!(&out[4..8], &0xdead_beef_u32.to_be_bytes());
        assert_eq!(&out[8..10], &[0, 0], "secs is zero in replies");
        assert_eq!(&out[10..12], &[0x80, 0x00], "flags copied");
        assert_eq!(&out[16..20], &[192, 168, 88, 10], "yiaddr");
        assert_eq!(&out[20..24], &[192, 168, 88, 1], "siaddr");
        assert_eq!(&out[28..34], &MAC.0);
        assert_eq!(&out[34..44], &[0u8; 10], "chaddr padding");
        assert_eq!(&out[108..118], b"mmips.elf\0");
        assert_eq!(&out[236..240], &MAGIC_COOKIE);
        assert_eq!(
            &out[240..262],
            &[
                53, 1, 2, // OFFER
                54, 4, 192, 168, 88, 1, // server id
                51, 4, 0, 0, 0x0e, 0x10, // lease 3600 s
                1, 4, 255, 255, 255, 0,   // netmask
                255, // end
            ]
        );
        assert!(out[262..].iter().all(|&b| b == 0));
    }

    #[test]
    fn parses_hops() {
        let mut buf = sample_request(MAC, Some(1), None);
        buf[3] = 2;
        assert_eq!(Request::parse(&buf).unwrap().hops, 2);
    }

    #[test]
    fn ciaddr_is_zero_in_offer_and_copied_in_ack() {
        let mut buf = sample_request(MAC, Some(1), Some("MMipsBoot"));
        buf[12..16].copy_from_slice(&[10, 0, 0, 9]);
        let req = Request::parse(&buf).unwrap();
        let offer = build_reply(&req, &params(ReplyKind::Offer));
        assert_eq!(&offer[12..16], &[0, 0, 0, 0], "OFFER ciaddr");
        let ack = build_reply(&req, &params(ReplyKind::Ack));
        assert_eq!(&ack[12..16], &[10, 0, 0, 9], "ACK ciaddr");
        let bootp = build_reply(&req, &params(ReplyKind::Bootp));
        assert_eq!(&bootp[12..16], &[10, 0, 0, 9], "BOOTP ciaddr");
        let nak = build_reply(&req, &params(ReplyKind::Nak));
        assert_eq!(&nak[12..16], &[0, 0, 0, 0], "NAK ciaddr");
    }

    #[test]
    fn dhcp_nak_layout() {
        let mut buf = sample_request(MAC, Some(3), Some("MMipsBoot"));
        buf[12..16].copy_from_slice(&[10, 0, 0, 9]);
        let req = Request::parse(&buf).unwrap();
        let out = build_reply(&req, &params(ReplyKind::Nak));
        assert_eq!(out.len(), MIN_LEN);
        assert_eq!(&out[0..4], &[2, 1, 6, 0]);
        assert_eq!(&out[4..8], &0xdead_beef_u32.to_be_bytes());
        assert_eq!(&out[10..12], &[0x80, 0x00], "flags copied");
        assert_eq!(&out[12..28], &[0u8; 16], "ciaddr, yiaddr, siaddr, giaddr");
        assert_eq!(&out[28..34], &MAC.0);
        assert!(out[44..236].iter().all(|&b| b == 0), "no sname, no file");
        assert_eq!(&out[236..240], &MAGIC_COOKIE);
        assert_eq!(
            &out[240..250],
            &[
                53, 1, 6, // NAK
                54, 4, 192, 168, 88, 1,   // server id
                255, // end: no lease time, no netmask
            ]
        );
        assert!(out[250..].iter().all(|&b| b == 0));
    }

    #[test]
    fn dhcp_ack_has_type_5() {
        let req = Request::parse(&sample_request(MAC, Some(3), Some("MMipsBoot"))).unwrap();
        let out = build_reply(&req, &params(ReplyKind::Ack));
        assert_eq!(&out[240..243], &[53, 1, 5]);
    }

    #[test]
    fn bootp_reply_has_only_netmask() {
        let req = Request::parse(&sample_request(MAC, None, Some("Mips_boot"))).unwrap();
        let out = build_reply(&req, &params(ReplyKind::Bootp));
        assert_eq!(out.len(), MIN_LEN);
        assert_eq!(
            &out[236..247],
            &[99, 130, 83, 99, 1, 4, 255, 255, 255, 0, 255]
        );
    }

    #[test]
    fn reply_round_trips_through_parser_fields() {
        let req = Request::parse(&sample_request(MAC, Some(1), None)).unwrap();
        let mut out = build_reply(&req, &params(ReplyKind::Offer));
        out[0] = 1; // pretend it's a request to reuse the parser
        let back = Request::parse(&out).unwrap();
        assert_eq!(back.xid, req.xid);
        assert_eq!(back.chaddr, MAC);
        assert_eq!(back.file, "mmips.elf");
        assert_eq!(back.message_type, Some(MessageType::Offer));
        assert_eq!(back.server_id, Some(Ipv4Addr::new(192, 168, 88, 1)));
    }

    #[test]
    fn long_file_name_is_truncated_and_nul_terminated() {
        let req = Request::parse(&sample_request(MAC, None, None)).unwrap();
        let long = "x".repeat(200);
        let mut p = params(ReplyKind::Bootp);
        p.file = &long;
        let out = build_reply(&req, &p);
        assert!(out[108..108 + MAX_FILE_LEN].iter().all(|&b| b == b'x'));
        assert_eq!(out[108 + MAX_FILE_LEN], 0);
    }

    #[test]
    fn message_type_round_trip() {
        for v in 0..=10u8 {
            assert_eq!(MessageType::from_u8(v).to_u8(), v);
        }
    }
}
