//! Device discovery ("hello") frames.

use crate::frame::{Frame, Mac};

/// The first field of every hello.
pub const HELLO_TAG: &str = "DSCV";

/// Information a device in Netinstall mode broadcasts while waiting for a server.
///
/// Payload layout (newline separated): `DSCV\n<license id>\n<license
/// key>\n<model>\n<arch>\n<min os>\n<f7>\n<f8>\n`. The official server
/// requires all 8 fields; pyNetinstall reads the first 6, and so do we
/// (fields 7 and 8, meaning unknown, are kept in [`DeviceInfo::extra`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub mac: Mac,
    /// Always [`HELLO_TAG`].
    pub tag: String,
    pub license_id: String,
    pub license_key: String,
    pub model: String,
    pub arch: String,
    pub min_os: String,
    /// Fields after `min_os` (real devices send two).
    pub extra: Vec<String>,
}

impl DeviceInfo {
    /// A hello frame carries `seq = 1, ack = 0`. So does a server's offer
    /// (`OFFR`), which [`DeviceInfo::parse`] rejects by its first field.
    pub fn is_hello(frame: &Frame) -> bool {
        frame.seq == 1 && frame.ack == 0
    }

    /// Parse a hello frame. Returns `None` unless the first field is
    /// [`HELLO_TAG`] and there are at least 6 fields.
    pub fn parse(frame: &Frame) -> Option<Self> {
        let text = String::from_utf8_lossy(&frame.payload);
        let mut rows: Vec<&str> = text.split('\n').collect();
        if rows.len() < 6 || rows[0] != HELLO_TAG {
            return None;
        }
        if rows.len() > 6 && rows.last() == Some(&"") {
            rows.pop(); // the newline ending the last field
        }
        Some(Self {
            mac: frame.src,
            tag: rows[0].to_string(),
            license_id: rows[1].to_string(),
            license_key: rows[2].to_string(),
            model: rows[3].to_string(),
            arch: rows[4].to_string(),
            min_os: rows[5].to_string(),
            extra: rows[6..].iter().map(|s| s.to_string()).collect(),
        })
    }

    /// Encode as hello payload (used by the device simulator): every field
    /// followed by a newline.
    pub fn to_payload(&self) -> Vec<u8> {
        let mut out = String::new();
        for field in [
            &self.tag,
            &self.license_id,
            &self.license_key,
            &self.model,
            &self.arch,
            &self.min_os,
        ]
        .into_iter()
        .chain(&self.extra)
        {
            out.push_str(field);
            out.push('\n');
        }
        out.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(payload: &[u8]) -> Frame {
        Frame::new(Mac([0xaa; 6]), Mac::BROADCAST, 1, 0, payload.to_vec())
    }

    #[test]
    fn parses_pynetinstall_field_order() {
        let info =
            DeviceInfo::parse(&hello(b"DSCV\nLIC1\nKEY1\nRB5009UG+S+\narm64\n7.1\n")).unwrap();
        assert_eq!(info.mac, Mac([0xaa; 6]));
        assert_eq!(info.tag, "DSCV");
        assert_eq!(info.license_id, "LIC1");
        assert_eq!(info.license_key, "KEY1");
        assert_eq!(info.model, "RB5009UG+S+");
        assert_eq!(info.arch, "arm64");
        assert_eq!(info.min_os, "7.1");
        assert!(info.extra.is_empty());
    }

    #[test]
    fn parses_the_official_eight_fields() {
        let info = DeviceInfo::parse(&hello(
            b"DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n",
        ))
        .unwrap();
        assert_eq!(info.license_id, "ABCD-1234");
        assert_eq!(info.arch, "arm64");
        assert_eq!(info.extra, ["x", "y"]);
        assert_eq!(
            info.to_payload(),
            b"DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
        );
    }

    #[test]
    fn rejects_other_tags() {
        assert_eq!(
            DeviceInfo::parse(&hello(b"OFFR\n\nkeep\nremoveBranding\n\0")),
            None
        );
        assert_eq!(
            DeviceInfo::parse(&hello(b"dscv\n\n\nhEX\nmmips\n7.1\n")),
            None
        );
    }

    #[test]
    fn rejects_too_few_fields() {
        assert_eq!(DeviceInfo::parse(&hello(b"DSCV\nLIC1\nKEY1\nRB\n")), None);
    }

    #[test]
    fn payload_roundtrip() {
        let info = DeviceInfo::parse(&hello(b"DSCV\n\n\nhEX\nmmips\n6.40\n")).unwrap();
        assert_eq!(DeviceInfo::parse(&hello(&info.to_payload())), Some(info));
    }

    #[test]
    fn hello_detection_uses_seq_1_ack_0() {
        assert!(DeviceInfo::is_hello(&hello(b"x")));
        let mut f = hello(b"x");
        f.ack = 1;
        assert!(!DeviceInfo::is_hello(&f));
    }
}
