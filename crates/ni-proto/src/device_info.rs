//! Device discovery ("hello") frames.

use crate::frame::{Frame, Mac};

/// Information a device in Netinstall mode broadcasts while waiting for a server.
///
/// Payload layout (newline separated, per pyNetinstall `InterfaceInfo.from_data`):
/// `<tag>\n<license id>\n<license key>\n<model>\n<arch>\n<min os>\n`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub mac: Mac,
    pub tag: String,
    pub license_id: String,
    pub license_key: String,
    pub model: String,
    pub arch: String,
    pub min_os: String,
}

impl DeviceInfo {
    /// A hello frame carries `seq = 1, ack = 0`.
    /// Note: our own first server frame has the same numbers, so callers must
    /// also ignore frames whose `src` is the server's MAC.
    pub fn is_hello(frame: &Frame) -> bool {
        frame.seq == 1 && frame.ack == 0
    }

    /// Parse a hello frame. Returns `None` if it has fewer than 6 fields.
    pub fn parse(frame: &Frame) -> Option<Self> {
        let text = String::from_utf8_lossy(&frame.payload);
        let rows: Vec<&str> = text.split('\n').collect();
        if rows.len() < 6 {
            return None;
        }
        Some(Self {
            mac: frame.src,
            tag: rows[0].to_string(),
            license_id: rows[1].to_string(),
            license_key: rows[2].to_string(),
            model: rows[3].to_string(),
            arch: rows[4].to_string(),
            min_os: rows[5].to_string(),
        })
    }

    /// Encode as hello payload (used by the device simulator).
    pub fn to_payload(&self) -> Vec<u8> {
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n",
            self.tag, self.license_id, self.license_key, self.model, self.arch, self.min_os
        )
        .into_bytes()
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
