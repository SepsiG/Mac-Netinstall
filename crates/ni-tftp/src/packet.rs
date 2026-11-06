//! TFTP packet codec (RFC 1350, options per RFC 2347).
//!
//! All integers are big-endian. Strings are NUL-terminated.
//!
//! | opcode | packet | layout after the 2-byte opcode               |
//! |--------|--------|----------------------------------------------|
//! | 1      | RRQ    | filename, mode, then option name/value pairs |
//! | 2      | WRQ    | same as RRQ                                  |
//! | 3      | DATA   | block (u16), 0..=blksize data bytes          |
//! | 4      | ACK    | block (u16)                                  |
//! | 5      | ERROR  | code (u16), message                          |
//! | 6      | OACK   | option name/value pairs                      |

use thiserror::Error;

/// Well-known TFTP server port.
pub const PORT: u16 = 69;
/// Block size without the `blksize` option.
pub const DEFAULT_BLKSIZE: usize = 512;

/// TFTP error codes (RFC 1350, RFC 2347).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    NotDefined,
    FileNotFound,
    AccessViolation,
    DiskFull,
    IllegalOperation,
    UnknownTransferId,
    FileExists,
    NoSuchUser,
    OptionRefused,
    Other(u16),
}

impl ErrorCode {
    pub fn from_u16(v: u16) -> Self {
        match v {
            0 => ErrorCode::NotDefined,
            1 => ErrorCode::FileNotFound,
            2 => ErrorCode::AccessViolation,
            3 => ErrorCode::DiskFull,
            4 => ErrorCode::IllegalOperation,
            5 => ErrorCode::UnknownTransferId,
            6 => ErrorCode::FileExists,
            7 => ErrorCode::NoSuchUser,
            8 => ErrorCode::OptionRefused,
            other => ErrorCode::Other(other),
        }
    }

    pub fn to_u16(self) -> u16 {
        match self {
            ErrorCode::NotDefined => 0,
            ErrorCode::FileNotFound => 1,
            ErrorCode::AccessViolation => 2,
            ErrorCode::DiskFull => 3,
            ErrorCode::IllegalOperation => 4,
            ErrorCode::UnknownTransferId => 5,
            ErrorCode::FileExists => 6,
            ErrorCode::NoSuchUser => 7,
            ErrorCode::OptionRefused => 8,
            ErrorCode::Other(v) => v,
        }
    }
}

/// RRQ/WRQ contents. Option names are lowercased; values kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub filename: String,
    /// Transfer mode, lowercased (`octet`, `netascii`, ...).
    pub mode: String,
    pub options: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Rrq(Request),
    Wrq(Request),
    Data { block: u16, data: Vec<u8> },
    Ack(u16),
    Error { code: ErrorCode, message: String },
    Oack(Vec<(String, String)>),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PacketError {
    #[error("packet too short")]
    TooShort,
    #[error("unknown opcode {0}")]
    UnknownOpcode(u16),
    #[error("missing NUL terminator")]
    Unterminated,
    #[error("option {0:?} has no value")]
    OptionWithoutValue(String),
}

impl Packet {
    pub fn parse(buf: &[u8]) -> Result<Packet, PacketError> {
        if buf.len() < 2 {
            return Err(PacketError::TooShort);
        }
        let opcode = u16::from_be_bytes([buf[0], buf[1]]);
        let body = &buf[2..];
        match opcode {
            1 | 2 => {
                let mut fields = Strings(body);
                let filename = fields.next_str()?.ok_or(PacketError::TooShort)?;
                let mode = fields
                    .next_str()?
                    .ok_or(PacketError::TooShort)?
                    .to_ascii_lowercase();
                let options = fields.pairs()?;
                let req = Request {
                    filename,
                    mode,
                    options,
                };
                Ok(if opcode == 1 {
                    Packet::Rrq(req)
                } else {
                    Packet::Wrq(req)
                })
            }
            3 => {
                let block = be16(body)?;
                Ok(Packet::Data {
                    block,
                    data: body[2..].to_vec(),
                })
            }
            4 => Ok(Packet::Ack(be16(body)?)),
            5 => {
                let code = ErrorCode::from_u16(be16(body)?);
                // Be lenient: a missing terminator still yields the text.
                let text = &body[2..];
                let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
                let message = String::from_utf8_lossy(&text[..end]).into_owned();
                Ok(Packet::Error { code, message })
            }
            6 => Ok(Packet::Oack(Strings(body).pairs()?)),
            other => Err(PacketError::UnknownOpcode(other)),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Packet::Rrq(r) | Packet::Wrq(r) => {
                let op: u16 = if matches!(self, Packet::Rrq(_)) { 1 } else { 2 };
                out.extend_from_slice(&op.to_be_bytes());
                put_str(&mut out, &r.filename);
                put_str(&mut out, &r.mode);
                for (k, v) in &r.options {
                    put_str(&mut out, k);
                    put_str(&mut out, v);
                }
            }
            Packet::Data { block, data } => {
                out.extend_from_slice(&3u16.to_be_bytes());
                out.extend_from_slice(&block.to_be_bytes());
                out.extend_from_slice(data);
            }
            Packet::Ack(block) => {
                out.extend_from_slice(&4u16.to_be_bytes());
                out.extend_from_slice(&block.to_be_bytes());
            }
            Packet::Error { code, message } => {
                out.extend_from_slice(&5u16.to_be_bytes());
                out.extend_from_slice(&code.to_u16().to_be_bytes());
                put_str(&mut out, message);
            }
            Packet::Oack(opts) => {
                out.extend_from_slice(&6u16.to_be_bytes());
                for (k, v) in opts {
                    put_str(&mut out, k);
                    put_str(&mut out, v);
                }
            }
        }
        out
    }
}

