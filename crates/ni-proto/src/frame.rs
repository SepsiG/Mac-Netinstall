//! Netinstall frame codec.
//!
//! Wire layout (all integers little-endian), as written by pyNetinstall:
//!
//! | offset | size | field                         |
//! |--------|------|-------------------------------|
//! | 0      | 6    | source MAC                    |
//! | 6      | 6    | destination MAC               |
//! | 12     | 2    | reserved, always 0            |
//! | 14     | 2    | payload length                |
//! | 16     | 2    | `seq`: sender sequence number |
//! | 18     | 2    | `ack`: acknowledged sequence  |
//! | 20     | ..   | payload                       |
//!
//! Offset 16 always carries the *server's* sequence number: the server
//! increments it per frame, and the device echoes it back in its replies.
//! Offset 18 is the acknowledged number: the server puts its previous
//! sequence number there (`seq - 1`), the device the one it is answering
//! (so a device reply has `seq == ack`). Hello frames use `seq = 1, ack = 0`.

use bytes::Bytes;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// UDP port used by server and device (both directions, broadcast).
pub const PORT: u16 = 5000;
/// Fixed header length in bytes.
pub const HEADER_LEN: usize = 20;
/// Maximum file bytes carried by one frame.
pub const MAX_CHUNK: usize = 1024;

/// Ethernet MAC address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Mac(pub [u8; 6]);

impl Mac {
    pub const BROADCAST: Mac = Mac([0xff; 6]);
    pub const ZERO: Mac = Mac([0; 6]);
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0;
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    }
}

impl fmt::Debug for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mac({self})")
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid MAC address: {0:?}")]
pub struct ParseMacError(pub String);

impl FromStr for Mac {
    type Err = ParseMacError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseMacError(s.to_string());
        let parts: Vec<&str> = s.split([':', '-']).collect();
        if parts.len() != 6 {
            return Err(err());
        }
        let mut out = [0u8; 6];
        for (i, part) in parts.iter().enumerate() {
            if part.len() != 2 {
                return Err(err());
            }
            out[i] = u8::from_str_radix(part, 16).map_err(|_| err())?;
        }
        Ok(Mac(out))
    }
}

/// One Netinstall UDP datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub src: Mac,
    pub dst: Mac,
    pub seq: u16,
    pub ack: u16,
    pub payload: Bytes,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("frame too short: {0} bytes, need at least {HEADER_LEN}")]
    TooShort(usize),
}

impl Frame {
    pub fn new(src: Mac, dst: Mac, seq: u16, ack: u16, payload: impl Into<Bytes>) -> Self {
        Self {
            src,
            dst,
            seq,
            ack,
            payload: payload.into(),
        }
    }

    /// Serialize to wire format.
    ///
    /// # Panics
    ///
    /// If the payload is longer than [`u16::MAX`] bytes (it cannot be
    /// represented in the length field). Netinstall payloads are at most
    /// [`MAX_CHUNK`] bytes of file data or a short control message.
    pub fn encode(&self) -> Vec<u8> {
        let len = u16::try_from(self.payload.len()).expect("payload larger than u16::MAX");
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.src.0);
        out.extend_from_slice(&self.dst.0);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.ack.to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Parse from wire format.
    ///
    /// Deliberately lenient, like pyNetinstall: the reserved and length
    /// fields are not validated; the payload is everything after the header.
    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < HEADER_LEN {
            return Err(DecodeError::TooShort(buf.len()));
        }
        let mac_at = |o: usize| {
            let mut m = [0u8; 6];
            m.copy_from_slice(&buf[o..o + 6]);
            Mac(m)
        };
        let u16_at = |o: usize| u16::from_le_bytes([buf[o], buf[o + 1]]);
        Ok(Self {
            src: mac_at(0),
            dst: mac_at(6),
            seq: u16_at(16),
            ack: u16_at(18),
            payload: Bytes::copy_from_slice(&buf[HEADER_LEN..]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: Mac = Mac([0x02, 0, 0, 0, 0, 0x01]);
    const DEVICE: Mac = Mac([0xaa; 6]);

    #[test]
    fn encode_matches_pynetinstall_layout() {
        // pyNetinstall: mac + dev_mac + pack("<HHHH", 0, len, state[1], state[0]) + data
        let frame = Frame::new(SERVER, DEVICE, 1, 0, &b"OFFR\n\n"[..]);
        let expected: Vec<u8> = [
            &[0x02, 0, 0, 0, 0, 0x01][..],
            &[0xaa; 6][..],
            &[0x00, 0x00],
            &[0x06, 0x00],
            &[0x01, 0x00],
            &[0x00, 0x00],
            b"OFFR\n\n",
        ]
        .concat();
        assert_eq!(frame.encode(), expected);
    }

    #[test]
    fn decode_reads_seq_and_ack_from_offsets_16_and_18() {
        let mut buf = vec![0u8; HEADER_LEN];
        buf[16] = 0x34;
        buf[17] = 0x12;
        buf[18] = 0x78;
        buf[19] = 0x56;
        buf.extend_from_slice(b"RETR");
        let frame = Frame::decode(&buf).unwrap();
        assert_eq!(frame.seq, 0x1234);
        assert_eq!(frame.ack, 0x5678);
        assert_eq!(&frame.payload[..], b"RETR");
    }

    #[test]
    fn decode_rejects_short_buffer() {
        assert_eq!(Frame::decode(&[0u8; 19]), Err(DecodeError::TooShort(19)));
    }

    #[test]
    fn mac_parse_and_display_roundtrip() {
        let mac: Mac = "48:a9:8a:01:02:0F".parse().unwrap();
        assert_eq!(mac.0, [0x48, 0xa9, 0x8a, 0x01, 0x02, 0x0f]);
        assert_eq!(mac.to_string(), "48:A9:8A:01:02:0F");
        assert_eq!("48-A9-8A-01-02-0F".parse::<Mac>().unwrap(), mac);
        assert!("48:A9:8A:01:02".parse::<Mac>().is_err());
        assert!("48:A9:8A:01:02:GG".parse::<Mac>().is_err());
    }
}
