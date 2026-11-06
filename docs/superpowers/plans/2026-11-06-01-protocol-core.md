# Plan 1 — Protocol Core (ni-proto + ni-sim) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A pure-Rust, network-free implementation of the MikroTik Netinstall protocol (server side) plus a simulated device, proven end-to-end by in-memory tests.

**Architecture:** Cargo workspace. `ni-proto` is sans-IO: frames in, frames/events out, time passed in as `Instant` — no sockets, no threads, no async. `ni-sim` is the device side of the same protocol (also sans-IO) plus a harness that wires both together with a virtual clock and optional packet-loss injection. Later plans put real sockets (Plan 2) and orchestration (Plan 4) around these.

**Tech Stack:** Rust stable (via rustup, edition 2021), `bytes`, `thiserror`, `proptest` (dev).

**Spec:** `docs/superpowers/specs/2026-11-06-mac-netinstall-design.md`
**Roadmap:** `docs/superpowers/plans/2026-11-06-00-roadmap.md`

## Protocol background (read first)

Source of truth: pyNetinstall (`pynetinstall/flash.py`, `network.py`, `interface.py`, GPL-3.0, https://github.com/rfdrake/pynetinstall), verified on real hardware by its authors.

- UDP port 5000, both directions, sent to broadcast `255.255.255.255:5000`.
- Frame = 20-byte header + payload. Header (little-endian): src MAC (6), dst MAC (6), reserved `0` (u16), payload length (u16), `seq` (u16 @16), `ack` (u16 @18).
- A device in Netinstall mode broadcasts a hello: `seq=1, ack=0`, payload `<tag>\n<lic id>\n<lic key>\n<model>\n<arch>\n<min os>\n`.
- The k-th server frame carries `seq=k, ack=k-1`; the device answers with `seq=k, ack=k`. Frames with other numbers are stale and ignored.
- Session: `OFFR\n\n`→`YACK\n`; empty→`STRT` (formatting done, slow); empty→`RETR`; per package: `FILE\n<name>\n<size>\n` (some devices don't answer), 1024-byte chunks each answered (last one with `RETR`), empty spacer; optional config script as `autorun.scr`; empty; `FILE\n`→`WTRM`; `TERM\n` (no answer, device reboots).
- Devices periodically re-send their last reply.

## File structure

```
Cargo.toml                         # workspace
rust-toolchain.toml                # pins rustup stable + rustfmt/clippy
.gitignore
LICENSE                            # GPL-3.0 text
NOTICE                             # credits (pyNetinstall, merlinthemagic)
README.md
crates/ni-proto/Cargo.toml
crates/ni-proto/src/lib.rs         # module wiring + re-exports
crates/ni-proto/src/frame.rs       # Mac, Frame, encode/decode
crates/ni-proto/src/device_info.rs # hello frame parsing
crates/ni-proto/src/message.rs     # payload constants, FILE header helpers
crates/ni-proto/src/session.rs     # ServerSession state machine
crates/ni-proto/tests/frame_props.rs
crates/ni-sim/Cargo.toml
crates/ni-sim/src/lib.rs           # DeviceSim
crates/ni-sim/src/harness.rs       # in-memory pump with virtual clock
crates/ni-sim/tests/e2e.rs
```

---

### Task 0: Toolchain and workspace bootstrap

**Files:**
- Create: `rust-toolchain.toml`, `Cargo.toml`, `.gitignore`, `LICENSE`, `NOTICE`, `README.md`

- [ ] **Step 1: Make sure rustup's cargo wins over MacPorts' old Rust (1.71)**

Run: `which cargo && cargo --version`
Expected: `/Users/<you>/.cargo/bin/cargo` and a version ≥ 1.80.

If it prints `/opt/local/bin/cargo`, **ask the user** before changing their shell profile. With approval, add this as the last line of `~/.bash_profile` (and `~/.zshrc` if zsh is used), then open a new shell:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

For the current session only: `export PATH="$HOME/.cargo/bin:$PATH"`.

- [ ] **Step 2: Create `rust-toolchain.toml`**

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
targets = ["aarch64-apple-darwin"]
```

- [ ] **Step 3: Create workspace `Cargo.toml` (only ni-proto for now)**

```toml
[workspace]
resolver = "2"
members = ["crates/ni-proto"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "GPL-3.0-only"
rust-version = "1.80"

[workspace.dependencies]
bytes = "1"
thiserror = "2"
proptest = "1"
ni-proto = { path = "crates/ni-proto" }
```

- [ ] **Step 4: Create `.gitignore`**

```gitignore
/target
**/*.rs.bk
.DS_Store
node_modules/
dist/
```

- [ ] **Step 5: Add LICENSE, NOTICE, README**

Run: `curl -fsSL https://www.gnu.org/licenses/gpl-3.0.txt -o LICENSE && head -3 LICENSE`
Expected: first line contains `GNU GENERAL PUBLIC LICENSE`.

`NOTICE`:

```text
mac-netinstall
Copyright (C) 2026 mac-netinstall contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License version 3 (see LICENSE).

The Netinstall protocol implementation is derived from pyNetinstall
(https://github.com/rfdrake/pynetinstall), Copyright 2022-2023 DVT -
Daten-Verarbeitung Tirol GmbH, GPL-3.0, which is based on the reverse
engineering work of merlinthemagic.

MikroTik, RouterOS and RouterBOARD are trademarks of SIA Mikrotikls.
This project is not affiliated with MikroTik. No MikroTik binaries are
distributed with this project; boot images and RouterOS packages are
downloaded from mikrotik.com at runtime by the user.
```

`README.md`:

```markdown
# mac-netinstall

Native macOS (Apple Silicon) Netinstall for MikroTik RouterBOARD devices.

Status: early development. See `docs/superpowers/specs/` for the design.

## Development

Requires Rust via rustup (`rust-toolchain.toml` pins the channel).

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

License: GPL-3.0 (see LICENSE and NOTICE).
```

- [ ] **Step 6: Commit**

```bash
git add rust-toolchain.toml Cargo.toml .gitignore LICENSE NOTICE README.md
git commit -m "chore: bootstrap cargo workspace, license and notice"
```

---

### Task 1: ni-proto crate + frame codec

**Files:**
- Create: `crates/ni-proto/Cargo.toml`, `crates/ni-proto/src/lib.rs`, `crates/ni-proto/src/frame.rs`

- [ ] **Step 1: Create `crates/ni-proto/Cargo.toml`**

```toml
[package]
name = "ni-proto"
description = "Sans-IO MikroTik Netinstall protocol (server side)"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
bytes.workspace = true
thiserror.workspace = true

[dev-dependencies]
proptest.workspace = true
```

- [ ] **Step 2: Create `crates/ni-proto/src/lib.rs` (frame only for now)**

```rust
//! Sans-IO implementation of the MikroTik Netinstall UDP protocol (server side).
//!
//! Protocol knowledge is derived from pyNetinstall
//! (<https://github.com/rfdrake/pynetinstall>, GPL-3.0).

pub mod frame;

pub use frame::{DecodeError, Frame, Mac, ParseMacError, HEADER_LEN, MAX_CHUNK, PORT};
```

- [ ] **Step 3: Write the failing tests** — create `crates/ni-proto/src/frame.rs` containing only the test module:

```rust
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
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test -p ni-proto`
Expected: compile errors such as `cannot find type 'Mac' in this scope`.

- [ ] **Step 5: Implement** — replace `crates/ni-proto/src/frame.rs` with the full file (implementation above, same tests below):

```rust
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
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p ni-proto`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/ni-proto
git commit -m "feat(ni-proto): frame codec and MAC type"
```

---

### Task 2: Device hello parsing

**Files:**
- Create: `crates/ni-proto/src/device_info.rs`
- Modify: `crates/ni-proto/src/lib.rs`

- [ ] **Step 1: Write the failing tests** — create `crates/ni-proto/src/device_info.rs` with only:

```rust
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
```

And update `crates/ni-proto/src/lib.rs`:

```rust
//! Sans-IO implementation of the MikroTik Netinstall UDP protocol (server side).
//!
//! Protocol knowledge is derived from pyNetinstall
//! (<https://github.com/rfdrake/pynetinstall>, GPL-3.0).

pub mod device_info;
pub mod frame;

pub use device_info::DeviceInfo;
pub use frame::{DecodeError, Frame, Mac, ParseMacError, HEADER_LEN, MAX_CHUNK, PORT};
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ni-proto device_info`
Expected: compile error `cannot find type 'DeviceInfo'` / `use of undeclared type`.

- [ ] **Step 3: Implement** — full `crates/ni-proto/src/device_info.rs`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ni-proto`
Expected: `test result: ok. 8 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/ni-proto
git commit -m "feat(ni-proto): parse device hello frames"
```

---

### Task 3: Payload constants and FILE header helpers

**Files:**
- Create: `crates/ni-proto/src/message.rs`
- Modify: `crates/ni-proto/src/lib.rs`

- [ ] **Step 1: Write the failing tests** — create `crates/ni-proto/src/message.rs` with only:

```rust
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
```

Add `pub mod message;` to `crates/ni-proto/src/lib.rs` (after `pub mod frame;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ni-proto message`
Expected: compile error `cannot find function 'file_header'`.

- [ ] **Step 3: Implement** — full `crates/ni-proto/src/message.rs`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ni-proto`
Expected: `test result: ok. 10 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/ni-proto
git commit -m "feat(ni-proto): payload constants and FILE header helpers"
```

---

### Task 4: ServerSession state machine

The core of the protocol. Read the module doc comment in the implementation before starting: it lists the exact step sequence and sequence-number rules.

Design notes:
- All steps are precomputed in `build_steps` (file chunks are cheap `Bytes::slice`s of a shared buffer, so a 15 MB package costs ~15k small structs).
- `Wait` is the only mutable "where are we" state: `Send` (due at an instant, optionally a resend), `Reply` (deadline + retries left), `Finished`.
- Unanswered frames are re-sent `max_retransmits` times with the same `seq`. Optional steps (the `FILE` header) move on after retries instead of failing — a lost header must be retried, otherwise the device ignores the following chunks.
- Any reply starting with `NACK` fails the session with `Rejected`.

**Files:**
- Create: `crates/ni-proto/src/session.rs`
- Modify: `crates/ni-proto/src/lib.rs`

- [ ] **Step 1: Write the failing tests** — create `crates/ni-proto/src/session.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: Mac = Mac([0x02, 0, 0, 0, 0, 0x01]);
    const DEVICE: Mac = Mac([0xaa; 6]);

    fn device() -> DeviceInfo {
        DeviceInfo {
            mac: DEVICE,
            tag: "DSCV".into(),
            license_id: String::new(),
            license_key: String::new(),
            model: "hEX".into(),
            arch: "mmips".into(),
            min_os: "7.0".into(),
        }
    }

    fn plan() -> InstallPlan {
        InstallPlan {
            packages: vec![PlanFile {
                name: "routeros.npk".into(),
                data: Bytes::from(vec![7u8; 10]),
            }],
            config_script: None,
        }
    }

    fn reply(seq: u16, payload: &'static [u8]) -> Frame {
        Frame::new(DEVICE, SERVER, seq, seq, payload)
    }

    #[test]
    fn first_frame_is_offer_with_seq_1_ack_0() {
        let now = Instant::now();
        let mut s = ServerSession::new(SERVER, device(), &plan(), SessionConfig::default(), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (SERVER, DEVICE, 1, 0));
        assert_eq!(&f.payload[..], message::OFFR);
        assert_eq!(s.poll_transmit(now), None, "must wait for reply");
    }

    #[test]
    fn ignores_replies_with_wrong_seq_or_source() {
        let now = Instant::now();
        let mut s = ServerSession::new(SERVER, device(), &plan(), SessionConfig::default(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&Frame::new(DEVICE, SERVER, 1, 0, &b"YACK\n"[..]), now);
        s.handle_frame(
            &Frame::new(Mac([0xbb; 6]), SERVER, 1, 1, &b"YACK\n"[..]),
            now,
        );
        assert_eq!(s.poll_transmit(now), None);
        s.handle_frame(&reply(1, message::YACK), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.seq, f.ack), (2, 1));
        assert!(f.payload.is_empty());
    }

    #[test]
    fn formatting_step_uses_format_timeout() {
        let now = Instant::now();
        let cfg = SessionConfig::default();
        let mut s = ServerSession::new(SERVER, device(), &plan(), cfg.clone(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, message::YACK), now);
        s.poll_transmit(now).unwrap();
        assert_eq!(s.phase(), &Phase::Formatting);
        assert_eq!(s.poll_timeout(), Some(now + cfg.format_timeout));
    }

    #[test]
    fn retransmits_then_fails_with_timeout() {
        let now = Instant::now();
        let cfg = SessionConfig {
            max_retransmits: 2,
            ..SessionConfig::default()
        };
        let mut s = ServerSession::new(SERVER, device(), &plan(), cfg, now);
        let first = s.poll_transmit(now).unwrap();
        for _ in 0..2 {
            let t = s.poll_timeout().unwrap();
            s.handle_timeout(t);
            assert_eq!(
                s.poll_transmit(t),
                Some(first.clone()),
                "same frame re-sent"
            );
        }
        let t = s.poll_timeout().unwrap();
        s.handle_timeout(t);
        assert!(s.is_finished());
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::Timeout { phase: "offering" }))
        );
    }

    #[test]
    fn nack_fails_with_rejected() {
        let now = Instant::now();
        let mut s = ServerSession::new(SERVER, device(), &plan(), SessionConfig::default(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, b"NACK\nno space\n"), now);
        assert!(s.is_finished());
        assert_eq!(s.phase(), &Phase::Failed);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::Rejected { phase: "offering" }))
        );
    }

    #[test]
    fn total_bytes_includes_config_script() {
        let mut p = plan();
        p.config_script = Some(Bytes::from_static(b"/system identity set name=x\n"));
        assert_eq!(p.total_bytes(), 10 + 28);
    }
}
```

Replace `crates/ni-proto/src/lib.rs` with the final version:

```rust
//! Sans-IO implementation of the MikroTik Netinstall UDP protocol (server side).
//!
//! Protocol knowledge is derived from pyNetinstall
//! (<https://github.com/rfdrake/pynetinstall>, GPL-3.0).

pub mod device_info;
pub mod frame;
pub mod message;
pub mod session;

pub use device_info::DeviceInfo;
pub use frame::{DecodeError, Frame, Mac, ParseMacError, HEADER_LEN, MAX_CHUNK, PORT};
pub use session::{Event, FailReason, InstallPlan, Phase, PlanFile, ServerSession, SessionConfig};
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ni-proto session`
Expected: compile errors (`ServerSession`, `InstallPlan`, … not found).

- [ ] **Step 3: Implement** — full `crates/ni-proto/src/session.rs`:

```rust
//! Server-side install session for one device (sans-IO).
//!
//! The session is driven by the caller:
//! - [`ServerSession::poll_transmit`] returns frames to broadcast,
//! - [`ServerSession::handle_frame`] feeds frames received from the device,
//! - [`ServerSession::handle_timeout`] must be called once
//!   [`ServerSession::poll_timeout`] has passed,
//! - [`ServerSession::poll_event`] yields progress/phase events.
//!
//! Step sequence (mirrors pyNetinstall `Flasher.run`):
//! 1. `OFFR` → any reply (normally `YACK`)
//! 2. empty → any reply (`STRT`, device formatted; long timeout)
//! 3. empty → any reply (`RETR`)
//! 4. per package: `FILE` header (reply optional) → chunks (last chunk
//!    must be answered with `RETR`) → empty spacer
//! 5. optional config script as `autorun.scr` (same as a package, no spacer)
//! 6. empty → any reply
//! 7. `FILE\n` → any reply (`WTRM`)
//! 8. `TERM\n` → no reply, session done
//!
//! Sequence numbers: the k-th server frame has `seq = k, ack = k - 1`;
//! the device answers with `seq = k, ack = k`.

use crate::device_info::DeviceInfo;
use crate::frame::{Frame, Mac, MAX_CHUNK};
use crate::message;
use bytes::Bytes;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use thiserror::Error;

/// A file to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFile {
    pub name: String,
    pub data: Bytes,
}

/// What to install on one device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallPlan {
    /// RouterOS packages; the main `routeros` package must be first.
    pub packages: Vec<PlanFile>,
    /// Optional RouterOS script, uploaded as `autorun.scr`.
    pub config_script: Option<Bytes>,
}

impl InstallPlan {
    pub fn total_bytes(&self) -> u64 {
        let pkgs: u64 = self.packages.iter().map(|p| p.data.len() as u64).sum();
        pkgs + self.config_script.as_ref().map_or(0, |c| c.len() as u64)
    }
}

/// Timing parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// Wait for an ordinary reply.
    pub reply_timeout: Duration,
    /// Wait for the device to finish formatting.
    pub format_timeout: Duration,
    /// Wait for replies some devices never send (`FILE` header).
    pub optional_timeout: Duration,
    /// Re-sends of an unanswered frame before failing.
    pub max_retransmits: u32,
    /// Pause between file chunks (pyNetinstall uses 5 ms to avoid state errors).
    pub chunk_delay: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_secs(10),
            format_timeout: Duration::from_secs(120),
            optional_timeout: Duration::from_secs(3),
            max_retransmits: 3,
            chunk_delay: Duration::from_millis(5),
        }
    }
}

/// Coarse session phase, for UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Offering,
    Formatting,
    Preparing,
    Uploading { file: String },
    Finishing,
    Rebooting,
    Done,
    Failed,
}

impl Phase {
    /// Stable machine-readable name (used in error codes and i18n keys).
    pub fn name(&self) -> &'static str {
        match self {
            Phase::Offering => "offering",
            Phase::Formatting => "formatting",
            Phase::Preparing => "preparing",
            Phase::Uploading { .. } => "uploading",
            Phase::Finishing => "finishing",
            Phase::Rebooting => "rebooting",
            Phase::Done => "done",
            Phase::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FailReason {
    #[error("device did not reply during {phase}")]
    Timeout { phase: &'static str },
    #[error("device rejected the request during {phase}")]
    Rejected { phase: &'static str },
}

impl FailReason {
    /// Stable code for UI translation.
    pub fn code(&self) -> &'static str {
        match self {
            FailReason::Timeout { .. } => "timeout",
            FailReason::Rejected { .. } => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PhaseChanged(Phase),
    Progress {
        file: String,
        file_sent: u64,
        file_total: u64,
        bytes_sent: u64,
        bytes_total: u64,
    },
    Done,
    Failed(FailReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Do not wait for a reply.
    None,
    /// Any reply with matching sequence numbers.
    Any,
    /// Like `Any`, but once retransmits are exhausted the session moves on.
    Optional,
    /// Only a reply with exactly this payload.
    Payload(&'static [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeoutKind {
    Reply,
    Format,
    Optional,
}

#[derive(Debug, Clone)]
struct ChunkInfo {
    file: String,
    file_sent: u64,
    file_total: u64,
}

#[derive(Debug, Clone)]
struct Step {
    phase: Phase,
    payload: Bytes,
    expect: Expect,
    timeout: TimeoutKind,
    chunk: Option<ChunkInfo>,
    delay_after: bool,
}

impl Step {
    fn control(phase: Phase, payload: &'static [u8], expect: Expect, timeout: TimeoutKind) -> Self {
        Self {
            phase,
            payload: Bytes::from_static(payload),
            expect,
            timeout,
            chunk: None,
            delay_after: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Wait {
    /// Ready to send the current step (or re-send the last frame) at `at`.
    Send {
        at: Instant,
        resend: Option<u32>,
    },
    /// Waiting for the device to answer the current step.
    Reply {
        deadline: Instant,
        retries_left: u32,
    },
    Finished,
}

/// Install session for one device.
pub struct ServerSession {
    server_mac: Mac,
    device: DeviceInfo,
    cfg: SessionConfig,
    steps: Vec<Step>,
    idx: usize,
    seq: u16,
    wait: Wait,
    phase: Phase,
    last_frame: Option<Frame>,
    bytes_sent: u64,
    bytes_total: u64,
    events: VecDeque<Event>,
}

impl ServerSession {
    pub fn new(
        server_mac: Mac,
        device: DeviceInfo,
        plan: &InstallPlan,
        cfg: SessionConfig,
        now: Instant,
    ) -> Self {
        let mut events = VecDeque::new();
        events.push_back(Event::PhaseChanged(Phase::Offering));
        Self {
            server_mac,
            device,
            cfg,
            steps: build_steps(plan),
            idx: 0,
            seq: 0,
            wait: Wait::Send {
                at: now,
                resend: None,
            },
            phase: Phase::Offering,
            last_frame: None,
            bytes_sent: 0,
            bytes_total: plan.total_bytes(),
            events,
        }
    }

    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.wait, Wait::Finished)
    }

    /// Next frame to send, if any is due at `now`.
    pub fn poll_transmit(&mut self, now: Instant) -> Option<Frame> {
        let Wait::Send { at, resend } = self.wait else {
            return None;
        };
        if now < at {
            return None;
        }
        if let Some(retries_left) = resend {
            let frame = self.last_frame.clone()?;
            let timeout = self.timeout(self.steps[self.idx].timeout);
            self.wait = Wait::Reply {
                deadline: now + timeout,
                retries_left,
            };
            return Some(frame);
        }
        let step = self.steps.get(self.idx)?.clone();
        self.set_phase(step.phase.clone());
        self.seq = self.seq.wrapping_add(1);
        let frame = Frame::new(
            self.server_mac,
            self.device.mac,
            self.seq,
            self.seq.wrapping_sub(1),
            step.payload.clone(),
        );
        self.last_frame = Some(frame.clone());
        if step.expect == Expect::None {
            self.advance(now, false);
        } else {
            self.wait = Wait::Reply {
                deadline: now + self.timeout(step.timeout),
                retries_left: self.cfg.max_retransmits,
            };
        }
        Some(frame)
    }

    /// Feed a frame received on the wire. Unrelated frames are ignored.
    pub fn handle_frame(&mut self, frame: &Frame, now: Instant) {
        if !matches!(self.wait, Wait::Reply { .. }) {
            return;
        }
        if frame.src != self.device.mac || frame.seq != self.seq || frame.ack != self.seq {
            return;
        }
        if frame.payload.starts_with(message::NACK) {
            self.fail(FailReason::Rejected {
                phase: self.phase.name(),
            });
            return;
        }
        let step = &self.steps[self.idx];
        if let Expect::Payload(expected) = step.expect {
            if frame.payload.as_ref() != expected {
                return;
            }
        }
        let delay = step.delay_after;
        if let Some(chunk) = step.chunk.clone() {
            self.bytes_sent += step.payload.len() as u64;
            self.events.push_back(Event::Progress {
                file: chunk.file,
                file_sent: chunk.file_sent,
                file_total: chunk.file_total,
                bytes_sent: self.bytes_sent,
                bytes_total: self.bytes_total,
            });
        }
        self.advance(now, delay);
    }

    /// Call when `now >= poll_timeout()`.
    pub fn handle_timeout(&mut self, now: Instant) {
        let Wait::Reply {
            deadline,
            retries_left,
        } = self.wait
        else {
            return;
        };
        if now < deadline {
            return;
        }
        if retries_left > 0 {
            self.wait = Wait::Send {
                at: now,
                resend: Some(retries_left - 1),
            };
        } else if self.steps[self.idx].expect == Expect::Optional {
            // Some devices never answer this step; move on.
            self.advance(now, false);
        } else {
            self.fail(FailReason::Timeout {
                phase: self.phase.name(),
            });
        }
    }

    /// When the session next needs attention (send or timeout).
    pub fn poll_timeout(&self) -> Option<Instant> {
        match self.wait {
            Wait::Send { at, .. } => Some(at),
            Wait::Reply { deadline, .. } => Some(deadline),
            Wait::Finished => None,
        }
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    fn timeout(&self, kind: TimeoutKind) -> Duration {
        match kind {
            TimeoutKind::Reply => self.cfg.reply_timeout,
            TimeoutKind::Format => self.cfg.format_timeout,
            TimeoutKind::Optional => self.cfg.optional_timeout,
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        if self.phase != phase {
            self.phase = phase.clone();
            self.events.push_back(Event::PhaseChanged(phase));
        }
    }

    fn advance(&mut self, now: Instant, delay: bool) {
        self.idx += 1;
        if self.idx >= self.steps.len() {
            self.wait = Wait::Finished;
            self.set_phase(Phase::Done);
            self.events.push_back(Event::Done);
        } else {
            let at = if delay {
                now + self.cfg.chunk_delay
            } else {
                now
            };
            self.wait = Wait::Send { at, resend: None };
        }
    }

    fn fail(&mut self, reason: FailReason) {
        self.wait = Wait::Finished;
        self.set_phase(Phase::Failed);
        self.events.push_back(Event::Failed(reason));
    }
}

fn build_steps(plan: &InstallPlan) -> Vec<Step> {
    use Expect::Any;
    use TimeoutKind::{Format, Reply};

    let mut steps = vec![
        Step::control(Phase::Offering, message::OFFR, Any, Reply),
        Step::control(Phase::Formatting, b"", Any, Format),
        Step::control(Phase::Preparing, b"", Any, Reply),
    ];
    for pkg in &plan.packages {
        push_file(&mut steps, &pkg.name, &pkg.data);
        let phase = Phase::Uploading {
            file: pkg.name.clone(),
        };
        steps.push(Step::control(phase, b"", Any, Reply));
    }
    if let Some(script) = &plan.config_script {
        push_file(&mut steps, message::CONFIG_SCRIPT_NAME, script);
    }
    steps.push(Step::control(Phase::Finishing, b"", Any, Reply));
    steps.push(Step::control(
        Phase::Finishing,
        message::FILE_END,
        Any,
        Reply,
    ));
    steps.push(Step::control(
        Phase::Rebooting,
        message::TERM,
        Expect::None,
        Reply,
    ));
    steps
}

fn push_file(steps: &mut Vec<Step>, name: &str, data: &Bytes) {
    let phase = Phase::Uploading {
        file: name.to_string(),
    };
    let total = data.len() as u64;
    steps.push(Step {
        phase: phase.clone(),
        payload: Bytes::from(message::file_header(name, total)),
        expect: Expect::Optional,
        timeout: TimeoutKind::Optional,
        chunk: None,
        delay_after: false,
    });
    let chunks: Vec<Bytes> = if data.is_empty() {
        vec![Bytes::new()]
    } else {
        (0..data.len())
            .step_by(MAX_CHUNK)
            .map(|o| data.slice(o..(o + MAX_CHUNK).min(data.len())))
            .collect()
    };
    let last = chunks.len() - 1;
    let mut sent = 0u64;
    for (i, chunk) in chunks.into_iter().enumerate() {
        sent += chunk.len() as u64;
        let is_last = i == last;
        steps.push(Step {
            phase: phase.clone(),
            payload: chunk,
            expect: if is_last {
                Expect::Payload(message::RETR)
            } else {
                Expect::Any
            },
            timeout: TimeoutKind::Reply,
            chunk: Some(ChunkInfo {
                file: name.to_string(),
                file_sent: sent,
                file_total: total,
            }),
            delay_after: !is_last,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: Mac = Mac([0x02, 0, 0, 0, 0, 0x01]);
    const DEVICE: Mac = Mac([0xaa; 6]);

    fn device() -> DeviceInfo {
        DeviceInfo {
            mac: DEVICE,
            tag: "DSCV".into(),
            license_id: String::new(),
            license_key: String::new(),
            model: "hEX".into(),
            arch: "mmips".into(),
            min_os: "7.0".into(),
        }
    }

    fn plan() -> InstallPlan {
        InstallPlan {
            packages: vec![PlanFile {
                name: "routeros.npk".into(),
                data: Bytes::from(vec![7u8; 10]),
            }],
            config_script: None,
        }
    }

    fn reply(seq: u16, payload: &'static [u8]) -> Frame {
        Frame::new(DEVICE, SERVER, seq, seq, payload)
    }

    #[test]
    fn first_frame_is_offer_with_seq_1_ack_0() {
        let now = Instant::now();
        let mut s = ServerSession::new(SERVER, device(), &plan(), SessionConfig::default(), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (SERVER, DEVICE, 1, 0));
        assert_eq!(&f.payload[..], message::OFFR);
        assert_eq!(s.poll_transmit(now), None, "must wait for reply");
    }

    #[test]
    fn ignores_replies_with_wrong_seq_or_source() {
        let now = Instant::now();
        let mut s = ServerSession::new(SERVER, device(), &plan(), SessionConfig::default(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&Frame::new(DEVICE, SERVER, 1, 0, &b"YACK\n"[..]), now);
        s.handle_frame(
            &Frame::new(Mac([0xbb; 6]), SERVER, 1, 1, &b"YACK\n"[..]),
            now,
        );
        assert_eq!(s.poll_transmit(now), None);
        s.handle_frame(&reply(1, message::YACK), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.seq, f.ack), (2, 1));
        assert!(f.payload.is_empty());
    }

    #[test]
    fn formatting_step_uses_format_timeout() {
        let now = Instant::now();
        let cfg = SessionConfig::default();
        let mut s = ServerSession::new(SERVER, device(), &plan(), cfg.clone(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, message::YACK), now);
        s.poll_transmit(now).unwrap();
        assert_eq!(s.phase(), &Phase::Formatting);
        assert_eq!(s.poll_timeout(), Some(now + cfg.format_timeout));
    }

    #[test]
    fn retransmits_then_fails_with_timeout() {
        let now = Instant::now();
        let cfg = SessionConfig {
            max_retransmits: 2,
            ..SessionConfig::default()
        };
        let mut s = ServerSession::new(SERVER, device(), &plan(), cfg, now);
        let first = s.poll_transmit(now).unwrap();
        for _ in 0..2 {
            let t = s.poll_timeout().unwrap();
            s.handle_timeout(t);
            assert_eq!(
                s.poll_transmit(t),
                Some(first.clone()),
                "same frame re-sent"
            );
        }
        let t = s.poll_timeout().unwrap();
        s.handle_timeout(t);
        assert!(s.is_finished());
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::Timeout { phase: "offering" }))
        );
    }

    #[test]
    fn nack_fails_with_rejected() {
        let now = Instant::now();
        let mut s = ServerSession::new(SERVER, device(), &plan(), SessionConfig::default(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, b"NACK\nno space\n"), now);
        assert!(s.is_finished());
        assert_eq!(s.phase(), &Phase::Failed);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::Rejected { phase: "offering" }))
        );
    }

    #[test]
    fn total_bytes_includes_config_script() {
        let mut p = plan();
        p.config_script = Some(Bytes::from_static(b"/system identity set name=x\n"));
        assert_eq!(p.total_bytes(), 10 + 28);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ni-proto`
Expected: `test result: ok. 16 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/ni-proto
git commit -m "feat(ni-proto): server install session state machine"
```

---

### Task 5: Property tests for the codec

**Files:**
- Create: `crates/ni-proto/tests/frame_props.rs`

- [ ] **Step 1: Write the property tests**

```rust
use ni_proto::{Frame, Mac, MAX_CHUNK};
use proptest::prelude::*;

fn arb_mac() -> impl Strategy<Value = Mac> {
    any::<[u8; 6]>().prop_map(Mac)
}

proptest! {
    #[test]
    fn encode_decode_roundtrip(
        src in arb_mac(),
        dst in arb_mac(),
        seq in any::<u16>(),
        ack in any::<u16>(),
        payload in proptest::collection::vec(any::<u8>(), 0..=MAX_CHUNK),
    ) {
        let frame = Frame::new(src, dst, seq, ack, payload);
        prop_assert_eq!(Frame::decode(&frame.encode()).unwrap(), frame);
    }

    #[test]
    fn decode_never_panics(buf in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = Frame::decode(&buf);
    }
}
```

- [ ] **Step 2: Run them**

Run: `cargo test -p ni-proto --test frame_props`
Expected: `test result: ok. 2 passed`. (They pass immediately because the codec exists; they guard against regressions. If one fails, proptest prints the minimal failing input — fix the codec, not the test.)

- [ ] **Step 3: Commit**

```bash
git add crates/ni-proto/tests
git commit -m "test(ni-proto): codec property tests"
```

---

### Task 6: Device simulator and in-memory harness

**Files:**
- Create: `crates/ni-sim/Cargo.toml`, `crates/ni-sim/src/lib.rs`, `crates/ni-sim/src/harness.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Add ni-sim to the workspace** — in root `Cargo.toml` change the members line to:

```toml
members = ["crates/ni-proto", "crates/ni-sim"]
```

- [ ] **Step 2: Create `crates/ni-sim/Cargo.toml`**

```toml
[package]
name = "ni-sim"
description = "Simulated MikroTik device (Netinstall client side) for tests"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
bytes.workspace = true
ni-proto.workspace = true
```

- [ ] **Step 3: Create `crates/ni-sim/src/lib.rs`**

```rust
//! Simulated MikroTik device in Netinstall mode (sans-IO).
//!
//! Behaviour mirrors what pyNetinstall expects from real hardware:
//! broadcasts hello frames until offered, answers each server frame with
//! `seq = ack = <server seq>`, and periodically re-sends its last reply
//! (real devices do this too).

pub mod harness;

use bytes::Bytes;
use ni_proto::{message, DeviceInfo, Frame, Mac};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct SimConfig {
    pub info: DeviceInfo,
    pub hello_interval: Duration,
    pub resend_interval: Duration,
}

impl SimConfig {
    pub fn new(mac: Mac, model: &str, arch: &str) -> Self {
        Self {
            info: DeviceInfo {
                mac,
                tag: "DSCV".into(),
                license_id: "SIM-0000".into(),
                license_key: String::new(),
                model: model.into(),
                arch: arch.into(),
                min_os: "7.0".into(),
            },
            hello_interval: Duration::from_secs(1),
            resend_interval: Duration::from_millis(500),
        }
    }
}

/// A file the simulated device received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedFile {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Discovering,
    Offered,
    Formatted,
    Ready,
    Receiving {
        name: String,
        size: u64,
        data: Vec<u8>,
    },
    Finished,
    Terminated,
}

pub struct DeviceSim {
    cfg: SimConfig,
    state: State,
    last_seq: u16,
    last_reply: Option<Frame>,
    tx: VecDeque<Frame>,
    next_timer: Option<Instant>,
    received: Vec<ReceivedFile>,
}

impl DeviceSim {
    pub fn new(cfg: SimConfig, now: Instant) -> Self {
        Self {
            cfg,
            state: State::Discovering,
            last_seq: 0,
            last_reply: None,
            tx: VecDeque::new(),
            next_timer: Some(now),
            received: Vec::new(),
        }
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.cfg.info
    }

    pub fn received_files(&self) -> &[ReceivedFile] {
        &self.received
    }

    pub fn is_terminated(&self) -> bool {
        self.state == State::Terminated
    }

    pub fn poll_transmit(&mut self) -> Option<Frame> {
        self.tx.pop_front()
    }

    pub fn poll_timeout(&self) -> Option<Instant> {
        self.next_timer
    }

    pub fn handle_timeout(&mut self, now: Instant) {
        match self.next_timer {
            Some(t) if t <= now => {}
            _ => return,
        }
        match self.state {
            State::Terminated => self.next_timer = None,
            State::Discovering => {
                let hello = Frame::new(
                    self.cfg.info.mac,
                    Mac::BROADCAST,
                    1,
                    0,
                    self.cfg.info.to_payload(),
                );
                self.tx.push_back(hello);
                self.next_timer = Some(now + self.cfg.hello_interval);
            }
            _ => {
                if let Some(reply) = &self.last_reply {
                    self.tx.push_back(reply.clone());
                }
                self.next_timer = Some(now + self.cfg.resend_interval);
            }
        }
    }

    pub fn handle_frame(&mut self, frame: &Frame, now: Instant) {
        let me = self.cfg.info.mac;
        if self.state == State::Terminated || frame.dst != me || frame.src == me {
            return;
        }
        if self.last_seq != 0 && frame.seq == self.last_seq {
            // Server re-sent its last frame: repeat our answer.
            if let Some(reply) = &self.last_reply {
                self.tx.push_back(reply.clone());
            }
            return;
        }
        if frame.seq != self.last_seq.wrapping_add(1) {
            return;
        }
        self.last_seq = frame.seq;
        for payload in self.process(&frame.payload) {
            let reply = Frame::new(
                me,
                frame.src,
                frame.seq,
                frame.seq,
                Bytes::from_static(payload),
            );
            self.last_reply = Some(reply.clone());
            self.tx.push_back(reply);
        }
        self.next_timer = if self.state == State::Terminated {
            None
        } else {
            Some(now + self.cfg.resend_interval)
        };
    }

    fn process(&mut self, payload: &[u8]) -> Vec<&'static [u8]> {
        use message::{NACK, RETR, STRT, WTRM, YACK};
        let state = std::mem::replace(&mut self.state, State::Discovering);
        let (next, replies) = match state {
            State::Discovering if payload.starts_with(b"OFFR") => (State::Offered, vec![YACK]),
            State::Offered if payload.is_empty() => (State::Formatted, vec![STRT]),
            State::Formatted if payload.is_empty() => (State::Ready, vec![RETR]),
            State::Ready if payload == message::FILE_END => (State::Finished, vec![WTRM]),
            State::Ready if payload.is_empty() => (State::Ready, vec![RETR]),
            State::Ready => match message::parse_file_header(payload) {
                Some((name, size)) => (
                    State::Receiving {
                        name,
                        size,
                        data: Vec::new(),
                    },
                    vec![RETR],
                ),
                None => (State::Ready, vec![NACK]),
            },
            State::Receiving {
                name,
                size,
                mut data,
            } => {
                data.extend_from_slice(payload);
                if data.len() as u64 >= size {
                    self.received.push(ReceivedFile { name, data });
                    // pyNetinstall reads twice after the last chunk.
                    (State::Ready, vec![RETR, RETR])
                } else {
                    (State::Receiving { name, size, data }, vec![RETR])
                }
            }
            State::Finished if payload.starts_with(b"TERM") => (State::Terminated, vec![]),
            other => (other, vec![NACK]),
        };
        self.state = next;
        replies
    }
}
```

- [ ] **Step 4: Create `crates/ni-sim/src/harness.rs`**

```rust
//! In-memory driver connecting a [`ServerSession`] and a [`DeviceSim`]
//! with a virtual clock. Frames pass through encode/decode on the way.

use crate::DeviceSim;
use ni_proto::{DeviceInfo, Frame, ServerSession};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToDevice,
    ToServer,
}

#[derive(Debug, Clone, Copy)]
pub struct PumpResult {
    /// Virtual time when pumping stopped.
    pub now: Instant,
}

/// Fire the simulator's first hello and parse it.
pub fn discover(sim: &mut DeviceSim, now: Instant) -> DeviceInfo {
    sim.handle_timeout(now);
    let hello = sim.poll_transmit().expect("simulator emits hello");
    DeviceInfo::parse(&hello).expect("valid hello payload")
}

/// Run until the server session finishes or `limit` virtual time passes.
/// `deliver` decides whether each frame reaches the other side.
pub fn pump(
    server: &mut ServerSession,
    sim: &mut DeviceSim,
    mut now: Instant,
    limit: Duration,
    mut deliver: impl FnMut(Direction, &Frame) -> bool,
) -> PumpResult {
    let end = now + limit;
    loop {
        let mut progressed = false;
        while let Some(f) = server.poll_transmit(now) {
            progressed = true;
            let wire = Frame::decode(&f.encode()).expect("decodable");
            if deliver(Direction::ToDevice, &wire) {
                sim.handle_frame(&wire, now);
            }
        }
        while let Some(f) = sim.poll_transmit() {
            progressed = true;
            let wire = Frame::decode(&f.encode()).expect("decodable");
            if deliver(Direction::ToServer, &wire) {
                server.handle_frame(&wire, now);
            }
        }
        if server.is_finished() {
            return PumpResult { now };
        }
        if progressed {
            continue;
        }
        let next = [server.poll_timeout(), sim.poll_timeout()]
            .into_iter()
            .flatten()
            .min();
        match next {
            Some(t) if t <= end => {
                now = now.max(t);
                server.handle_timeout(now);
                sim.handle_timeout(now);
            }
            _ => return PumpResult { now },
        }
    }
}
```

- [ ] **Step 5: Verify it compiles cleanly**

Run: `cargo clippy -p ni-sim --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/ni-sim
git commit -m "feat(ni-sim): simulated netinstall device and in-memory harness"
```

---

### Task 7: End-to-end tests (server ⇄ simulated device)

These tests are the acceptance criteria for this plan: full installs, multiple packages including edge sizes (exactly 2×1024, 1 byte, empty), loss in both directions, a device that never answers `FILE` headers, and a silent device.

**Files:**
- Create: `crates/ni-sim/tests/e2e.rs`

- [ ] **Step 1: Write the tests**

```rust
use bytes::Bytes;
use ni_proto::{
    message, Event, FailReason, InstallPlan, Mac, Phase, PlanFile, ServerSession, SessionConfig,
};
use ni_sim::harness::{discover, pump, Direction};
use ni_sim::{DeviceSim, ReceivedFile, SimConfig};
use std::time::{Duration, Instant};

const SERVER: Mac = Mac([0x02, 0, 0, 0, 0, 0x01]);
const DEVICE: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x01]);

fn pattern(len: usize, seed: u8) -> Bytes {
    Bytes::from(
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect::<Vec<u8>>(),
    )
}

fn setup(sim_cfg: SimConfig, plan: &InstallPlan) -> (ServerSession, DeviceSim, Instant) {
    let now = Instant::now();
    let mut sim = DeviceSim::new(sim_cfg, now);
    let info = discover(&mut sim, now);
    assert_eq!(info.mac, DEVICE);
    let server = ServerSession::new(SERVER, info, plan, SessionConfig::default(), now);
    (server, sim, now)
}

fn events(server: &mut ServerSession) -> Vec<Event> {
    std::iter::from_fn(|| server.poll_event()).collect()
}

#[test]
fn installs_package_and_config_script() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros-7.16-mmips.npk".into(),
            data: pattern(3000, 1),
        }],
        config_script: Some(Bytes::from_static(b"/system identity set name=lab\n")),
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);

    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |_, _| true,
    );

    assert!(sim.is_terminated());
    assert_eq!(
        sim.received_files(),
        &[
            ReceivedFile {
                name: "routeros-7.16-mmips.npk".into(),
                data: pattern(3000, 1).to_vec()
            },
            ReceivedFile {
                name: message::CONFIG_SCRIPT_NAME.into(),
                data: b"/system identity set name=lab\n".to_vec()
            },
        ]
    );
    let ev = events(&mut server);
    assert_eq!(ev.last(), Some(&Event::Done));
    let last_progress = ev.iter().rev().find_map(|e| match e {
        Event::Progress {
            bytes_sent,
            bytes_total,
            ..
        } => Some((*bytes_sent, *bytes_total)),
        _ => None,
    });
    assert_eq!(
        last_progress,
        Some((plan.total_bytes(), plan.total_bytes()))
    );
    let phases: Vec<&str> = ev
        .iter()
        .filter_map(|e| match e {
            Event::PhaseChanged(p) => Some(p.name()),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        [
            "offering",
            "formatting",
            "preparing",
            "uploading",
            "uploading",
            "finishing",
            "rebooting",
            "done"
        ]
    );
}

#[test]
fn installs_multiple_packages_including_empty_and_exact_chunk_sizes() {
    let plan = InstallPlan {
        packages: vec![
            PlanFile {
                name: "routeros.npk".into(),
                data: pattern(2048, 2),
            },
            PlanFile {
                name: "wifi-qcom.npk".into(),
                data: pattern(1, 3),
            },
            PlanFile {
                name: "empty.npk".into(),
                data: Bytes::new(),
            },
        ],
        config_script: None,
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hAP ax2", "arm64"), &plan);

    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |_, _| true,
    );

    let names: Vec<&str> = sim
        .received_files()
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(names, ["routeros.npk", "wifi-qcom.npk", "empty.npk"]);
    assert_eq!(sim.received_files()[0].data, pattern(2048, 2).to_vec());
    assert!(sim.received_files()[2].data.is_empty());
    assert!(sim.is_terminated());
    assert_eq!(events(&mut server).last(), Some(&Event::Done));
}

#[test]
fn recovers_from_lost_device_replies() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: pattern(5000, 4),
        }],
        config_script: None,
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "RB951", "mipsbe"), &plan);
    let mut to_server = 0;

    // Drop every 3rd device frame.
    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(300),
        |dir, _| {
            if dir == Direction::ToServer {
                to_server += 1;
                to_server % 3 != 0
            } else {
                true
            }
        },
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, pattern(5000, 4).to_vec());
}

#[test]
fn recovers_from_lost_server_frames() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: pattern(5000, 5),
        }],
        config_script: None,
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "RB951", "mipsbe"), &plan);
    let mut to_device = 0;

    // Drop every 4th server frame, except TERM (which is never retransmitted).
    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(300),
        |dir, f| {
            if dir == Direction::ToDevice && &f.payload[..] != message::TERM {
                to_device += 1;
                to_device % 4 != 0
            } else {
                true
            }
        },
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, pattern(5000, 5).to_vec());
    assert!(sim.is_terminated());
}

#[test]
fn file_header_without_reply_is_tolerated() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: pattern(100, 6),
        }],
        config_script: None,
    };
    let mut cfg = SimConfig::new(DEVICE, "RB951", "mipsbe");
    cfg.resend_interval = Duration::from_secs(3600); // no device-side resends
    let (mut server, mut sim, now) = setup(cfg, &plan);
    let mut last_to_device = Bytes::new();

    // Drop the device's answer to every FILE header.
    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, f| match dir {
            Direction::ToDevice => {
                last_to_device = f.payload.clone();
                true
            }
            Direction::ToServer => message::parse_file_header(&last_to_device).is_none(),
        },
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, pattern(100, 6).to_vec());
}

#[test]
fn silent_device_fails_with_timeout_in_offering() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: pattern(10, 7),
        }],
        config_script: None,
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "RB951", "mipsbe"), &plan);

    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(600),
        |dir, _| dir == Direction::ToDevice,
    );

    assert_eq!(server.phase(), &Phase::Failed);
    assert_eq!(
        events(&mut server).last(),
        Some(&Event::Failed(FailReason::Timeout { phase: "offering" }))
    );
}
```

- [ ] **Step 2: Run them**

Run: `cargo test -p ni-sim --test e2e`
Expected: `test result: ok. 6 passed`.

If `recovers_from_lost_server_frames` fails with `Timeout { phase: "uploading" }`, check that `handle_timeout` in `session.rs` re-sends Optional steps before moving on (retries first, then `advance`) — this exact bug was found while prototyping this plan.

- [ ] **Step 3: Commit**

```bash
git add crates/ni-sim/tests
git commit -m "test(ni-sim): end-to-end install scenarios incl. packet loss"
```

---

### Task 8: Final verification

- [ ] **Step 1: Format check**

Run: `cargo fmt --all --check`
Expected: no output, exit code 0. If it fails, run `cargo fmt --all` and commit.

- [ ] **Step 2: Lint**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace`
Expected: all suites `ok` — 16 (ni-proto unit) + 2 (frame_props) + 6 (e2e) = 24 tests passing.

- [ ] **Step 4: Commit any formatting fixes**

```bash
git status --short   # should be clean; if not:
git commit -am "style: cargo fmt"
```