/// Encode a DATA packet without building a [`Packet`] (avoids a copy).
pub fn encode_data(block: u16, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&block.to_be_bytes());
    out.extend_from_slice(data);
    out
}

fn be16(b: &[u8]) -> Result<u16, PacketError> {
    match b {
        [hi, lo, ..] => Ok(u16::from_be_bytes([*hi, *lo])),
        _ => Err(PacketError::TooShort),
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// Iterator over NUL-terminated strings.
struct Strings<'a>(&'a [u8]);

impl Strings<'_> {
    fn next_str(&mut self) -> Result<Option<String>, PacketError> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let end = self
            .0
            .iter()
            .position(|&b| b == 0)
            .ok_or(PacketError::Unterminated)?;
        let s = String::from_utf8_lossy(&self.0[..end]).into_owned();
        self.0 = &self.0[end + 1..];
        Ok(Some(s))
    }

    fn pairs(&mut self) -> Result<Vec<(String, String)>, PacketError> {
        let mut out = Vec::new();
        while let Some(name) = self.next_str()? {
            let value = self
                .next_str()?
                .ok_or_else(|| PacketError::OptionWithoutValue(name.clone()))?;
            out.push((name.to_ascii_lowercase(), value));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_rrq() {
        let p = Packet::parse(b"\x00\x01boot.elf\x00octet\x00").unwrap();
        assert_eq!(
            p,
            Packet::Rrq(Request {
                filename: "boot.elf".into(),
                mode: "octet".into(),
                options: vec![],
            })
        );
    }

    #[test]
    fn parses_rrq_with_options_and_lowercases_names_and_mode() {
        let p =
            Packet::parse(b"\x00\x01k\x00NetASCII\x00TSize\x000\x00blksize\x001468\x00").unwrap();
        let Packet::Rrq(r) = p else { panic!() };
        assert_eq!(r.mode, "netascii");
        assert_eq!(
            r.options,
            vec![
                ("tsize".to_string(), "0".to_string()),
                ("blksize".to_string(), "1468".to_string())
            ]
        );
    }

    #[test]
    fn parses_wrq() {
        assert!(matches!(
            Packet::parse(b"\x00\x02f\x00octet\x00"),
            Ok(Packet::Wrq(_))
        ));
    }

    #[test]
    fn rejects_malformed_requests() {
        assert_eq!(Packet::parse(b"\x00"), Err(PacketError::TooShort));
        assert_eq!(Packet::parse(b"\x00\x01"), Err(PacketError::TooShort));
        assert_eq!(Packet::parse(b"\x00\x01f\x00"), Err(PacketError::TooShort));
        assert_eq!(
            Packet::parse(b"\x00\x01file"),
            Err(PacketError::Unterminated)
        );
        assert_eq!(
            Packet::parse(b"\x00\x01f\x00octet\x00tsize\x00"),
            Err(PacketError::OptionWithoutValue("tsize".into()))
        );
        assert_eq!(
            Packet::parse(b"\x00\x09"),
            Err(PacketError::UnknownOpcode(9))
        );
        assert_eq!(Packet::parse(b"\x00\x04\x01"), Err(PacketError::TooShort));
    }

    #[test]
    fn parses_ack_data_error_oack() {
        assert_eq!(Packet::parse(b"\x00\x04\x01\x02"), Ok(Packet::Ack(0x0102)));
        assert_eq!(
            Packet::parse(b"\x00\x03\x00\x07abc"),
            Ok(Packet::Data {
                block: 7,
                data: b"abc".to_vec()
            })
        );
        assert_eq!(
            Packet::parse(b"\x00\x05\x00\x01nope\x00"),
            Ok(Packet::Error {
                code: ErrorCode::FileNotFound,
                message: "nope".into()
            })
        );
        assert_eq!(
            Packet::parse(b"\x00\x05\x00\x08no nul"),
            Ok(Packet::Error {
                code: ErrorCode::OptionRefused,
                message: "no nul".into()
            })
        );
        assert_eq!(
            Packet::parse(b"\x00\x06tsize\x0042\x00"),
            Ok(Packet::Oack(vec![("tsize".into(), "42".into())]))
        );
    }

    #[test]
    fn encodes_exact_bytes() {
        assert_eq!(Packet::Ack(513).encode(), b"\x00\x04\x02\x01");
        assert_eq!(encode_data(1, b"hi"), b"\x00\x03\x00\x01hi");
        assert_eq!(
            Packet::Data {
                block: 1,
                data: b"hi".to_vec()
            }
            .encode(),
            encode_data(1, b"hi")
        );
        assert_eq!(
            Packet::Error {
                code: ErrorCode::AccessViolation,
                message: "access violation".into()
            }
            .encode(),
            b"\x00\x05\x00\x02access violation\x00"
        );
        assert_eq!(
            Packet::Oack(vec![("tsize".into(), "1500".into())]).encode(),
            b"\x00\x06tsize\x001500\x00"
        );
    }

    #[test]
    fn requests_round_trip() {
        let req = Request {
            filename: "a/b".into(),
            mode: "octet".into(),
            options: vec![("tsize".into(), "0".into())],
        };
        for p in [Packet::Rrq(req.clone()), Packet::Wrq(req)] {
            assert_eq!(Packet::parse(&p.encode()).unwrap(), p);
        }
    }

    #[test]
    fn error_code_round_trip() {
        for v in 0..=10u16 {
            assert_eq!(ErrorCode::from_u16(v).to_u16(), v);
        }
    }
}
