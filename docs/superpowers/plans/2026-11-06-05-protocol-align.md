# Plan 5 — Protocol alignment with the official netinstall-cli + configuration modes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make our Netinstall server speak exactly like MikroTik's official `netinstall-cli` 7.25.1 (proved by golden tests against its captured frames and by running our device simulator against the real tool in a VM), and add its configuration options — keep / default / empty / script, mode script, remove branding, license key, after-install action — end to end: protocol, simulator, engine profiles, CLI flags and the GUI's profile editor. Also fixes the cache view listing empty versions as "RouterOS x.y.z – 0 B".

**Architecture:** `ni-proto` gets the official payloads (`message::offer`, `file_header` and `term` with their trailing NUL) and a rewritten step machine: offer (re-offered after a `NACK`), format, prepare, per file header → 1024-byte chunks (one reply each, none for an empty file) → empty poll, then polls until `WTRM` (falling back to pyNetinstall's `FILE\n`), then `TERM`. Frame headers mirror the device's MAC fields, so the server's own MAC disappears from the wire and from the engine. `InstallPlan` carries a `DeviceConfig` and the options and orders the extra files (`autorun.scr`, `mode.scr`) like the official tool. `ni-sim` becomes the device the official server expects (the spike's `sim.py`). Sixteen compact fixtures derived from the spike's captures drive two golden tests: our session against the official server's frames, and our simulator against the official device replies. `ni-engine` profiles grow the options (old profile files still load), the CLI gets `--config / --script / --mode-script / --remove-branding / --license-key / --after`, and the app gets a license-key picker (read in Rust), the options in the profile editor and the configuration per device row.

**Tech Stack:** Rust stable (rustup, edition 2021, MSRV 1.88; the app crate 1.90), tokio, serde, clap 4 (`ValueEnum`), Tauri 2.12 + `tauri-plugin-dialog` (Rust API), Svelte 5 + TypeScript, Vitest + Testing Library. No new dependency. The VM check additionally uses the `x86_64-unknown-linux-musl` std from rustup and `rust-lld`.

**Ground truth:** `docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md` (official `netinstall-cli` 7.25.1 captured in an Ubuntu VM against a simulated device, spike 5).
**Spec:** `docs/superpowers/specs/2026-11-06-mac-netinstall-design.md`
**Roadmap:** `docs/superpowers/plans/2026-11-06-00-roadmap.md` — row 5 "Protocol alignment + config modes"; the last task records the facts, the VM run and the new hardware checks.
**Builds on:** Plans 1–4 and 6 (everything merged on `main`); branch `feat/protocol-align`.

## Background (read first)

**Toolchain:** `export PATH="$HOME/.cargo/bin:$PATH"` so rustup's cargo comes first (see Plan 1 Task 0). Frontend commands run in `app/`; install its packages once with `npm ci` (never `npm install`: the lock file is the source of truth). Task 2 does that if `app/node_modules` is missing.

**The official flow** (research doc §2, here for e05 `-s user.rsc`; `S` = server, `D` = device):

```text
D  [dev][ff..ff] seq1 ack0  DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n
S  [ff..ff][dev] seq1 ack0  OFFR\n\n\nremoveBranding\n\0        D YACK\n
S  [00..00][dev] seq2 ack0  ""                                    D STRT
S                seq3 ack1  ""                                    D RETR
S                seq4 ack2  FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0   D RETR
S                seq5…13661  1024-byte chunks (last 437)          D RETR each
S                seq13662   ""   (poll)                           D RETR
S                seq13663   FILE\nautorun.scr\n91\n\0             D RETR
S                seq13664   <91 bytes>                            D RETR
S                seq13665   ""   (poll)                           D RETR
S                seq13666   ""   (poll)                           D WTRM
S                seq13667   TERM\n\0                              (no reply)
```

Server `[0:6]` is `[6:12]` of the device frame it answers, `[6:12]` the device; ack is seq − 2. Configuration options (research doc §0):

| Flags | OFFR | Files after the packages | TERM |
|---|---|---|---|
| none (keep) | `OFFR\n\nkeep\nremoveBranding\n\0` | – | `TERM\n\0` |
| `-r` | `OFFR\n\n\nremoveBranding\n\0` | – | |
| `-e` | as `-r` | `autorun.scr` (0 B, header only) | |
| `-s f` / `-r -s f` | as `-r` | `autorun.scr` = f | |
| `-r -sm m` | as `-r` | `mode.scr` = m | |
| `-e -sm m` | as `-r` | `mode.scr`, then empty `autorun.scr` | |
| `-r -s f -sm m` | as `-r` | `autorun.scr`, then `mode.scr` | |
| `-b` | `OFFR\n\nkeep\n\n\0` (field 3 empty) | – | |
| `-k file` | `OFFR\n<file without \n>\nkeep\nremoveBranding\n\0` | – | |
| `--reboot` / `--shutdown` | unchanged | – | `TERM\nInstallation successful\n\0` / `TERM\nshutdown\n\0` |

**Testing without hardware:** as before — sans-IO unit tests, `ni_sim::harness` (session ↔ simulator on a virtual clock), engine tests on `lo0` with `ni_sim::lan::SimLan`, the app's `tests/bridge.rs` through `AppCore`, the frontend with `tests/mock.ts`. New: `crates/ni-proto/tests/official/mod.rs` loads the captured runs; `crates/ni-sim/tests/official_replay.rs` includes it with `#[path]`.

## Design notes and deliberate choices

- **Follow the captures, bound what the official tool waits for forever.** A `NACK` to the offer re-offers the same frame after `offer_retry` (1 s), at most `max_retransmits` times, then `flash_rejected` (official: forever). After the last file the session polls every `poll_interval` (250 ms), at most `finish_polls` (20) times, then sends pyNetinstall's `FILE\n` and `TERM` (official: polls forever) — so a pyNetinstall-era device that never says `WTRM` still finishes. `WTRM` before the last file is a loud `flash_ended_early` (`remaining`): the official tool skips the remaining files silently and reports success (confirmed in the VM). Same-seq retransmits stay (unverified on hardware, as before).
- **No server MAC.** Frames mirror the device's MAC fields like the official tool's, so `no_mac`, `EngineConfig::server_mac`, `NetPlan::server_mac` and the CLI's hidden `--server-mac` go away; the engine drops its own looped-back broadcasts by source address (server IP + its port). A looped-back frame could never pass a session anyway: a reply needs `seq == ack`, ours have `ack = seq − 2`. The interface MAC is still listed for display.
- **Deviation: a mode script also goes with a script.** The fixed decision said "only with Default/Empty"; the captures show `-r -s f -sm m` is accepted (autorun.scr, then mode.scr), so only `keep` refuses it (`mode_script_with_keep`, like the official CLI).
- **Default configuration stays `default`.** The official tool keeps the configuration by default; our profiles and CLI keep defaulting to RouterOS's default configuration (what Plans 4–6 did), so existing profiles and scripts behave as before. `keep` is one choice away.
- **Profile migration without a schema version.** Old `{"mode": "default"}` is still the default configuration, `{"mode": "custom_script", …}` is read as `script` (`#[serde(alias)]`), the new fields take defaults; the next save writes the new form.
- **License key:** the key file's contents with `\n` and `\r` removed (the official tool removes `\n`), printable ASCII, ≤ 4096 bytes. The app reads the file in Rust (`pick_license_key`, at most 64 KiB) and gives only the key to the window.
- **`removeBranding` polarity** is as captured: the field reads `removeBranding` when `-b` is *absent*. "Remove branding" in the UI and `--remove-branding` are `-b`. What a device does with it is a hardware check.
- **Hello:** field 0 must be `DSCV` (so our own offer, also `seq 1 ack 0`, is never a hello); 6 fields are enough (pyNetinstall devices), the simulator sends 8 like real ones.
- **min_os** newer than the installed version is a `min_os_newer` warning only (the official tool does not check it).
- **Device row configuration:** the `preparing` state carries the profile's `config` (and snapshot devices have `config`), so the table shows it without extra requests.
- **Empty version directories** are never listed; listings and deletions remove them unless a download is starting in them (`busy`); the frontend also ignores a version without items.
- **Not in this plan (documented in the roadmap):** a TFTP "official-compat" mode (DATA from port 69, 1452-byte blocks) — the research shows what the official tool does, not that RouterBOOT needs it, and replying from port 69 needs `SO_REUSEPORT` on the listener, which Plan 2 deliberately avoids; PING/PONG detection of a second Netinstall server (follow-up). BOOTP already answers requests shorter than 300 bytes (the official server ignores them); Task 8 pins that.
- **Every code block below was compiled and tested** in a prototype before this plan was written; the plan was replayed on a fresh clone of `feat/protocol-align`, and the resulting tree matches the prototype (build outputs and `node_modules` aside). The VM run (Task 13's roadmap entry) used the prototype's `udp_device`.

## Public API changes

```rust
// ni-proto
message::offer(&Offer { license_key, keep_config, remove_branding }) -> Vec<u8>;  message::parse_offer(..)
message::term(AfterInstall::{Reboot, RebootWithMessage, Shutdown});  message::parse_term(..)
message::file_header(name, size)  // now ends with \0;  MODE_SCRIPT_NAME = "mode.scr"
DeviceInfo { .., extra: Vec<String> }   // HELLO_TAG = "DSCV" required
InstallPlan { packages, config: DeviceConfig, mode_script, remove_branding, license_key, after_install }
plan.files(); plan.offer(); plan.system_package_first();
ServerSession::new(device, &plan, cfg, now)   // no server MAC
SessionConfig { .., offer_retry, finish_polls, poll_interval };  FailReason::EndedEarly { remaining }
// ni-sim
SimConfig { .., after_files: AfterFiles::{Official, PyNetinstall, NeverWtrm, EarlyWtrm}, reply_dst }
sim.offer(); sim.after_install();   lan::DeviceReport { .., offer, after_install }
// ni-engine
ConfigMode::{Keep, Default, Empty, Script { local_name }};  AfterInstall::{Reboot, RebootMessage, Shutdown}
Profile { .., mode_script, remove_branding, license_key, after_install };  license_key_from_file(..)
DeviceView { .., config };  DeviceState::Preparing { version, config }
// codes: + flash_ended_early, invalid_license_key, mode_script_with_keep, min_os_newer; − no_mac
// app: + pick_license_key command (LicenseKeyView { key }), key_file_failed
```

```text
ni-cli profiles save / flash --version …  [--config keep|default|empty] [--script F]
    [--mode-script F] [--remove-branding] [--license-key F] [--after reboot|reboot-message|shutdown]
```

## File structure

```
crates/ni-proto/src/message.rs              # official payloads (offer, FILE, TERM), Offer, AfterInstall
crates/ni-proto/src/device_info.rs          # DSCV required, extra fields
crates/ni-proto/src/session/{mod,steps}.rs  # DeviceConfig, InstallPlan options, the official step machine
crates/ni-proto/tests/official/mod.rs       # capture loader (shared with ni-sim)
crates/ni-proto/tests/conformance.rs        # golden test: our session vs the official server
crates/ni-proto/tests/fixtures/official-7.25.1/*.txt, user.rsc, mode.rsc, fake.key
crates/ni-sim/src/lib.rs                    # the device the official server expects
crates/ni-sim/src/lan.rs                    # reports offer / TERM
crates/ni-sim/tests/{e2e,lan,official_replay}.rs
crates/ni-sim/examples/udp_device.rs        # the simulator on a real UDP socket (VM check)
crates/ni-engine/src/profile.rs             # ConfigMode, AfterInstall, options, migration
crates/ni-engine/src/prepare.rs             # plan from the profile's options
crates/ni-engine/src/config.rs              # no server MAC
crates/ni-engine/src/engine/{core,device,mod}.rs   # own frames by address, min_os warning, config per device
crates/ni-engine/src/{event,codes,lib,preflight}.rs
crates/ni-cli/src/{cli,commands,flash,output}.rs   # configuration flags
crates/ni-bootp/src/responder.rs            # short requests (test)
crates/ni-assets/src/assets/inventory.rs    # empty version directories
app/src-tauri/{build.rs,capabilities/default.json,src/*.rs,tests/bridge.rs}  # pick_license_key
app/src/{lib/*.ts,components/*.svelte,locales/*.json}, app/tests/*.ts
docs/superpowers/plans/2026-11-06-00-roadmap.md
```

## Tasks

---

### Task 1: ni-proto: the official offer, file header and TERM payloads; hello needs DSCV

**Files:**
- Modify: `crates/ni-proto/src/device_info.rs`
- Modify: `crates/ni-proto/src/lib.rs`
- Modify: `crates/ni-proto/src/message.rs`
- Modify: `crates/ni-proto/src/session/mod.rs`
- Modify: `crates/ni-sim/src/lib.rs`

The official server's text payloads end with a NUL counted in the length, the offer carries three fields (license key, `keep`, `removeBranding`) and `TERM` an optional message. `message::offer` / `parse_offer`, `term` / `parse_term`, `AfterInstall` and `MODE_SCRIPT_NAME` are added; `file_header` gains the NUL (`parse_file_header` accepts both forms). The offer test pins the four captured hex strings of the research doc. A hello must start with `DSCV` (our own offer also has `seq 1 ack 0`) and keeps fields after `min_os` in `DeviceInfo::extra`; the simulator now sends the official 8 fields. The session itself changes in Task 2. The unit tests live in the module itself (`#[cfg(test)] mod tests`), so they are written together with the code; run them right after.

- [ ] **Step 1: Edit `crates/ni-proto/src/device_info.rs` (with its unit tests)**

1. In `crates/ni-proto/src/device_info.rs`, replace:

```rust
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
```

with:

```rust
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
```

2. In `crates/ni-proto/src/device_info.rs`, replace:

```rust
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
```

with:

```rust
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
```

3. In `crates/ni-proto/src/device_info.rs`, replace:

```rust
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
```

with:

```rust
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
```

4. In `crates/ni-proto/src/device_info.rs`, replace:

```rust
        assert_eq!(info.arch, "arm64");
        assert_eq!(info.min_os, "7.1");
    }

```

with:

```rust
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

```

- [ ] **Step 2: Edit `crates/ni-proto/src/lib.rs`**

1. In `crates/ni-proto/src/lib.rs`, replace:

```rust
//!
//! Protocol knowledge is derived from pyNetinstall
//! (<https://github.com/rfdrake/pynetinstall>, GPL-3.0).

pub mod device_info;
```

with:

```rust
//!
//! Protocol knowledge is derived from pyNetinstall
//! (<https://github.com/rfdrake/pynetinstall>, GPL-3.0) and from captures of
//! the official `netinstall-cli` 7.25.1 talking to a simulated device
//! (`docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md`),
//! which is what the session follows where the two differ.

pub mod device_info;
```

2. In `crates/ni-proto/src/lib.rs`, replace:

```rust
pub mod session;

pub use device_info::DeviceInfo;
pub use frame::{DecodeError, Frame, Mac, ParseMacError, HEADER_LEN, MAX_CHUNK, PORT};
pub use session::{
    Event, FailReason, InstallPlan, Phase, PlanError, PlanFile, ServerSession, SessionConfig,
```

with:

```rust
pub mod session;

pub use device_info::{DeviceInfo, HELLO_TAG};
pub use frame::{DecodeError, Frame, Mac, ParseMacError, HEADER_LEN, MAX_CHUNK, PORT};
pub use message::{AfterInstall, Offer};
pub use session::{
    Event, FailReason, InstallPlan, Phase, PlanError, PlanFile, ServerSession, SessionConfig,
```

- [ ] **Step 3: Edit `crates/ni-proto/src/message.rs` (with its unit tests)**

1. In `crates/ni-proto/src/message.rs`, replace:

```rust
//! Payload constants and helpers.
//!
//! Server → device payloads and expected device replies, as used by pyNetinstall.

/// Offer to install (server → device). Device answers [`YACK`].
pub const OFFR: &[u8] = b"OFFR\n\n";
pub const YACK: &[u8] = b"YACK\n";
/// Negative acknowledgement prefix (device → server).
///
/// Speculative: the string appears in the official Netinstall tool, but
/// pyNetinstall never checks for it. A device reply starting with it fails
/// the session.
pub const NACK: &[u8] = b"NACK";
/// Device finished formatting.
```

with:

```rust
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
```

2. In `crates/ni-proto/src/message.rs`, replace:

```rust
/// Device waiting for terminate.
pub const WTRM: &[u8] = b"WTRM";
/// "No more files" marker (server → device). Device answers [`WTRM`].
pub const FILE_END: &[u8] = b"FILE\n";
/// Reboot command (server → device). No reply.
pub const TERM: &[u8] = b"TERM\n";
/// Magic bytes every RouterOS `.npk` package starts with (pyNetinstall `verify_npk`).
pub const NPK_MAGIC: [u8; 4] = [0x1E, 0xF1, 0xD0, 0xBA];
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
```

with:

```rust
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
```

3. In `crates/ni-proto/src/message.rs`, replace:

```rust
    fn file_header_roundtrip() {
        let h = file_header("routeros-7.16-arm64.npk", 12_345_678);
        assert_eq!(h, b"FILE\nrouteros-7.16-arm64.npk\n12345678\n");
        assert_eq!(
            parse_file_header(&h),
            Some(("routeros-7.16-arm64.npk".to_string(), 12_345_678))
        );
    }
```

with:

```rust
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
```

4. In `crates/ni-proto/src/message.rs`, replace:

```rust
        assert_eq!(parse_file_header(b"FILE\nx\nnotanumber\n"), None);
    }
}
```

with:

```rust
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
```

- [ ] **Step 4: Edit `crates/ni-proto/src/session/mod.rs` (with its unit tests)**

1. In `crates/ni-proto/src/session/mod.rs`, replace:

```rust
            arch: "mmips".into(),
            min_os: "7.0".into(),
        }
    }
```

with:

```rust
            arch: "mmips".into(),
            min_os: "7.0".into(),
            extra: Vec::new(),
        }
    }
```

- [ ] **Step 5: Edit `crates/ni-sim/src/lib.rs`**

1. In `crates/ni-sim/src/lib.rs`, replace:

```rust
                arch: arch.into(),
                min_os: "7.0".into(),
            },
            hello_interval: Duration::from_secs(1),
```

with:

```rust
                arch: arch.into(),
                min_os: "7.0".into(),
                extra: vec!["0".into(), "0".into()],
            },
            hello_interval: Duration::from_secs(1),
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p ni-proto -p ni-sim`
Expected: every suite `ok`; ni-proto's unit tests `test result: ok. 33 passed`, ni-sim's `13 passed` (lib) and `14 passed` (e2e) — the old flow still works with the new payloads

- [ ] **Step 7: Commit**

```bash
git add crates/ni-proto/src/device_info.rs crates/ni-proto/src/lib.rs crates/ni-proto/src/message.rs crates/ni-proto/src/session/mod.rs crates/ni-sim/src/lib.rs
git commit -m "feat(proto): official offer, file header and TERM payloads; strict hello tag" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: ni-proto + ni-sim: the official session flow

**Files:**
- Modify: `app/src-tauri/tests/bridge.rs`
- Modify: `app/src/lib/codes.json`
- Modify: `app/src/locales/en.json`
- Modify: `app/src/locales/hu.json`
- Modify: `crates/ni-assets/src/assets/packages.rs`
- Modify: `crates/ni-assets/tests/network.rs`
- Modify: `crates/ni-engine/src/codes.rs`
- Modify: `crates/ni-engine/src/engine/core.rs`
- Modify: `crates/ni-engine/src/prepare.rs`
- Modify: `crates/ni-engine/tests/common/mod.rs`
- Modify: `crates/ni-engine/tests/lifecycle.rs`
- Modify: `crates/ni-proto/src/lib.rs`
- Modify: `crates/ni-proto/src/session/mod.rs`
- Modify: `crates/ni-proto/src/session/steps.rs`
- Modify: `crates/ni-sim/src/lan.rs`
- Modify: `crates/ni-sim/src/lib.rs`
- Modify: `crates/ni-sim/tests/e2e.rs`
- Modify: `crates/ni-sim/tests/lan.rs`

The heart of the plan. `InstallPlan` gets `config: DeviceConfig` (`Keep`, `Default`, `Empty`, `Script(bytes)`), `mode_script`, `remove_branding`, `license_key`, `after_install`; `files()` orders `autorun.scr` / `mode.scr` like the official tool and `validate` refuses a mode script with `Keep` and a bad key. The step machine (`session/steps.rs`) is rebuilt on the official flow: offer (a `NACK` re-offers after `offer_retry`), format, prepare, per file header → chunks (one reply each, no data frame for an empty file) → poll; after the last file polls until `WTRM` (paced by `poll_interval`, at most `finish_polls`, then `FILE\n`); `WTRM` earlier is `FailReason::EndedEarly`. Frames mirror the device's MAC fields (`ServerSession::new` loses its server-MAC argument) and carry `ack = seq − 2`. `ni-sim` becomes the device the official server expects (zeros in `[6:12]`, `RETR` then `WTRM` after a file; `SimConfig::after_files` for pyNetinstall-era, never-`WTRM` and early-`WTRM` devices; it validates the offer and records offer, files and `TERM`, which `lan::DeviceReport` passes on). The engine follows minimally: the new `ServerSession::new`, `DeviceConfig` in `prepare`, `flash_ended_early` (`remaining`) with its translations, and faster finish polls in the test rigs. The session and simulator unit tests live in their modules.

- [ ] **Step 1: Install the frontend packages**

Only if `app/node_modules` does not exist yet (the translation tests need it):

```bash
[ -d app/node_modules ] || (cd app && npm ci)
```

Expected: `added … packages` (or nothing, if they were installed already).

- [ ] **Step 2: Update the tests in `app/src-tauri/tests/bridge.rs`**

1. In `app/src-tauri/tests/bridge.rs`, replace:

```rust
                max_retransmits: 2,
                chunk_delay: Duration::from_millis(1),
            };
        }));
```

with:

```rust
                max_retransmits: 2,
                chunk_delay: Duration::from_millis(1),
                offer_retry: Duration::from_millis(50),
                finish_polls: 5,
                poll_interval: Duration::from_millis(10),
            };
        }));
```

- [ ] **Step 3: Update the tests in `crates/ni-assets/tests/network.rs`**

1. In `crates/ni-assets/tests/network.rs`, replace:

```rust
    let plan = ni_proto::InstallPlan {
        packages: files,
        config_script: None,
    };
    assert_eq!(plan.validate(), Ok(()));
```

with:

```rust
    let plan = ni_proto::InstallPlan {
        packages: files,
        ..Default::default()
    };
    assert_eq!(plan.validate(), Ok(()));
```

- [ ] **Step 4: Update the test helper `crates/ni-engine/tests/common/mod.rs`**

1. In `crates/ni-engine/tests/common/mod.rs`, replace:

```rust
        max_retransmits: 2,
        chunk_delay: Duration::from_millis(1),
    }
}
```

with:

```rust
        max_retransmits: 2,
        chunk_delay: Duration::from_millis(1),
        offer_retry: Duration::from_millis(50),
        finish_polls: 5,
        poll_interval: Duration::from_millis(10),
    }
}
```

- [ ] **Step 5: Update the tests in `crates/ni-engine/tests/lifecycle.rs`**

1. In `crates/ni-engine/tests/lifecycle.rs`, replace:

```rust
use ni_engine::{DeviceState, EngineEvent, EngineHandle, WaitReason};
use ni_net::{bind_udp, BindOptions};
use std::net::Ipv4Addr;
use std::time::Duration;
```

with:

```rust
use ni_engine::{DeviceState, EngineEvent, EngineHandle, WaitReason};
use ni_net::{bind_udp, BindOptions};
use ni_sim::AfterFiles;
use std::net::Ipv4Addr;
use std::time::Duration;
```

2. In `crates/ni-engine/tests/lifecycle.rs`, replace:

```rust

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_device_that_netboots_again_is_reflashed() {
    let mut rig = rig(|_| {}).await;
```

with:

```rust

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_that_stops_before_the_last_file_is_flash_ended_early() {
    let mut rig = rig(|_| {}).await;
    let mut early = sim_config(A);
    early.after_files = AfterFiles::EarlyWtrm;
    let sim = boot(&rig, A, early).await;
    rig.lan.add(sim).await;
    let e = state(&mut rig.events, A, "failed").await;
    let EngineEvent::Device {
        state: DeviceState::Failed { code, params },
        ..
    } = e
    else {
        unreachable!()
    };
    assert_eq!(code, "flash_ended_early");
    assert_eq!(params["remaining"], "1", "wireless was not sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_device_that_netboots_again_is_reflashed() {
    let mut rig = rig(|_| {}).await;
```

- [ ] **Step 6: Update the tests in `crates/ni-sim/tests/e2e.rs`**

Replace the whole of `crates/ni-sim/tests/e2e.rs` with:

```rust
use bytes::Bytes;
use ni_proto::{
    message, AfterInstall, DeviceConfig, Event, FailReason, Frame, InstallPlan, Mac, Offer, Phase,
    PlanError, PlanFile, ServerSession, SessionConfig, MAX_CHUNK,
};
use ni_sim::harness::{discover, pump, pump_with, Direction};
use ni_sim::{AfterFiles, DeviceSim, ReceivedFile, SimConfig};
use std::time::{Duration, Instant};

const DEVICE: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x01]);

/// Deterministic test data without short periods (so duplicated or swapped
/// chunks are detectable).
fn pattern(len: usize, seed: u8) -> Bytes {
    Bytes::from(
        (0..len as u32)
            .map(|i| ((i.wrapping_mul(2_654_435_761) >> 24) as u8).wrapping_add(seed))
            .collect::<Vec<u8>>(),
    )
}

/// NPK-looking test data: the NPK magic followed by [`pattern`] bytes.
fn npk(len: usize, seed: u8) -> Bytes {
    assert!(len >= message::NPK_MAGIC.len());
    let mut v = message::NPK_MAGIC.to_vec();
    v.extend_from_slice(&pattern(len - message::NPK_MAGIC.len(), seed));
    Bytes::from(v)
}

fn setup(sim_cfg: SimConfig, plan: &InstallPlan) -> (ServerSession, DeviceSim, Instant) {
    let now = Instant::now();
    let mut sim = DeviceSim::new(sim_cfg, now);
    let info = discover(&mut sim, now);
    assert_eq!(info.mac, DEVICE);
    let server = ServerSession::new(info, plan, SessionConfig::default(), now).unwrap();
    (server, sim, now)
}

fn assert_no_violations(sim: &DeviceSim) {
    assert!(
        sim.violations().is_empty(),
        "protocol violations: {:?}",
        sim.violations()
    );
}

fn one_package(data: Bytes) -> InstallPlan {
    InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data,
        }],
        ..InstallPlan::default()
    }
}

fn events(server: &mut ServerSession) -> Vec<Event> {
    std::iter::from_fn(|| server.poll_event()).collect()
}

#[test]
fn installs_package_and_config_script() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros-7.16-mmips.npk".into(),
            data: npk(3000, 1),
        }],
        config: DeviceConfig::Script(Bytes::from_static(b"/system identity set name=lab\n")),
        ..InstallPlan::default()
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
    assert_no_violations(&sim);
    assert_eq!(
        sim.received_files(),
        &[
            ReceivedFile {
                name: "routeros-7.16-mmips.npk".into(),
                data: npk(3000, 1).to_vec()
            },
            ReceivedFile {
                name: message::CONFIG_SCRIPT_NAME.into(),
                data: b"/system identity set name=lab\n".to_vec()
            },
        ]
    );
    let ev = events(&mut server);
    assert_eq!(ev.last(), Some(&Event::Done));
    assert_eq!(server.phase(), &Phase::Done);
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
        ]
    );
}

#[test]
fn installs_multiple_packages_including_empty_and_exact_chunk_sizes() {
    let plan = InstallPlan {
        packages: vec![
            PlanFile {
                name: "routeros.npk".into(),
                data: npk(2048, 2),
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
        ..InstallPlan::default()
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
    assert_eq!(sim.received_files()[0].data, npk(2048, 2).to_vec());
    assert!(sim.received_files()[2].data.is_empty());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
    assert_eq!(events(&mut server).last(), Some(&Event::Done));
}

#[test]
fn recovers_from_lost_device_replies() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(5000, 4),
        }],
        ..InstallPlan::default()
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
    assert_eq!(sim.received_files()[0].data, npk(5000, 4).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn recovers_from_lost_server_frames() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(5000, 5),
        }],
        ..InstallPlan::default()
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
            if dir == Direction::ToDevice && message::parse_term(&f.payload).is_none() {
                to_device += 1;
                to_device % 4 != 0
            } else {
                true
            }
        },
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, npk(5000, 5).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn file_header_without_reply_is_tolerated() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(100, 6),
        }],
        ..InstallPlan::default()
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
    assert_eq!(sim.received_files()[0].data, npk(100, 6).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn silent_device_fails_with_timeout_in_offering() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(10, 7),
        }],
        ..InstallPlan::default()
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
    let ev = events(&mut server);
    assert_eq!(
        ev.last(),
        Some(&Event::Failed(FailReason::Timeout { phase: "offering" }))
    );
    assert!(!ev.contains(&Event::PhaseChanged(Phase::Failed)));
}

/// Classify a server → device frame for the transcript.
fn kind(payload: &[u8]) -> String {
    if let Some(o) = message::parse_offer(payload) {
        format!("OFFR keep={}", o.keep_config)
    } else if payload.is_empty() {
        "EMPTY".into()
    } else if payload == message::FILE_END {
        "FILE_END".into()
    } else if let Some(after) = message::parse_term(payload) {
        format!("TERM {after:?}")
    } else if let Some((name, size)) = message::parse_file_header(payload) {
        format!("FILE {name} {size}")
    } else {
        format!("CHUNK {}", payload.len())
    }
}

#[test]
fn transcript_follows_the_official_flow() {
    let script = Bytes::from_static(b"/system identity set name=gold\n");
    let plan = InstallPlan {
        packages: vec![
            PlanFile {
                name: "routeros.npk".into(),
                data: npk(1500, 10),
            },
            PlanFile {
                name: "extra.npk".into(),
                data: pattern(10, 11),
            },
        ],
        config: DeviceConfig::Script(script.clone()),
        ..InstallPlan::default()
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);
    let mut sent: Vec<(String, u16, u16)> = Vec::new();

    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, f| {
            if dir == Direction::ToDevice {
                sent.push((kind(&f.payload), f.seq, f.ack));
            }
            true
        },
    );

    // OFFR, format, prepare; per file: header, chunks, poll; one more poll
    // (answered WTRM); TERM. The k-th frame has seq k, ack k - 2.
    let script_len = script.len();
    let expected: Vec<(String, u16, u16)> = [
        "OFFR keep=false".to_string(),
        "EMPTY".into(),
        "EMPTY".into(),
        "FILE routeros.npk 1500".into(),
        "CHUNK 1024".into(),
        "CHUNK 476".into(),
        "EMPTY".into(),
        "FILE extra.npk 10".into(),
        "CHUNK 10".into(),
        "EMPTY".into(),
        format!("FILE autorun.scr {script_len}"),
        format!("CHUNK {script_len}"),
        "EMPTY".into(),
        "EMPTY".into(),
        "TERM Reboot".into(),
    ]
    .into_iter()
    .zip(1u16..)
    .map(|(k, seq)| (k, seq, seq.saturating_sub(2)))
    .collect();
    assert_eq!(sent, expected);
    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn every_configuration_reaches_the_device() {
    let script = Bytes::from_static(b"/system identity set name=s\n");
    let mode = Bytes::from_static(b":log info mode\n");
    /// Configuration, mode script, files after the package.
    type Case = (DeviceConfig, Option<Bytes>, Vec<(&'static str, usize)>);
    let cases: Vec<Case> = vec![
        (DeviceConfig::Keep, None, vec![]),
        (DeviceConfig::Default, None, vec![]),
        (
            DeviceConfig::Default,
            Some(mode.clone()),
            vec![("mode.scr", 15)],
        ),
        (DeviceConfig::Empty, None, vec![("autorun.scr", 0)]),
        (
            DeviceConfig::Empty,
            Some(mode.clone()),
            vec![("mode.scr", 15), ("autorun.scr", 0)],
        ),
        (
            DeviceConfig::Script(script.clone()),
            Some(mode.clone()),
            vec![("autorun.scr", 28), ("mode.scr", 15)],
        ),
    ];
    for (config, mode_script, extra) in cases {
        let keep = config == DeviceConfig::Keep;
        let plan = InstallPlan {
            packages: vec![PlanFile {
                name: "routeros.npk".into(),
                data: npk(1100, 20),
            }],
            config: config.clone(),
            mode_script,
            remove_branding: true,
            license_key: "KEY-1".into(),
            after_install: AfterInstall::Shutdown,
        };
        let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);
        pump(
            &mut server,
            &mut sim,
            now,
            Duration::from_secs(60),
            |_, _| true,
        );
        assert_eq!(events(&mut server).last(), Some(&Event::Done), "{config:?}");
        assert_no_violations(&sim);
        let mut want = vec![("routeros.npk", 1100)];
        want.extend(extra);
        let got: Vec<(&str, usize)> = sim
            .received_files()
            .iter()
            .map(|f| (f.name.as_str(), f.data.len()))
            .collect();
        assert_eq!(got, want, "{config:?}");
        assert_eq!(
            sim.offer(),
            Some(&Offer {
                license_key: "KEY-1".into(),
                keep_config: keep,
                remove_branding: true
            })
        );
        assert_eq!(sim.after_install(), Some(AfterInstall::Shutdown));
    }
}

#[test]
fn a_pynetinstall_era_device_finishes_through_the_file_end_fallback() {
    for after in [AfterFiles::PyNetinstall, AfterFiles::NeverWtrm] {
        let mut sim_cfg = SimConfig::new(DEVICE, "hEX", "mmips");
        sim_cfg.after_files = after;
        let plan = one_package(npk(2000, 21));
        let (mut server, mut sim, now) = setup(sim_cfg, &plan);
        let mut sent = Vec::new();
        let end = pump(
            &mut server,
            &mut sim,
            now,
            Duration::from_secs(60),
            |dir, f| {
                if dir == Direction::ToDevice {
                    sent.push(kind(&f.payload));
                }
                true
            },
        );
        assert_eq!(events(&mut server).last(), Some(&Event::Done), "{after:?}");
        assert!(sim.is_terminated());
        assert_no_violations(&sim);
        let cfg = SessionConfig::default();
        let polls = sent.iter().skip(5).filter(|k| *k == "EMPTY").count();
        assert_eq!(polls, cfg.finish_polls as usize, "{after:?}");
        assert_eq!(sent[sent.len() - 2..], ["FILE_END", "TERM Reboot"]);
        // The polls were paced: the fallback costs finish_polls - 1 intervals.
        assert!(end.now >= now + cfg.poll_interval * (cfg.finish_polls - 1));
    }
}

#[test]
fn a_device_that_stops_early_fails_loudly() {
    let mut sim_cfg = SimConfig::new(DEVICE, "hEX", "mmips");
    sim_cfg.after_files = AfterFiles::EarlyWtrm;
    let mut plan = one_package(npk(2000, 22));
    plan.config = DeviceConfig::Script(Bytes::from_static(b"/x\n"));
    let (mut server, mut sim, now) = setup(sim_cfg, &plan);
    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |_, _| true,
    );
    assert_eq!(
        events(&mut server).last(),
        Some(&Event::Failed(FailReason::EndedEarly { remaining: 1 }))
    );
    assert!(!sim.is_terminated(), "no TERM after a failure");
}

#[test]
fn a_refused_offer_is_repeated_then_rejected() {
    let mut sim_cfg = SimConfig::new(DEVICE, "hEX", "mmips");
    sim_cfg.reject_offer = true;
    let plan = one_package(npk(100, 23));
    let (mut server, mut sim, now) = setup(sim_cfg, &plan);
    let mut offers = 0;
    let end = pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, f| {
            if dir == Direction::ToDevice {
                assert_eq!((f.seq, f.ack), (1, 0));
                offers += 1;
            }
            true
        },
    );
    let cfg = SessionConfig::default();
    assert_eq!(offers, 1 + cfg.max_retransmits);
    assert!(end.now >= now + cfg.offer_retry * cfg.max_retransmits);
    assert_eq!(
        events(&mut server).last(),
        Some(&Event::Failed(FailReason::Rejected { phase: "offering" }))
    );
    assert_no_violations(&sim);
}

#[test]
fn slow_format_within_format_timeout_succeeds() {
    let cfg = SessionConfig::default();
    let delay = cfg.reply_timeout * 3;
    assert!(cfg.reply_timeout < delay && delay < cfg.format_timeout);
    let mut sim_cfg = SimConfig::new(DEVICE, "CCR2004", "arm64");
    sim_cfg.format_delay = delay;
    let plan = one_package(npk(2000, 12));
    let (mut server, mut sim, now) = setup(sim_cfg, &plan);

    let end = pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(300),
        |_, _| true,
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert!(end.now >= now + delay, "the format delay really elapsed");
    assert_eq!(sim.received_files()[0].data, npk(2000, 12).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

/// Documents a risk rather than desired behaviour: the session retransmits
/// unanswered frames with the same seq, assuming the device de-duplicates
/// them (pyNetinstall never retransmits). If a device does not, a lost reply
/// on a mid-file chunk makes the device append that chunk twice. This has to
/// be verified on real hardware.
#[test]
fn without_device_dedup_a_lost_reply_corrupts_the_file() {
    let mut sim_cfg = SimConfig::new(DEVICE, "RB951", "mipsbe");
    sim_cfg.dedup = false;
    // No device-side re-sends, so the server has to retransmit.
    sim_cfg.resend_interval = Duration::from_secs(3600);
    let data = npk(4 * MAX_CHUNK, 14);
    let plan = one_package(data.clone());
    let (mut server, mut sim, now) = setup(sim_cfg, &plan);
    // OFFR=1, format=2, prepare=3, header=4, chunk 1=5, chunk 2=6.
    let chunk2_seq = 6;
    let mut dropped = false;

    pump_with(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(120),
        |dir, f| {
            if dir == Direction::ToServer && f.seq == chunk2_seq && !dropped {
                dropped = true;
                return Vec::new();
            }
            vec![f]
        },
    );

    assert!(dropped);
    let received = &sim.received_files()[0].data;
    assert_ne!(received, &data.to_vec(), "corruption is detectable");
    // The device stored chunk 2 twice and considers the file complete
    // after chunk 3.
    let c = |i: usize| &data[i * MAX_CHUNK..(i + 1) * MAX_CHUNK];
    assert_eq!(received, &[c(0), c(1), c(1), c(2)].concat());
}

#[test]
fn frames_from_other_macs_are_ignored() {
    const OTHER: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x02]);
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(2500, 15),
        }],
        config: DeviceConfig::Script(Bytes::from_static(b"/x\n")),
        ..InstallPlan::default()
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);
    let mut other = DeviceSim::new(SimConfig::new(OTHER, "hEX", "mmips"), now);
    other.handle_timeout(now);
    let other_hello = other.poll_transmit().expect("hello from the second device");
    let mut injected = 0;

    pump_with(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, f| match dir {
            Direction::ToDevice => vec![f],
            Direction::ToServer => {
                injected += 1;
                // A second device's hello, and a reply from it with matching
                // seq/ack that would abort the session if it were accepted.
                let spoof = Frame::new(OTHER, Mac::ZERO, f.seq, f.ack, message::NACK);
                vec![other_hello.clone(), spoof, f]
            }
        },
    );

    assert!(injected > 10);
    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, npk(2500, 15).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn nack_mid_upload_fails_with_rejected() {
    let plan = one_package(npk(3000, 16));
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);
    // Chunk 2 has seq 6 (see above); the device rejects it.
    pump_with(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, mut f| {
            if dir == Direction::ToServer && f.seq == 6 {
                f.payload = Bytes::from_static(b"NACK\nwrite failed\n");
            }
            vec![f]
        },
    );

    assert_eq!(server.phase(), &Phase::Failed);
    assert_eq!(
        events(&mut server).last(),
        Some(&Event::Failed(FailReason::Rejected { phase: "uploading" }))
    );
}

#[test]
fn invalid_plans_are_rejected_before_anything_is_sent() {
    let now = Instant::now();
    let mut sim = DeviceSim::new(SimConfig::new(DEVICE, "hEX", "mmips"), now);
    let info = discover(&mut sim, now);
    let new = |plan: &InstallPlan| {
        ServerSession::new(info.clone(), plan, SessionConfig::default(), now).err()
    };
    assert_eq!(new(&InstallPlan::default()), Some(PlanError::NoPackages));
    assert_eq!(
        new(&one_package(pattern(100, 17))),
        Some(PlanError::NotAnNpk {
            name: "routeros.npk".into()
        })
    );
}
```

- [ ] **Step 7: Update the tests in `crates/ni-sim/tests/lan.rs`**

1. In `crates/ni-sim/tests/lan.rs`, replace:

```rust
#![cfg(feature = "lan")]

use ni_proto::{message, DeviceInfo, Frame, Mac};
use ni_sim::lan::{bootp_request, parse_boot_reply, BootReply, SimLan};
use ni_sim::{DeviceSim, SimConfig};
```

with:

```rust
#![cfg(feature = "lan")]

use ni_proto::{message, DeviceInfo, Frame, Mac, Offer};
use ni_sim::lan::{bootp_request, parse_boot_reply, BootReply, SimLan};
use ni_sim::{DeviceSim, SimConfig};
```

2. In `crates/ni-sim/tests/lan.rs`, replace:

```rust

const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 1, 2, 3]);
const SERVER: Mac = Mac([2, 0, 0, 0, 0, 1]);

#[test]
```

with:

```rust

const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 1, 2, 3]);

#[test]
```

3. In `crates/ni-sim/tests/lan.rs`, replace:

```rust

    // An offer is answered with YACK...
    let offer = Frame::new(SERVER, MAC, 1, 0, message::OFFR.to_vec());
    server
        .send_to(&offer.encode(), lan.netinstall_addr())
```

with:

```rust

    // An offer is answered with YACK...
    let offer = Frame::new(Mac::BROADCAST, MAC, 1, 0, message::offer(&Offer::default()));
    server
        .send_to(&offer.encode(), lan.netinstall_addr())
```

- [ ] **Step 8: Run it to verify it fails**

Run: `cargo test --workspace --no-run`
Expected: the tests do not compile: `unresolved import `ni_proto::DeviceConfig``, `unresolved import `ni_sim::AfterFiles``, `this function takes 5 arguments but 4 arguments were supplied`, `struct `InstallPlan` has no field named `config``

- [ ] **Step 9: Edit `app/src/locales/en.json`**

1. In `app/src/locales/en.json`, replace:

```json
  "code.duplicate_package": "The package {package} is listed twice.",
  "code.engine_gone": "The engine stopped unexpectedly. Restart Mac Netinstall.",
  "code.flash_rejected": "The device refused the install (step: {phase}).",
  "code.flash_timeout": "The device stopped answering (step: {phase}).",
```

with:

```json
  "code.duplicate_package": "The package {package} is listed twice.",
  "code.engine_gone": "The engine stopped unexpectedly. Restart Mac Netinstall.",
  "code.flash_ended_early": "The device stopped receiving with {remaining, plural, one {# file} other {# files}} still to send: the install is incomplete.",
  "code.flash_rejected": "The device refused the install (step: {phase}).",
  "code.flash_timeout": "The device stopped answering (step: {phase}).",
```

- [ ] **Step 10: Edit `app/src/locales/hu.json`**

1. In `app/src/locales/hu.json`, replace:

```json
  "code.duplicate_package": "Ez a csomag kétszer szerepel: {package}.",
  "code.engine_gone": "A motor váratlanul leállt. Indítsd újra a Mac Netinstallt.",
  "code.flash_rejected": "Az eszköz elutasította a telepítést (lépés: {phase}).",
  "code.flash_timeout": "Az eszköz nem válaszol (lépés: {phase}).",
```

with:

```json
  "code.duplicate_package": "Ez a csomag kétszer szerepel: {package}.",
  "code.engine_gone": "A motor váratlanul leállt. Indítsd újra a Mac Netinstallt.",
  "code.flash_ended_early": "Az eszköz leállt, mielőtt minden fájlt megkapott volna ({remaining} fájl kimaradt): a telepítés nem teljes.",
  "code.flash_rejected": "Az eszköz elutasította a telepítést (lépés: {phase}).",
  "code.flash_timeout": "Az eszköz nem válaszol (lépés: {phase}).",
```

- [ ] **Step 11: Edit `crates/ni-assets/src/assets/packages.rs` (with its unit tests)**

1. In `crates/ni-assets/src/assets/packages.rs`, replace:

```rust
        let plan = InstallPlan {
            packages: files.clone(),
            config_script: None,
        };
        assert_eq!(plan.validate(), Ok(()));
```

with:

```rust
        let plan = InstallPlan {
            packages: files.clone(),
            ..Default::default()
        };
        assert_eq!(plan.validate(), Ok(()));
```

- [ ] **Step 12: Edit `crates/ni-engine/src/codes.rs`**

1. In `crates/ni-engine/src/codes.rs`, replace:

```rust
    "duplicate_package",
    "engine_gone",
    "flash_rejected",
    "flash_timeout",
```

with:

```rust
    "duplicate_package",
    "engine_gone",
    "flash_ended_early",
    "flash_rejected",
    "flash_timeout",
```

- [ ] **Step 13: Edit `crates/ni-engine/src/engine/core.rs` (with its unit tests)**

1. In `crates/ni-engine/src/engine/core.rs`, replace:

```rust
        let Some(run) = self.run.as_mut() else { return };
        let session_cfg = run.cfg.session.clone();
        let server_mac = run.net.server_mac;
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
```

with:

```rust
        let Some(run) = self.run.as_mut() else { return };
        let session_cfg = run.cfg.session.clone();
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
```

2. In `crates/ni-engine/src/engine/core.rs`, replace:

```rust
                let info = dev.info.clone().expect("prepared devices have said hello");
                dev.version = Some(version.clone());
                match ServerSession::new(server_mac, info, &plan, session_cfg, Instant::now()) {
                    Ok(session) => {
                        dev.session = Some(session);
```

with:

```rust
                let info = dev.info.clone().expect("prepared devices have said hello");
                dev.version = Some(version.clone());
                match ServerSession::new(info, &plan, session_cfg, Instant::now()) {
                    Ok(session) => {
                        dev.session = Some(session);
```

3. In `crates/ni-engine/src/engine/core.rs`, replace:

```rust
                        Failure::new("flash_rejected").with("phase", phase)
                    }
                    FailReason::Cancelled => Failure::new("cancelled"),
                };
```

with:

```rust
                        Failure::new("flash_rejected").with("phase", phase)
                    }
                    FailReason::EndedEarly { remaining } => {
                        Failure::new("flash_ended_early").with("remaining", remaining)
                    }
                    FailReason::Cancelled => Failure::new("cancelled"),
                };
```

- [ ] **Step 14: Edit `crates/ni-engine/src/prepare.rs` (with its unit tests)**

1. In `crates/ni-engine/src/prepare.rs`, replace:

```rust
use ni_assets::{AssetError, Assets, KernelSet, PkgArch, Version, VersionSpec};
use ni_bootp::BootArch;
use ni_proto::InstallPlan;
use ni_tftp::FileMap;
use std::collections::HashMap;
```

with:

```rust
use ni_assets::{AssetError, Assets, KernelSet, PkgArch, Version, VersionSpec};
use ni_bootp::BootArch;
use ni_proto::{DeviceConfig, InstallPlan};
use ni_tftp::FileMap;
use std::collections::HashMap;
```

2. In `crates/ni-engine/src/prepare.rs`, replace:

```rust
        .await
        .map_err(fail)?;
    let config_script = match &profile.config {
        ConfigMode::Default => None,
        ConfigMode::CustomScript { local_name } => {
            Some(assets.local_file(local_name).await.map_err(fail)?)
        }
    };
    let plan = InstallPlan {
        packages,
        config_script,
    };
    plan.validate()
```

with:

```rust
        .await
        .map_err(fail)?;
    let config = match &profile.config {
        ConfigMode::Default => DeviceConfig::Default,
        ConfigMode::CustomScript { local_name } => {
            DeviceConfig::Script(assets.local_file(local_name).await.map_err(fail)?)
        }
    };
    let plan = InstallPlan {
        packages,
        config,
        ..InstallPlan::default()
    };
    plan.validate()
```

3. In `crates/ni-engine/src/prepare.rs`, replace:

```rust
mod tests {
    use super::*;
    use ni_assets::testutil::{assets, mirror, tempdir, v, TestServer};
    use ni_assets::Channel;
```

with:

```rust
mod tests {
    use super::*;
    use bytes::Bytes;
    use ni_assets::testutil::{assets, mirror, tempdir, v, TestServer};
    use ni_assets::Channel;
```

4. In `crates/ni-engine/src/prepare.rs`, replace:

```rust
        assert_eq!(plan.packages[1].data.as_ref(), wireless.as_slice());
        assert_eq!(
            plan.config_script.as_deref(),
            Some(&b"/system identity set name=x\n"[..])
        );
    }
```

with:

```rust
        assert_eq!(plan.packages[1].data.as_ref(), wireless.as_slice());
        assert_eq!(
            plan.config,
            DeviceConfig::Script(Bytes::from_static(b"/system identity set name=x\n"))
        );
    }
```

- [ ] **Step 15: Edit `crates/ni-proto/src/lib.rs`**

1. In `crates/ni-proto/src/lib.rs`, replace:

```rust
pub use message::{AfterInstall, Offer};
pub use session::{
    Event, FailReason, InstallPlan, Phase, PlanError, PlanFile, ServerSession, SessionConfig,
};
```

with:

```rust
pub use message::{AfterInstall, Offer};
pub use session::{
    DeviceConfig, Event, FailReason, InstallPlan, Phase, PlanError, PlanFile, ServerSession,
    SessionConfig, MAX_FINISH_POLLS,
};
```

- [ ] **Step 16: Edit `crates/ni-proto/src/session/mod.rs` (with its unit tests)**

Replace the whole of `crates/ni-proto/src/session/mod.rs` with:

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
//! Step sequence (the official `netinstall-cli` 7.25.1, as captured):
//! 1. `OFFR\n<key>\n<keep>\n<brand>\n\0` → `YACK`. A `NACK` makes us offer
//!    again (same frame) after [`SessionConfig::offer_retry`], at most
//!    [`SessionConfig::max_retransmits`] times, then the session fails
//!    with [`FailReason::Rejected`].
//! 2. empty → any reply (`STRT`, device formatted; long timeout)
//! 3. empty → any reply (`RETR`)
//! 4. per file (packages, the system package first, then `autorun.scr` /
//!    `mode.scr`; see [`InstallPlan::files`]): `FILE\n<name>\n<size>\n\0`
//!    (reply optional) → one frame per 1024 bytes, one reply each (an
//!    empty file has none) → an empty poll. Before the last file the poll
//!    must be answered `RETR`; `WTRM` there means the device stopped early
//!    ([`FailReason::EndedEarly`]).
//! 5. after the last file: empty polls while the device answers `RETR`
//!    (every [`SessionConfig::poll_interval`], at most
//!    [`SessionConfig::finish_polls`]) until it answers `WTRM`;
//! 6. fallback, only if it never did: pyNetinstall's `FILE\n` → any reply;
//! 7. `TERM\n<message>\0` → no reply, session done.
//!
//! Frame header: `[0:6]` mirrors `[6:12]` of the last device frame
//! answered (`ff:ff:ff:ff:ff:ff` for the offer, as a hello carries the
//! broadcast address there) and `[6:12]` is the device's MAC: our own MAC
//! is never on the wire. The k-th server frame has `seq = k` and
//! `ack = k - 2` (0 for the first two); the device answers with
//! `seq = ack = k`.
//!
//! Retransmission: the official server never retransmits (a lost reply
//! stalls until the device re-sends its last answer). We re-send an
//! unanswered frame with the *same* `seq`, assuming the device recognises
//! the duplicate and only repeats its previous answer. This is unverified
//! on hardware: a device that processed a re-sent chunk again would store
//! it twice (see the simulator's `SimConfig::dedup` and the
//! `without_device_dedup_a_lost_reply_corrupts_the_file` test).

mod steps;

use crate::device_info::DeviceInfo;
use crate::frame::{Frame, Mac};
use crate::message::{self, AfterInstall, Offer};
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use steps::{build_steps, Expect, Step, TimeoutKind};
use thiserror::Error;

/// Most empty polls after the last file before the session falls back to
/// `FILE\n` (the upper bound of [`SessionConfig::finish_polls`]).
pub const MAX_FINISH_POLLS: u32 = 64;

/// A file to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFile {
    pub name: String,
    pub data: Bytes,
}

/// What happens to the device's configuration (the official tool's
/// `-r` / `-e` / `-s`; nothing = keep).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceConfig {
    /// Keep the existing configuration (the official tool's default).
    Keep,
    /// RouterOS applies its default configuration (`-r`).
    #[default]
    Default,
    /// No configuration (`-e`): an empty `autorun.scr` is uploaded.
    Empty,
    /// Run this script on first boot (`-s`), uploaded as `autorun.scr`.
    Script(Bytes),
}

/// What to install on one device.
///
/// [`ServerSession::new`] validates the plan (see [`PlanError`]). In
/// particular, the whole session must fit in [`u16::MAX`] frames, so the
/// 16-bit sequence number never wraps (roughly 64 MiB of total payload).
/// Wraparound has never been exercised on hardware: pyNetinstall cannot even
/// produce it (`struct.pack("<H")` raises past 65535), so such plans are
/// rejected for now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallPlan {
    /// RouterOS packages; the system package (`routeros`) must be first.
    pub packages: Vec<PlanFile>,
    pub config: DeviceConfig,
    /// The official tool's mode script (`-sm`), uploaded as `mode.scr`.
    /// Not with [`DeviceConfig::Keep`].
    pub mode_script: Option<Bytes>,
    /// See [`Offer::remove_branding`].
    pub remove_branding: bool,
    /// Software license key for the offer, or empty. At most
    /// [`message::MAX_LICENSE_KEY`] bytes, no line breaks or NUL.
    pub license_key: String,
    pub after_install: AfterInstall,
}

impl InstallPlan {
    /// The offer's fields.
    pub fn offer(&self) -> Offer {
        Offer {
            license_key: self.license_key.clone(),
            keep_config: self.config == DeviceConfig::Keep,
            remove_branding: self.remove_branding,
        }
    }

    /// Every file to upload, in order: the packages, then (as the official
    /// tool orders them) `autorun.scr` and `mode.scr` for a script,
    /// `mode.scr` and an empty `autorun.scr` for an empty configuration,
    /// `mode.scr` alone for the default one.
    pub fn files(&self) -> Vec<(String, Bytes)> {
        let mut out: Vec<(String, Bytes)> = self
            .packages
            .iter()
            .map(|p| (p.name.clone(), p.data.clone()))
            .collect();
        let autorun = |data: Bytes| (message::CONFIG_SCRIPT_NAME.to_string(), data);
        let mode = self
            .mode_script
            .clone()
            .map(|data| (message::MODE_SCRIPT_NAME.to_string(), data));
        match &self.config {
            DeviceConfig::Keep => {}
            DeviceConfig::Default => out.extend(mode),
            DeviceConfig::Empty => {
                out.extend(mode);
                out.push(autorun(Bytes::new()));
            }
            DeviceConfig::Script(script) => {
                out.push(autorun(script.clone()));
                out.extend(mode);
            }
        }
        out
    }

    pub fn total_bytes(&self) -> u64 {
        self.files().iter().map(|(_, d)| d.len() as u64).sum()
    }
}

/// Reasons an [`InstallPlan`] is rejected before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("the plan contains no packages")]
    NoPackages,
    #[error("{name} does not look like a RouterOS package (missing NPK magic)")]
    NotAnNpk { name: String },
    /// A package name is empty, contains `\n` or `/`, or is one of the
    /// reserved script names ([`message::CONFIG_SCRIPT_NAME`],
    /// [`message::MODE_SCRIPT_NAME`]).
    #[error("invalid file name {name:?}")]
    InvalidFileName { name: String },
    #[error("the plan needs {frames} frames, at most {max} are supported")]
    TooManyFrames { frames: usize, max: usize },
    /// The official tool refuses this too ("Cannot apply mode script and
    /// keep old config at the same time").
    #[error("a mode script cannot be combined with keeping the configuration")]
    ModeScriptWithKeep,
    #[error("the license key is too long or contains a line break or NUL")]
    InvalidLicenseKey,
}

impl InstallPlan {
    /// Check the plan can be sent safely; see [`PlanError`].
    pub fn validate(&self) -> Result<(), PlanError> {
        let first = self.packages.first().ok_or(PlanError::NoPackages)?;
        if !first.data.starts_with(&message::NPK_MAGIC) {
            return Err(PlanError::NotAnNpk {
                name: first.name.clone(),
            });
        }
        if let Some(bad) = self.packages.iter().find(|p| !valid_package_name(&p.name)) {
            return Err(PlanError::InvalidFileName {
                name: bad.name.clone(),
            });
        }
        if self.mode_script.is_some() && self.config == DeviceConfig::Keep {
            return Err(PlanError::ModeScriptWithKeep);
        }
        if self.license_key.len() > message::MAX_LICENSE_KEY
            || self.license_key.contains(['\n', '\r', '\0'])
        {
            return Err(PlanError::InvalidLicenseKey);
        }
        let frames = steps::frame_count(self);
        let max = usize::from(u16::MAX);
        if frames > max {
            return Err(PlanError::TooManyFrames { frames, max });
        }
        Ok(())
    }
}

/// Package names travel in the newline-separated `FILE` header and become
/// file names on the device; the script names are reserved.
fn valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\n', '/', '\0'])
        && name != message::CONFIG_SCRIPT_NAME
        && name != message::MODE_SCRIPT_NAME
}

/// Timing parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// Wait for an ordinary reply.
    pub reply_timeout: Duration,
    /// Wait for the device to finish formatting.
    pub format_timeout: Duration,
    /// Wait for replies some devices never send (`FILE` header). With the
    /// default retransmits this waits about 40 s in total, close to
    /// pyNetinstall's 60 s socket timeout.
    pub optional_timeout: Duration,
    /// Re-sends of an unanswered frame before failing; also the number of
    /// offers made again after a `NACK`.
    pub max_retransmits: u32,
    /// Pause between file chunks (pyNetinstall uses 5 ms to avoid state errors).
    pub chunk_delay: Duration,
    /// Pause before offering again after a `NACK` (the official server
    /// waits 1 s).
    pub offer_retry: Duration,
    /// Empty polls after the last file before falling back to `FILE\n`
    /// (1 ..= [`MAX_FINISH_POLLS`]; values outside are clamped).
    pub finish_polls: u32,
    /// Pause between those polls (the first follows the last file at once).
    pub poll_interval: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_secs(10),
            format_timeout: Duration::from_secs(120),
            optional_timeout: Duration::from_secs(10),
            max_retransmits: 3,
            chunk_delay: Duration::from_millis(5),
            offer_retry: Duration::from_secs(1),
            finish_polls: 20,
            poll_interval: Duration::from_millis(250),
        }
    }
}

/// Coarse session phase, for UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Offering,
    Formatting,
    Preparing,
    Uploading { file: Arc<str> },
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
    /// The device answered a poll with `WTRM` while `remaining` files were
    /// still to be sent (the official server would skip them silently).
    #[error("device stopped receiving with {remaining} file(s) left")]
    EndedEarly { remaining: usize },
    #[error("installation cancelled")]
    Cancelled,
}

impl FailReason {
    /// Stable code for UI translation.
    pub fn code(&self) -> &'static str {
        match self {
            FailReason::Timeout { .. } => "timeout",
            FailReason::Rejected { .. } => "rejected",
            FailReason::EndedEarly { .. } => "ended_early",
            FailReason::Cancelled => "cancelled",
        }
    }
}

/// Session events.
///
/// [`Event::PhaseChanged`] is only emitted for active phases; the end of a
/// session is reported by [`Event::Done`] or [`Event::Failed`] alone (while
/// [`ServerSession::phase`] then reports [`Phase::Done`] / [`Phase::Failed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PhaseChanged(Phase),
    Progress {
        file: Arc<str>,
        file_sent: u64,
        file_total: u64,
        bytes_sent: u64,
        bytes_total: u64,
    },
    Done,
    Failed(FailReason),
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
///
/// Plans that would need more than [`u16::MAX`] frames are rejected by
/// [`ServerSession::new`], so sequence numbers never wrap (wraparound is
/// unverified on hardware; see [`InstallPlan`]).
pub struct ServerSession {
    device: DeviceInfo,
    cfg: SessionConfig,
    steps: Vec<Step>,
    idx: usize,
    seq: u16,
    wait: Wait,
    phase: Phase,
    last_frame: Option<Frame>,
    /// `[0:6]` of our next frame: `[6:12]` of the device's last reply.
    echo: Mac,
    /// Polls sent after the last file.
    finish_polls: u32,
    bytes_sent: u64,
    bytes_total: u64,
    events: VecDeque<Event>,
}

impl ServerSession {
    pub fn new(
        device: DeviceInfo,
        plan: &InstallPlan,
        mut cfg: SessionConfig,
        now: Instant,
    ) -> Result<Self, PlanError> {
        plan.validate()?;
        cfg.finish_polls = cfg.finish_polls.clamp(1, MAX_FINISH_POLLS);
        let mut events = VecDeque::new();
        events.push_back(Event::PhaseChanged(Phase::Offering));
        Ok(Self {
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
            echo: Mac::BROADCAST,
            finish_polls: 0,
            bytes_sent: 0,
            bytes_total: plan.total_bytes(),
            events,
        })
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
        let step = self.steps.get(self.idx)?;
        let (payload, expect, timeout) = (step.payload.clone(), step.expect, step.timeout);
        if self.phase != step.phase {
            self.phase = step.phase.clone();
            self.events
                .push_back(Event::PhaseChanged(self.phase.clone()));
        }
        // Cannot overflow: plans needing more than u16::MAX frames are rejected.
        debug_assert!(self.seq < u16::MAX, "sequence number would wrap");
        self.seq += 1;
        let frame = Frame::new(
            self.echo,
            self.device.mac,
            self.seq,
            self.seq.saturating_sub(2),
            payload,
        );
        self.last_frame = Some(frame.clone());
        if expect == Expect::None {
            self.advance(now, false);
        } else {
            self.wait = Wait::Reply {
                deadline: now + self.timeout(timeout),
                retries_left: self.cfg.max_retransmits,
            };
        }
        Some(frame)
    }

    /// Feed a frame received on the wire. Unrelated frames are ignored.
    pub fn handle_frame(&mut self, frame: &Frame, now: Instant) {
        let Wait::Reply { retries_left, .. } = self.wait else {
            return;
        };
        if frame.src != self.device.mac || frame.seq != self.seq || frame.ack != self.seq {
            return;
        }
        let step = &self.steps[self.idx];
        let nack = frame.payload.starts_with(message::NACK);
        let wtrm = frame.payload.as_ref() == message::WTRM;
        match step.expect {
            Expect::Offer if nack => {
                if retries_left == 0 {
                    self.fail(FailReason::Rejected {
                        phase: self.phase.name(),
                    });
                } else {
                    self.wait = Wait::Send {
                        at: now + self.cfg.offer_retry,
                        resend: Some(retries_left - 1),
                    };
                }
                return;
            }
            _ if nack => {
                self.fail(FailReason::Rejected {
                    phase: self.phase.name(),
                });
                return;
            }
            Expect::Poll { remaining } if remaining > 0 && wtrm => {
                self.fail(FailReason::EndedEarly { remaining });
                return;
            }
            _ => {}
        }
        self.echo = frame.dst;
        if let Expect::Poll { remaining: 0 } = step.expect {
            self.finish_polls += 1;
            if wtrm {
                // Done: skip the `FILE\n` fallback, straight to `TERM`.
                self.idx = self.steps.len() - 2;
                self.advance(now, false);
            } else if self.finish_polls < self.cfg.finish_polls {
                // Poll again (a new frame, not a re-send).
                self.wait = Wait::Send {
                    at: now + self.cfg.poll_interval,
                    resend: None,
                };
            } else {
                self.advance(now, false);
            }
            return;
        }
        let delay = step.delay_after;
        if let Some(chunk) = &step.chunk {
            self.bytes_sent += step.payload.len() as u64;
            self.events.push_back(Event::Progress {
                file: Arc::clone(&chunk.file),
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
            ..
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

    /// Abort the session. If it has not finished yet, it fails with
    /// [`FailReason::Cancelled`]; nothing more is sent.
    pub fn cancel(&mut self) {
        if !self.is_finished() {
            self.fail(FailReason::Cancelled);
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

    fn advance(&mut self, now: Instant, delay: bool) {
        self.idx += 1;
        if self.idx >= self.steps.len() {
            self.wait = Wait::Finished;
            self.phase = Phase::Done;
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
        self.phase = Phase::Failed;
        self.events.push_back(Event::Failed(reason));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            extra: Vec::new(),
        }
    }

    fn plan() -> InstallPlan {
        InstallPlan {
            packages: vec![PlanFile {
                name: "routeros.npk".into(),
                data: npk(10),
            }],
            ..InstallPlan::default()
        }
    }

    /// `len` bytes starting with the NPK magic.
    fn npk(len: usize) -> Bytes {
        let mut v = message::NPK_MAGIC.to_vec();
        v.resize(len.max(4), 7);
        Bytes::from(v)
    }

    fn session(plan: &InstallPlan, now: Instant) -> ServerSession {
        ServerSession::new(device(), plan, SessionConfig::default(), now).unwrap()
    }

    fn new_err(plan: &InstallPlan) -> PlanError {
        match ServerSession::new(device(), plan, SessionConfig::default(), Instant::now()) {
            Ok(_) => panic!("plan must be rejected"),
            Err(e) => e,
        }
    }

    fn file(name: &str, data: Bytes) -> PlanFile {
        PlanFile {
            name: name.into(),
            data,
        }
    }

    fn names(p: &InstallPlan) -> Vec<(String, usize)> {
        p.files().into_iter().map(|(n, d)| (n, d.len())).collect()
    }

    #[test]
    fn rejects_plan_without_packages() {
        assert_eq!(new_err(&InstallPlan::default()), PlanError::NoPackages);
    }

    #[test]
    fn rejects_first_package_without_npk_magic() {
        let p = InstallPlan {
            packages: vec![file("routeros.npk", Bytes::from_static(b"PK\x03\x04rest"))],
            ..InstallPlan::default()
        };
        assert_eq!(
            new_err(&p),
            PlanError::NotAnNpk {
                name: "routeros.npk".into()
            }
        );
        let short = InstallPlan {
            packages: vec![file("tiny.npk", Bytes::from_static(&[0x1E, 0xF1]))],
            ..InstallPlan::default()
        };
        assert_eq!(
            new_err(&short),
            PlanError::NotAnNpk {
                name: "tiny.npk".into()
            }
        );
    }

    #[test]
    fn rejects_invalid_file_names() {
        for bad in [
            "",
            "evil\nname.npk",
            "dir/name.npk",
            "nul\0.npk",
            message::CONFIG_SCRIPT_NAME,
            message::MODE_SCRIPT_NAME,
        ] {
            let p = InstallPlan {
                packages: vec![file("routeros.npk", npk(10)), file(bad, npk(10))],
                ..InstallPlan::default()
            };
            assert_eq!(new_err(&p), PlanError::InvalidFileName { name: bad.into() });
        }
    }

    #[test]
    fn rejects_a_mode_script_with_keep_and_bad_license_keys() {
        let mut p = plan();
        p.config = DeviceConfig::Keep;
        p.mode_script = Some(Bytes::from_static(b"x"));
        assert_eq!(new_err(&p), PlanError::ModeScriptWithKeep);
        for bad in [
            "a\nb",
            "a\rb",
            "a\0b",
            &"k".repeat(message::MAX_LICENSE_KEY + 1),
        ] {
            let mut p = plan();
            p.license_key = bad.into();
            assert_eq!(new_err(&p), PlanError::InvalidLicenseKey, "{bad:?}");
        }
        let mut p = plan();
        p.license_key = "k".repeat(message::MAX_LICENSE_KEY);
        assert_eq!(p.validate(), Ok(()));
    }

    #[test]
    fn rejects_plans_needing_more_than_u16_max_frames() {
        // Each package costs 2 frames (header, poll) plus one per chunk,
        // plus 5 fixed frames (OFFR, 2 empties, FILE\n, TERM) and up to
        // MAX_FINISH_POLLS - 1 extra polls.
        let plan_with = |n: usize| InstallPlan {
            packages: (0..n)
                .map(|i| {
                    file(
                        &format!("p{i}.npk"),
                        if i == 0 { npk(4) } else { Bytes::new() },
                    )
                })
                .collect(),
            ..InstallPlan::default()
        };
        let fixed = 5 + MAX_FINISH_POLLS as usize - 1;
        // The first package has one chunk.
        let max = (usize::from(u16::MAX) - fixed - 1) / 2;
        assert!(plan_with(max).validate().is_ok());
        assert_eq!(
            new_err(&plan_with(max + 1)),
            PlanError::TooManyFrames {
                frames: fixed + 1 + 2 * (max + 1),
                max: usize::from(u16::MAX),
            }
        );
    }

    #[test]
    fn files_follow_the_official_order() {
        let script = Bytes::from_static(b"/system identity set name=x\n");
        let mode = Bytes::from_static(b":log info mode\n");
        let mut p = plan();
        p.config = DeviceConfig::Keep;
        assert_eq!(names(&p), [("routeros.npk".into(), 10)]);
        p.config = DeviceConfig::Default;
        assert_eq!(names(&p), [("routeros.npk".into(), 10)]);
        p.mode_script = Some(mode.clone());
        assert_eq!(
            names(&p),
            [("routeros.npk".into(), 10), ("mode.scr".into(), 15)]
        );
        p.config = DeviceConfig::Empty;
        assert_eq!(
            names(&p),
            [
                ("routeros.npk".into(), 10),
                ("mode.scr".into(), 15),
                ("autorun.scr".into(), 0)
            ]
        );
        p.config = DeviceConfig::Script(script);
        assert_eq!(
            names(&p),
            [
                ("routeros.npk".into(), 10),
                ("autorun.scr".into(), 28),
                ("mode.scr".into(), 15)
            ]
        );
        assert_eq!(p.total_bytes(), 10 + 28 + 15);
        p.mode_script = None;
        assert_eq!(
            names(&p),
            [("routeros.npk".into(), 10), ("autorun.scr".into(), 28)]
        );
    }

    #[test]
    fn the_offer_carries_keep_key_and_branding() {
        let mut p = plan();
        assert_eq!(p.offer(), Offer::default());
        p.config = DeviceConfig::Keep;
        p.license_key = "KEY".into();
        p.remove_branding = true;
        assert_eq!(
            p.offer(),
            Offer {
                license_key: "KEY".into(),
                keep_config: true,
                remove_branding: true
            }
        );
        let now = Instant::now();
        let mut s = session(&p, now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!(&f.payload[..], b"OFFR\nKEY\nkeep\n\n\0");
    }

    fn reply(seq: u16, payload: &'static [u8]) -> Frame {
        Frame::new(DEVICE, Mac::ZERO, seq, seq, payload)
    }

    #[test]
    fn first_frame_is_offer_with_seq_1_ack_0_to_the_broadcast_slot() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (Mac::BROADCAST, DEVICE, 1, 0));
        assert_eq!(&f.payload[..], b"OFFR\n\n\nremoveBranding\n\0");
        assert_eq!(s.poll_transmit(now), None, "must wait for reply");
    }

    #[test]
    fn mac_fields_mirror_the_device_and_ack_is_seq_minus_two() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        let echo = Mac([0x12; 6]);
        s.handle_frame(&Frame::new(DEVICE, echo, 1, 1, message::YACK), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (echo, DEVICE, 2, 0));
        s.handle_frame(&reply(2, message::STRT), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (Mac::ZERO, DEVICE, 3, 1));
    }

    #[test]
    fn ignores_replies_with_wrong_seq_or_source() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&Frame::new(DEVICE, Mac::ZERO, 1, 0, &b"YACK\n"[..]), now);
        s.handle_frame(
            &Frame::new(Mac([0xbb; 6]), Mac::ZERO, 1, 1, &b"YACK\n"[..]),
            now,
        );
        assert_eq!(s.poll_transmit(now), None);
        s.handle_frame(&reply(1, message::YACK), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.seq, f.ack), (2, 0));
        assert!(f.payload.is_empty());
    }

    #[test]
    fn our_own_frames_are_never_taken_for_replies() {
        // A device that puts its own MAC in [6:12] makes us echo it in
        // [0:6]; our looped-back broadcast then has `src` = the device,
        // but `ack` is `seq - 2`, never `seq`.
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&Frame::new(DEVICE, DEVICE, 1, 1, message::YACK), now);
        let ours = s.poll_transmit(now).unwrap();
        assert_eq!(ours.src, DEVICE);
        s.handle_frame(&ours, now);
        assert_eq!(s.poll_transmit(now), None);
        assert_eq!(s.phase(), &Phase::Formatting);
    }

    #[test]
    fn formatting_step_uses_format_timeout() {
        let now = Instant::now();
        let cfg = SessionConfig::default();
        let mut s = ServerSession::new(device(), &plan(), cfg.clone(), now).unwrap();
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
        let mut s = ServerSession::new(device(), &plan(), cfg, now).unwrap();
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
    fn nack_to_the_offer_offers_again_then_fails_with_rejected() {
        let now = Instant::now();
        let cfg = SessionConfig {
            max_retransmits: 2,
            ..SessionConfig::default()
        };
        let mut s = ServerSession::new(device(), &plan(), cfg.clone(), now).unwrap();
        let offer = s.poll_transmit(now).unwrap();
        let mut t = now;
        for _ in 0..2 {
            s.handle_frame(&reply(1, b"NACK\n"), t);
            assert_eq!(s.poll_transmit(t), None, "not at once");
            t = s.poll_timeout().unwrap();
            assert_eq!(s.poll_transmit(t), Some(offer.clone()), "same frame again");
        }
        s.handle_frame(&reply(1, b"NACK\n"), t);
        assert!(s.is_finished());
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events,
            [
                Event::PhaseChanged(Phase::Offering),
                Event::Failed(FailReason::Rejected { phase: "offering" })
            ]
        );
        // A device that accepts the second offer goes on normally.
        let mut s = ServerSession::new(device(), &plan(), cfg.clone(), now).unwrap();
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, b"NACK\n"), now);
        let t = now + cfg.offer_retry;
        assert_eq!(s.poll_timeout(), Some(t));
        assert_eq!(s.poll_transmit(t).map(|f| f.seq), Some(1));
        s.handle_frame(&reply(1, message::YACK), t);
        assert_eq!(s.poll_transmit(t).map(|f| f.seq), Some(2));
    }

    #[test]
    fn nack_after_the_offer_fails_with_rejected() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, message::YACK), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(2, b"NACK\nno space\n"), now);
        assert!(s.is_finished());
        assert_eq!(s.phase(), &Phase::Failed);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::Rejected {
                phase: "formatting"
            }))
        );
    }

    /// Answer every frame of `s` with what `answer` returns for its payload
    /// until the session ends; returns the payloads sent (data frames as
    /// `<len>`) with their seq.
    fn run(
        s: &mut ServerSession,
        now: Instant,
        mut answer: impl FnMut(&[u8]) -> &'static [u8],
    ) -> Vec<(u16, String)> {
        let mut out = Vec::new();
        let mut t = now;
        while !s.is_finished() {
            match s.poll_transmit(t) {
                Some(f) => {
                    let p = &f.payload[..];
                    let shown = if p.is_empty() || p.ends_with(b"\0") || p == message::FILE_END {
                        String::from_utf8_lossy(p).into_owned()
                    } else {
                        format!("<{}>", p.len())
                    };
                    out.push((f.seq, shown));
                    if !p.starts_with(b"TERM") {
                        s.handle_frame(&reply(f.seq, answer(p)), t);
                    }
                }
                None => t = s.poll_timeout().unwrap(),
            }
        }
        out
    }

    /// The device sim.py emulates: `STRT` to the format frame, `RETR` to
    /// the prepare frame, then `RETR` to the first poll after a file and
    /// `WTRM` to a poll right after another poll.
    fn official_device() -> impl FnMut(&[u8]) -> &'static [u8] {
        let (mut empties, mut polled) = (0, false);
        move |p: &[u8]| {
            if p.starts_with(b"OFFR") {
                return message::YACK;
            }
            if !p.is_empty() {
                polled = false;
                return message::RETR;
            }
            empties += 1;
            let answer = match empties {
                1 => message::STRT,
                2 => message::RETR,
                _ if polled => message::WTRM,
                _ => message::RETR,
            };
            polled = empties > 2;
            answer
        }
    }

    #[test]
    fn official_flow_with_a_script_and_a_mode_script() {
        let now = Instant::now();
        let mut p = plan();
        p.packages[0].data = npk(1500);
        p.config = DeviceConfig::Script(Bytes::from_static(b"/x\n"));
        p.mode_script = Some(Bytes::from_static(b":y\n"));
        let mut s = session(&p, now);
        let frames = run(&mut s, now, official_device());
        let want: Vec<(u16, &str)> = vec![
            (1, "OFFR\n\n\nremoveBranding\n\0"),
            (2, ""),
            (3, ""),
            (4, "FILE\nrouteros.npk\n1500\n\0"),
            (5, "<1024>"),
            (6, "<476>"),
            (7, ""),
            (8, "FILE\nautorun.scr\n3\n\0"),
            (9, "<3>"),
            (10, ""),
            (11, "FILE\nmode.scr\n3\n\0"),
            (12, "<3>"),
            (13, ""),
            (14, ""),
            (15, "TERM\n\0"),
        ];
        let got: Vec<(u16, &str)> = frames.iter().map(|(q, p)| (*q, p.as_str())).collect();
        assert_eq!(got, want);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(events.last(), Some(&Event::Done));
    }

    #[test]
    fn empty_config_sends_a_header_only_autorun() {
        let now = Instant::now();
        let mut p = plan();
        p.config = DeviceConfig::Empty;
        let mut s = session(&p, now);
        let frames = run(&mut s, now, official_device());
        let tail: Vec<&str> = frames[3..].iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(
            tail,
            [
                "FILE\nrouteros.npk\n10\n\0",
                "<10>",
                "",
                "FILE\nautorun.scr\n0\n\0",
                "",
                "",
                "TERM\n\0"
            ]
        );
    }

    #[test]
    fn wtrm_before_the_last_file_fails_with_ended_early() {
        let now = Instant::now();
        let mut p = plan();
        p.packages.push(file("extra.npk", npk(5)));
        p.config = DeviceConfig::Empty;
        let mut s = session(&p, now);
        let frames = run(&mut s, now, |payload| {
            if payload.starts_with(b"OFFR") {
                message::YACK
            } else if payload.is_empty() {
                message::WTRM
            } else {
                message::RETR
            }
        });
        // OFFR, format, prepare: WTRM to the format/prepare polls is just
        // "any reply"; the poll after routeros.npk is where it counts.
        assert_eq!(frames.last().map(|(_, p)| p.as_str()), Some(""));
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::EndedEarly { remaining: 2 }))
        );
        assert_eq!(
            FailReason::EndedEarly { remaining: 2 }.code(),
            "ended_early"
        );
    }

    #[test]
    fn a_device_that_never_says_wtrm_gets_file_end_after_the_polls() {
        let now = Instant::now();
        let cfg = SessionConfig {
            finish_polls: 3,
            ..SessionConfig::default()
        };
        let mut s = ServerSession::new(device(), &plan(), cfg.clone(), now).unwrap();
        let mut times = Vec::new();
        let mut t = now;
        let mut payloads = Vec::new();
        while !s.is_finished() {
            match s.poll_transmit(t) {
                Some(f) => {
                    payloads.push(f.payload.to_vec());
                    times.push(t);
                    let answer: &[u8] = if f.payload.starts_with(b"OFFR") {
                        message::YACK
                    } else {
                        message::RETR
                    };
                    s.handle_frame(&reply(f.seq, answer), t);
                }
                None => t = s.poll_timeout().unwrap(),
            }
        }
        let tail: Vec<&[u8]> = payloads[5..].iter().map(Vec::as_slice).collect();
        assert_eq!(
            tail,
            [&b""[..], b"", b"", message::FILE_END, b"TERM\n\0"],
            "3 polls, FILE\\n, TERM"
        );
        // The first poll follows the file at once, the others are paced.
        assert_eq!(times[5] - times[4], Duration::ZERO);
        assert_eq!(times[6] - times[5], cfg.poll_interval);
        assert_eq!(times[7] - times[6], cfg.poll_interval);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(events.last(), Some(&Event::Done));
    }

    #[test]
    fn finish_polls_are_clamped() {
        let now = Instant::now();
        for (polls, sent) in [(0, 1), (MAX_FINISH_POLLS + 5, MAX_FINISH_POLLS as usize)] {
            let cfg = SessionConfig {
                finish_polls: polls,
                poll_interval: Duration::ZERO,
                ..SessionConfig::default()
            };
            let mut s = ServerSession::new(device(), &plan(), cfg, now).unwrap();
            let frames = run(&mut s, now, |p| {
                if p.starts_with(b"OFFR") {
                    message::YACK
                } else {
                    message::RETR
                }
            });
            let empties_after_data = frames[5..].iter().filter(|(_, p)| p.is_empty()).count();
            assert_eq!(empties_after_data, sent, "finish_polls = {polls}");
        }
    }

    #[test]
    fn failure_emits_failed_event_but_no_phase_change() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, message::YACK), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(2, b"NACK"), now);
        assert_eq!(s.phase(), &Phase::Failed);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events,
            [
                Event::PhaseChanged(Phase::Offering),
                Event::PhaseChanged(Phase::Formatting),
                Event::Failed(FailReason::Rejected {
                    phase: "formatting"
                })
            ]
        );
    }

    #[test]
    fn cancel_fails_an_unfinished_session_once() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.cancel();
        assert!(s.is_finished());
        assert_eq!(s.phase(), &Phase::Failed);
        assert_eq!(s.poll_transmit(now), None);
        assert_eq!(s.poll_timeout(), None);
        s.cancel();
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events,
            [
                Event::PhaseChanged(Phase::Offering),
                Event::Failed(FailReason::Cancelled)
            ]
        );
        assert_eq!(FailReason::Cancelled.code(), "cancelled");
    }

    #[test]
    fn default_timeouts() {
        let cfg = SessionConfig::default();
        assert_eq!(cfg.optional_timeout, Duration::from_secs(10));
        assert_eq!(cfg.offer_retry, Duration::from_secs(1));
        assert_eq!(cfg.finish_polls, 20);
    }

    #[test]
    fn upload_phase_and_progress_name_the_file() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        for (seq, answer) in [(1, message::YACK), (2, message::STRT), (3, message::RETR)] {
            s.poll_transmit(now).unwrap();
            s.handle_frame(&reply(seq, answer), now);
        }
        s.poll_transmit(now).unwrap(); // header
        assert_eq!(
            s.phase(),
            &Phase::Uploading {
                file: Arc::from("routeros.npk")
            }
        );
        s.handle_frame(&reply(4, message::RETR), now);
        let chunk = s.poll_transmit(now).unwrap();
        assert_eq!((chunk.seq, chunk.payload.len()), (5, 10));
        s.handle_frame(&reply(5, message::RETR), now);
        let progress = std::iter::from_fn(|| s.poll_event()).find_map(|e| match e {
            Event::Progress {
                file,
                file_sent,
                bytes_sent,
                bytes_total,
                ..
            } => Some((file, file_sent, bytes_sent, bytes_total)),
            _ => None,
        });
        assert_eq!(progress, Some((Arc::from("routeros.npk"), 10, 10, 10)));
        let poll = s.poll_transmit(now).expect("poll right after the file");
        assert!(poll.payload.is_empty());
        assert_eq!(s.phase(), &Phase::Finishing);
    }
}
```

- [ ] **Step 17: Edit `crates/ni-proto/src/session/steps.rs`**

1. In `crates/ni-proto/src/session/steps.rs`, replace:

```rust
//! Translation of an [`InstallPlan`] into the list of frames to send.

use super::{InstallPlan, Phase};
use crate::frame::MAX_CHUNK;
use crate::message;
```

with:

```rust
//! Translation of an [`InstallPlan`] into the list of frames to send.

use super::{InstallPlan, Phase, MAX_FINISH_POLLS};
use crate::frame::MAX_CHUNK;
use crate::message;
```

2. In `crates/ni-proto/src/session/steps.rs`, replace:

```rust
    /// Do not wait for a reply.
    None,
    /// Any reply with matching sequence numbers.
    Any,
    /// Like `Any`, but once retransmits are exhausted the session moves on.
    Optional,
    /// Two state-matched replies: the first with any payload, the second
    /// exactly this one (later mismatching replies are ignored). Mirrors
    /// pyNetinstall `do_file`, which reads twice after the last chunk and
    /// requires the second read to be `RETR`. The count is kept across
    /// retransmits, so the second reply can come from the device's periodic
    /// re-send of its last answer or from its answer to our retransmit
    /// (a de-duplicating device repeats it); waiting cannot deadlock.
    SecondIs(&'static [u8]),
}

```

with:

```rust
    /// Do not wait for a reply.
    None,
    /// The offer: `YACK` (any reply but `NACK`) moves on; `NACK` makes the
    /// session offer again.
    Offer,
    /// Any reply with matching sequence numbers.
    Any,
    /// Like `Any`, but once retransmits are exhausted the session moves on.
    Optional,
    /// The empty poll after a file. `remaining` files are still to be
    /// sent: then the device must not answer `WTRM`. After the last file
    /// (`remaining == 0`) `WTRM` ends the upload and anything else is
    /// polled again.
    Poll { remaining: usize },
}

```

3. In `crates/ni-proto/src/session/steps.rs`, replace:

```rust

impl Step {
    fn control(phase: Phase, payload: &'static [u8], expect: Expect, timeout: TimeoutKind) -> Self {
        Self {
            phase,
            payload: Bytes::from_static(payload),
            expect,
            timeout,
```

with:

```rust

impl Step {
    fn control(phase: Phase, payload: Bytes, expect: Expect, timeout: TimeoutKind) -> Self {
        Self {
            phase,
            payload,
            expect,
            timeout,
```

4. In `crates/ni-proto/src/session/steps.rs`, replace:

```rust
}

/// Number of file chunks `push_file` emits for `len` bytes (at least one).
fn chunk_count(len: usize) -> usize {
    len.div_ceil(MAX_CHUNK).max(1)
}

/// Number of frames [`build_steps`] produces for `plan`, without building it.
pub(super) fn frame_count(plan: &InstallPlan) -> usize {
    // OFFR, format, prepare / final empty, FILE\n, TERM
    let fixed = 6;
    // header + chunks + spacer
    let packages: usize = plan
        .packages
        .iter()
        .map(|p| 2 + chunk_count(p.data.len()))
        .sum();
    // header + chunks
    let script = plan
        .config_script
        .as_ref()
        .map_or(0, |s| 1 + chunk_count(s.len()));
    fixed + packages + script
}

pub(super) fn build_steps(plan: &InstallPlan) -> Vec<Step> {
    use Expect::Any;
    use TimeoutKind::{Format, Reply};

    let mut steps = vec![
        Step::control(Phase::Offering, message::OFFR, Any, Reply),
        Step::control(Phase::Formatting, b"", Any, Format),
        Step::control(Phase::Preparing, b"", Any, Reply),
    ];
    for pkg in &plan.packages {
        let name: Arc<str> = Arc::from(pkg.name.as_str());
        push_file(&mut steps, Arc::clone(&name), &pkg.data);
        let phase = Phase::Uploading { file: name };
        steps.push(Step::control(phase, b"", Any, Reply));
    }
    if let Some(script) = &plan.config_script {
        push_file(&mut steps, Arc::from(message::CONFIG_SCRIPT_NAME), script);
    }
    steps.push(Step::control(Phase::Finishing, b"", Any, Reply));
    steps.push(Step::control(
        Phase::Finishing,
        message::FILE_END,
        Any,
        Reply,
```

with:

````rust
}

/// Number of data frames for a file of `len` bytes (none for an empty one).
fn chunk_count(len: usize) -> usize {
    len.div_ceil(MAX_CHUNK)
}

/// Most frames (new sequence numbers) [`build_steps`] can lead to for
/// `plan`: every step once, plus up to [`MAX_FINISH_POLLS`] polls after
/// the last file.
pub(super) fn frame_count(plan: &InstallPlan) -> usize {
    // OFFR, format, prepare, FILE\n, TERM
    let fixed = 5;
    // header + chunks + poll
    let files: usize = plan
        .files()
        .iter()
        .map(|(_, data)| 2 + chunk_count(data.len()))
        .sum();
    fixed + files + (MAX_FINISH_POLLS as usize - 1)
}

/// The steps for `plan`, in the official tool's order:
///
/// ```text
/// OFFR → "" (format) → "" (prepare)
///   → per file: FILE header → chunks → "" (poll)
///   → [FILE\n, only if the device never answers a poll with WTRM] → TERM
/// ```
///
/// The last step is always `TERM`, the one before it `FILE\n`.
pub(super) fn build_steps(plan: &InstallPlan) -> Vec<Step> {
    use Expect::Any;
    use TimeoutKind::{Format, Reply};

    let offer = Bytes::from(message::offer(&plan.offer()));
    let mut steps = vec![
        Step::control(Phase::Offering, offer, Expect::Offer, Reply),
        Step::control(Phase::Formatting, Bytes::new(), Any, Format),
        Step::control(Phase::Preparing, Bytes::new(), Any, Reply),
    ];
    let files = plan.files();
    for (i, (name, data)) in files.iter().enumerate() {
        let name: Arc<str> = Arc::from(name.as_str());
        push_file(&mut steps, Arc::clone(&name), data);
        let remaining = files.len() - 1 - i;
        let phase = if remaining == 0 {
            Phase::Finishing
        } else {
            Phase::Uploading { file: name }
        };
        steps.push(Step::control(
            phase,
            Bytes::new(),
            Expect::Poll { remaining },
            Reply,
        ));
    }
    steps.push(Step::control(
        Phase::Finishing,
        Bytes::from_static(message::FILE_END),
        Any,
        Reply,
````

5. In `crates/ni-proto/src/session/steps.rs`, replace:

```rust
    steps.push(Step::control(
        Phase::Rebooting,
        message::TERM,
        Expect::None,
        Reply,
```

with:

```rust
    steps.push(Step::control(
        Phase::Rebooting,
        Bytes::from_static(message::term(plan.after_install)),
        Expect::None,
        Reply,
```

6. In `crates/ni-proto/src/session/steps.rs`, replace:

```rust
}

fn push_file(steps: &mut Vec<Step>, name: Arc<str>, data: &Bytes) {
    let header = Bytes::from(message::file_header(&name, data.len() as u64));
```

with:

```rust
}

/// The header, then one frame per [`MAX_CHUNK`] bytes (an empty file has
/// no data frame).
fn push_file(steps: &mut Vec<Step>, name: Arc<str>, data: &Bytes) {
    let header = Bytes::from(message::file_header(&name, data.len() as u64));
```

7. In `crates/ni-proto/src/session/steps.rs`, replace:

```rust
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
                Expect::SecondIs(message::RETR)
            } else {
                Expect::Any
            },
            timeout: TimeoutKind::Reply,
            chunk: Some(ChunkInfo {
                file: Arc::clone(&name),
                file_sent: sent,
                file_total: total,
            }),
```

with:

```rust
        delay_after: false,
    });
    let count = chunk_count(data.len());
    for i in 0..count {
        let start = i * MAX_CHUNK;
        let chunk = data.slice(start..(start + MAX_CHUNK).min(data.len()));
        let is_last = i + 1 == count;
        steps.push(Step {
            phase: phase.clone(),
            payload: chunk,
            expect: Expect::Any,
            timeout: TimeoutKind::Reply,
            chunk: Some(ChunkInfo {
                file: Arc::clone(&name),
                file_sent: ((start + MAX_CHUNK).min(data.len())) as u64,
                file_total: total,
            }),
```

- [ ] **Step 18: Edit `crates/ni-sim/src/lan.rs`**

1. In `crates/ni-sim/src/lan.rs`, replace:

```rust

use crate::{DeviceSim, ReceivedFile};
use ni_proto::{DeviceInfo, Frame, Mac};
use ni_tftp::{Packet, Request};
use std::collections::{BTreeMap, BTreeSet};
```

with:

```rust

use crate::{DeviceSim, ReceivedFile};
use ni_proto::{AfterInstall, DeviceInfo, Frame, Mac, Offer};
use ni_tftp::{Packet, Request};
use std::collections::{BTreeMap, BTreeSet};
```

2. In `crates/ni-sim/src/lan.rs`, replace:

```rust
pub struct DeviceReport {
    pub received: Vec<ReceivedFile>,
    pub terminated: bool,
    pub violations: Vec<String>,
```

with:

```rust
pub struct DeviceReport {
    pub received: Vec<ReceivedFile>,
    /// The offer it accepted.
    pub offer: Option<Offer>,
    /// What `TERM` asked for, once received.
    pub after_install: Option<AfterInstall>,
    pub terminated: bool,
    pub violations: Vec<String>,
```

3. In `crates/ni-sim/src/lan.rs`, replace:

```rust
                    let report = devices.get(&mac).map(|d| DeviceReport {
                        received: d.sim.received_files().to_vec(),
                        terminated: d.sim.is_terminated(),
                        violations: d.sim.violations().to_vec(),
```

with:

```rust
                    let report = devices.get(&mac).map(|d| DeviceReport {
                        received: d.sim.received_files().to_vec(),
                        offer: d.sim.offer().cloned(),
                        after_install: d.sim.after_install(),
                        terminated: d.sim.is_terminated(),
                        violations: d.sim.violations().to_vec(),
```

- [ ] **Step 19: Edit `crates/ni-sim/src/lib.rs` (with its unit tests)**

Replace the whole of `crates/ni-sim/src/lib.rs` with:

````rust
//! Simulated MikroTik device in Netinstall mode (sans-IO).
//!
//! Behaviour mirrors the device the official `netinstall-cli` 7.25.1
//! expects (the spike's `sim.py`, which that server accepted in every
//! captured run): broadcasts 8-field hello frames until offered, answers
//! each server frame with `seq = ack = <server seq>`, puts zeros in
//! `[6:12]` of its replies, and periodically re-sends its last reply (real
//! devices do this too).
//!
//! The simulator is deliberately strict: it only accepts the official
//! frame sequence,
//!
//! ```text
//! OFFR\n<key>\n<keep>\n<brand>\n\0 → empty (format) → empty (prepare)
//!   → loop { FILE header → chunks (none for an empty file) → empty poll }
//!   → empty polls until it answers WTRM → TERM\n<message>\0
//! ```
//!
//! How it answers the polls after a file is [`SimConfig::after_files`]:
//! besides the official behaviour, a pyNetinstall-era device (`WTRM` only
//! to `FILE\n`), one that never says `WTRM`, and one that stops early.
//! Frames whose `ack` is not `seq - 2` (0 for the first two), stale frames
//! older than the last one accepted and frames from other devices are
//! ignored or flagged. Anything else out of line (wrong order, an offer
//! with malformed fields, oversized chunks, non-final chunks shorter than
//! `MAX_CHUNK`, more data than announced, a script before any package) is
//! answered with `NACK` and recorded in [`DeviceSim::violations`], so
//! step-order regressions fail tests.

pub mod harness;
#[cfg(feature = "lan")]
pub mod lan;

use bytes::Bytes;
use ni_proto::message::{self, AfterInstall, Offer};
use ni_proto::{DeviceInfo, Frame, Mac, MAX_CHUNK};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How the device answers the empty polls that follow a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFiles {
    /// `RETR` to the first poll after a file, `WTRM` to a poll right after
    /// another poll (inferred from the official server's behaviour; what
    /// `sim.py --after-files-reply auto` does).
    Official,
    /// `RETR` to every poll, `WTRM` only to pyNetinstall's `FILE\n`.
    PyNetinstall,
    /// `RETR` to every poll and to `FILE\n`: never `WTRM`.
    NeverWtrm,
    /// `WTRM` to the first poll after the first file: stops early.
    EarlyWtrm,
}

#[derive(Debug, Clone)]
pub struct SimConfig {
    pub info: DeviceInfo,
    pub hello_interval: Duration,
    pub resend_interval: Duration,
    /// Treat a re-sent server frame (same seq) as a duplicate and only repeat
    /// the previous answer. When `false` it is processed again as if new
    /// (a re-sent chunk is appended twice), modelling a device that does not
    /// de-duplicate. Default `true`.
    pub dedup: bool,
    /// How long formatting takes before the device answers `STRT`. Until
    /// then it only re-sends its previous reply. Default zero.
    pub format_delay: Duration,
    /// Refuse the install: answer `OFFR` with `NACK` (as a device short of
    /// space would) and keep saying hello. Default `false`.
    pub reject_offer: bool,
    /// Default [`AfterFiles::Official`].
    pub after_files: AfterFiles,
    /// `[6:12]` of its replies (`[6:12]` of a hello is always the
    /// broadcast address). Default zeros, the only value the official
    /// server handled cleanly (see the research doc, §5).
    pub reply_dst: Mac,
}

impl SimConfig {
    pub fn new(mac: Mac, model: &str, arch: &str) -> Self {
        Self {
            info: DeviceInfo {
                mac,
                tag: ni_proto::HELLO_TAG.into(),
                license_id: "SIM-0000".into(),
                license_key: String::new(),
                model: model.into(),
                arch: arch.into(),
                min_os: "7.0".into(),
                extra: vec!["0".into(), "0".into()],
            },
            hello_interval: Duration::from_secs(1),
            resend_interval: Duration::from_millis(500),
            dedup: true,
            format_delay: Duration::ZERO,
            reject_offer: false,
            after_files: AfterFiles::Official,
            reply_dst: Mac::ZERO,
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
    /// Broadcasting hellos; expects `OFFR`.
    Discovering,
    /// Expects the empty "format" frame.
    Offered,
    /// Formatting; silent (apart from re-sends) until `until`.
    Formatting {
        until: Instant,
    },
    /// Expects the empty "prepare" frame.
    Formatted,
    /// Expects the first `FILE` header.
    Ready,
    Receiving {
        name: String,
        size: u64,
        data: Vec<u8>,
    },
    /// A file was received; expects polls, the next `FILE` header (after a
    /// poll) or `FILE\n`. `polls`: polls since the file.
    AfterFile {
        polls: u32,
    },
    /// Said `WTRM`; expects `TERM`.
    Finished,
    Terminated,
}

/// Outcome of processing one in-order server frame.
type Step = Result<(State, Vec<&'static [u8]>), (State, String)>;

pub struct DeviceSim {
    cfg: SimConfig,
    state: State,
    last_seq: Option<u16>,
    last_reply: Option<Frame>,
    tx: VecDeque<Frame>,
    next_timer: Option<Instant>,
    received: Vec<ReceivedFile>,
    offer: Option<Offer>,
    after_install: Option<AfterInstall>,
    violations: Vec<String>,
}

impl DeviceSim {
    pub fn new(cfg: SimConfig, now: Instant) -> Self {
        Self {
            cfg,
            state: State::Discovering,
            last_seq: None,
            last_reply: None,
            tx: VecDeque::new(),
            next_timer: Some(now),
            received: Vec::new(),
            offer: None,
            after_install: None,
            violations: Vec::new(),
        }
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.cfg.info
    }

    pub fn config(&self) -> &SimConfig {
        &self.cfg
    }

    pub fn received_files(&self) -> &[ReceivedFile] {
        &self.received
    }

    /// The offer the device accepted.
    pub fn offer(&self) -> Option<&Offer> {
        self.offer.as_ref()
    }

    /// What the `TERM` frame asked for, once received.
    pub fn after_install(&self) -> Option<AfterInstall> {
        self.after_install
    }

    /// Protocol violations by the server, in order of occurrence.
    pub fn violations(&self) -> &[String] {
        &self.violations
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
            State::Formatting { until } if until <= now => {
                self.state = State::Formatted;
                if let Some(seq) = self.last_seq {
                    self.reply(seq, message::STRT);
                }
                self.schedule(now);
            }
            _ => {
                if let Some(reply) = &self.last_reply {
                    self.tx.push_back(reply.clone());
                }
                self.schedule(now);
            }
        }
    }

    pub fn handle_frame(&mut self, frame: &Frame, now: Instant) {
        let me = self.cfg.info.mac;
        if self.state == State::Terminated || frame.dst != me || frame.src == me {
            return;
        }
        if self.last_seq.is_some_and(|last| frame.seq < last) {
            // Stale or delayed duplicate of an older frame.
            return;
        }
        let ack = frame.seq.saturating_sub(2);
        if frame.ack != ack {
            self.violation(format!(
                "frame seq {} carries ack {}, expected {ack}",
                frame.seq, frame.ack
            ));
            self.nack(frame.seq);
            return;
        }
        let repeated = self.last_seq == Some(frame.seq);
        if repeated && self.cfg.dedup {
            // Server re-sent its last frame: repeat our answer.
            if let Some(reply) = &self.last_reply {
                self.tx.push_back(reply.clone());
            }
            return;
        }
        let next = self.last_seq.map_or(1, |s| s.wrapping_add(1));
        if frame.seq != next && !repeated {
            self.violation(format!("frame seq {}, expected {next}", frame.seq));
            self.nack(frame.seq);
            return;
        }
        let state = std::mem::replace(&mut self.state, State::Discovering);
        let replies = match self.process(state, &frame.payload, now) {
            Ok((next, replies)) => {
                self.state = next;
                replies
            }
            Err((prev, why)) => {
                self.state = prev;
                self.violation(format!("seq {}: {why}", frame.seq));
                self.nack(frame.seq);
                return;
            }
        };
        if replies.first() == Some(&message::NACK) {
            // A refused offer: answered, but the same frame may come again.
            self.nack(frame.seq);
            self.schedule(now);
            return;
        }
        self.last_seq = Some(frame.seq);
        for payload in replies {
            self.reply(frame.seq, payload);
        }
        self.schedule(now);
    }

    fn process(&mut self, state: State, payload: &[u8], now: Instant) -> Step {
        use message::{RETR, STRT, WTRM, YACK};
        let what = describe(payload);
        match state {
            State::Discovering => match message::parse_offer(payload) {
                Some(_) if self.cfg.reject_offer => Ok((State::Discovering, vec![message::NACK])),
                Some(offer) => {
                    self.offer = Some(offer);
                    Ok((State::Offered, vec![YACK]))
                }
                None => Err((State::Discovering, format!("expected OFFR, got {what}"))),
            },
            State::Offered if payload.is_empty() => {
                if self.cfg.format_delay.is_zero() {
                    Ok((State::Formatted, vec![STRT]))
                } else {
                    let until = now + self.cfg.format_delay;
                    Ok((State::Formatting { until }, vec![]))
                }
            }
            State::Formatted if payload.is_empty() => Ok((State::Ready, vec![RETR])),
            State::Ready => self.header(State::Ready, payload),
            State::AfterFile { polls } if payload.is_empty() => {
                let wtrm = match self.cfg.after_files {
                    AfterFiles::Official => polls > 0,
                    AfterFiles::PyNetinstall | AfterFiles::NeverWtrm => false,
                    AfterFiles::EarlyWtrm => true,
                };
                if wtrm {
                    Ok((State::Finished, vec![WTRM]))
                } else {
                    Ok((State::AfterFile { polls: polls + 1 }, vec![RETR]))
                }
            }
            State::AfterFile { polls } if payload == message::FILE_END => {
                match self.cfg.after_files {
                    AfterFiles::PyNetinstall => Ok((State::Finished, vec![WTRM])),
                    AfterFiles::NeverWtrm => Ok((State::Finished, vec![RETR])),
                    AfterFiles::Official | AfterFiles::EarlyWtrm => Err((
                        State::AfterFile { polls },
                        "FILE\\n although the device would have said WTRM".into(),
                    )),
                }
            }
            State::AfterFile { polls } if polls == 0 => Err((
                State::AfterFile { polls },
                format!("expected the poll after a file, got {what}"),
            )),
            State::AfterFile { polls } => self.header(State::AfterFile { polls }, payload),
            State::Receiving {
                name,
                size,
                mut data,
            } => {
                let error = if payload.len() > MAX_CHUNK {
                    Some(format!(
                        "chunk of {} bytes exceeds {MAX_CHUNK}",
                        payload.len()
                    ))
                } else if payload.is_empty() {
                    Some("empty chunk while receiving".to_string())
                } else if ((data.len() + payload.len()) as u64) < size && payload.len() != MAX_CHUNK
                {
                    Some(format!(
                        "non-final chunk of {} bytes, expected {MAX_CHUNK}",
                        payload.len()
                    ))
                } else if (data.len() + payload.len()) as u64 > size {
                    Some(format!(
                        "{name}: {} bytes received, only {size} announced",
                        data.len() + payload.len()
                    ))
                } else {
                    None
                };
                if let Some(why) = error {
                    return Err((State::Receiving { name, size, data }, why));
                }
                data.extend_from_slice(payload);
                if (data.len() as u64) < size {
                    return Ok((State::Receiving { name, size, data }, vec![RETR]));
                }
                self.received.push(ReceivedFile { name, data });
                Ok((State::AfterFile { polls: 0 }, vec![RETR]))
            }
            State::Finished => match message::parse_term(payload) {
                Some(after) => {
                    self.after_install = Some(after);
                    Ok((State::Terminated, vec![]))
                }
                None => Err((State::Finished, format!("expected TERM, got {what}"))),
            },
            other => {
                let why = format!("unexpected {what} in state {}", state_name(&other));
                Err((other, why))
            }
        }
    }

    /// A `FILE` header: start receiving (an empty file is complete at once).
    fn header(&mut self, state: State, payload: &[u8]) -> Step {
        let Some((name, size)) = message::parse_file_header(payload) else {
            let why = format!("expected FILE header, got {}", describe(payload));
            return Err((state, why));
        };
        let script = name == message::CONFIG_SCRIPT_NAME || name == message::MODE_SCRIPT_NAME;
        if self.received.is_empty() && script {
            return Err((state, "script before any package".into()));
        }
        if size == 0 {
            self.received.push(ReceivedFile {
                name,
                data: Vec::new(),
            });
            return Ok((State::AfterFile { polls: 0 }, vec![message::RETR]));
        }
        let data = Vec::new();
        Ok((State::Receiving { name, size, data }, vec![message::RETR]))
    }

    fn reply(&mut self, seq: u16, payload: &'static [u8]) {
        let reply = Frame::new(
            self.cfg.info.mac,
            self.cfg.reply_dst,
            seq,
            seq,
            Bytes::from_static(payload),
        );
        self.last_reply = Some(reply.clone());
        self.tx.push_back(reply);
    }

    /// Answer `NACK` without treating the frame as accepted (it does not
    /// become the reply that is re-sent).
    fn nack(&mut self, seq: u16) {
        let reply = Frame::new(
            self.cfg.info.mac,
            self.cfg.reply_dst,
            seq,
            seq,
            Bytes::from_static(message::NACK),
        );
        self.tx.push_back(reply);
    }

    fn violation(&mut self, why: String) {
        self.violations.push(why);
    }

    fn schedule(&mut self, now: Instant) {
        self.next_timer = match self.state {
            State::Terminated => None,
            State::Discovering => Some(now + self.cfg.hello_interval),
            State::Formatting { until } => Some(until.min(now + self.cfg.resend_interval)),
            _ => Some(now + self.cfg.resend_interval),
        };
    }
}

fn state_name(state: &State) -> &'static str {
    match state {
        State::Discovering => "Discovering",
        State::Offered => "Offered",
        State::Formatting { .. } => "Formatting",
        State::Formatted => "Formatted",
        State::Ready => "Ready",
        State::Receiving { .. } => "Receiving",
        State::AfterFile { .. } => "AfterFile",
        State::Finished => "Finished",
        State::Terminated => "Terminated",
    }
}

/// Short human-readable description of a server payload.
fn describe(payload: &[u8]) -> String {
    if payload.is_empty() {
        return "empty frame".into();
    }
    if let Some((name, size)) = message::parse_file_header(payload) {
        return format!("FILE header {name} ({size} bytes)");
    }
    let head = &payload[..payload.len().min(8)];
    format!(
        "{} byte frame {:?}",
        payload.len(),
        String::from_utf8_lossy(head)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_proto::MAX_CHUNK;

    const DEVICE: Mac = Mac([0xaa; 6]);
    const OFFER: &[u8] = b"OFFR\n\nkeep\nremoveBranding\n\0";
    const TERM: &[u8] = b"TERM\n\0";

    fn sim_with(f: impl FnOnce(&mut SimConfig)) -> (DeviceSim, Instant) {
        let now = Instant::now();
        let mut cfg = SimConfig::new(DEVICE, "hEX", "mmips");
        f(&mut cfg);
        let mut sim = DeviceSim::new(cfg, now);
        sim.handle_timeout(now);
        assert!(sim.poll_transmit().is_some(), "hello");
        (sim, now)
    }

    /// Feed server frames with consecutive seq numbers starting at `*seq + 1`
    /// (`[0:6]` zeros, `ack = seq - 2`) and return the payloads of all replies.
    fn feed(sim: &mut DeviceSim, seq: &mut u16, payloads: &[&[u8]], now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for p in payloads {
            *seq += 1;
            let f = Frame::new(Mac::ZERO, DEVICE, *seq, seq.saturating_sub(2), p.to_vec());
            sim.handle_frame(&f, now);
            while let Some(r) = sim.poll_transmit() {
                assert_eq!((r.seq, r.ack), (*seq, *seq));
                assert_eq!((r.src, r.dst), (DEVICE, Mac::ZERO));
                out.push(r.payload.to_vec());
            }
        }
        out
    }

    fn header(name: &str, size: u64) -> Vec<u8> {
        message::file_header(name, size)
    }

    fn strs(replies: &[Vec<u8>]) -> Vec<&str> {
        replies
            .iter()
            .map(|r| std::str::from_utf8(r).unwrap())
            .collect()
    }

    #[test]
    fn hello_has_eight_fields_to_the_broadcast_slot() {
        let now = Instant::now();
        let mut sim = DeviceSim::new(SimConfig::new(DEVICE, "hEX", "mmips"), now);
        sim.handle_timeout(now);
        let hello = sim.poll_transmit().unwrap();
        assert_eq!(
            (hello.src, hello.dst, hello.seq, hello.ack),
            (DEVICE, Mac::BROADCAST, 1, 0)
        );
        assert_eq!(
            &hello.payload[..],
            b"DSCV\nSIM-0000\n\nhEX\nmmips\n7.0\n0\n0\n"
        );
    }

    #[test]
    fn accepts_the_official_sequence() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        let h1 = header("a.npk", 3);
        let h2 = header(message::CONFIG_SCRIPT_NAME, 2);
        let h3 = header(message::MODE_SCRIPT_NAME, 0);
        let replies = feed(
            &mut sim,
            &mut seq,
            &[
                OFFER, b"", b"", &h1, b"abc", b"", &h2, b"xy", b"", &h3, b"", b"", TERM,
            ],
            now,
        );
        assert_eq!(
            strs(&replies),
            [
                "YACK\n", "STRT", "RETR", "RETR", "RETR", "RETR", "RETR", "RETR", "RETR", "RETR",
                "RETR", "WTRM"
            ]
        );
        assert!(sim.is_terminated());
        assert!(sim.violations().is_empty(), "{:?}", sim.violations());
        let names: Vec<(&str, usize)> = sim
            .received_files()
            .iter()
            .map(|f| (f.name.as_str(), f.data.len()))
            .collect();
        assert_eq!(names, [("a.npk", 3), ("autorun.scr", 2), ("mode.scr", 0)]);
        assert_eq!(
            sim.offer(),
            Some(&Offer {
                license_key: String::new(),
                keep_config: true,
                remove_branding: false
            })
        );
        assert_eq!(sim.after_install(), Some(AfterInstall::Reboot));
    }

    #[test]
    fn other_after_files_behaviours() {
        let h1 = header("a.npk", 1);
        let h2 = header("b.npk", 1);
        let run = |after: AfterFiles, frames: &[&[u8]]| {
            let (mut sim, now) = sim_with(|c| c.after_files = after);
            let mut seq = 0;
            let mut all: Vec<&[u8]> = vec![OFFER, b"", b"", &h1, b"a"];
            all.extend_from_slice(frames);
            let r = feed(&mut sim, &mut seq, &all, now);
            let tail: Vec<String> = strs(&r[5..]).into_iter().map(String::from).collect();
            (tail, sim.is_terminated(), sim.violations().to_vec())
        };
        let (r, done, v) = run(
            AfterFiles::PyNetinstall,
            &[b"", b"", b"", message::FILE_END, TERM],
        );
        assert_eq!(
            (r, done, v.len()),
            (
                vec!["RETR".into(), "RETR".into(), "RETR".into(), "WTRM".into()],
                true,
                0
            )
        );
        let (r, done, v) = run(AfterFiles::NeverWtrm, &[b"", b"", message::FILE_END, TERM]);
        assert_eq!(
            (r, done, v.len()),
            (vec!["RETR".into(), "RETR".into(), "RETR".into()], true, 0)
        );
        let (r, done, _) = run(AfterFiles::EarlyWtrm, &[b"", &h2]);
        assert_eq!(r, ["WTRM", "NACK"]);
        assert!(!done);
        let (r, _, v) = run(AfterFiles::Official, &[b"", b"", message::FILE_END]);
        assert_eq!(r, ["RETR", "WTRM", "NACK"]);
        assert_eq!(v.len(), 1, "FILE\\n after WTRM is a violation");
    }

    #[test]
    fn reject_offer_answers_every_offer_with_nack() {
        let (mut sim, now) = sim_with(|c| c.reject_offer = true);
        let r = feed(&mut sim, &mut 0, &[OFFER], now);
        assert_eq!(r, [message::NACK.to_vec()]);
        // The server offers again with the same seq: refused again.
        let again = Frame::new(Mac::BROADCAST, DEVICE, 1, 0, OFFER);
        sim.handle_frame(&again, now);
        assert_eq!(
            sim.poll_transmit().map(|f| f.payload.to_vec()),
            Some(b"NACK".to_vec())
        );
        assert!(sim.violations().is_empty(), "a refusal is not a violation");
        assert!(!sim.is_terminated());
        assert!(sim.received_files().is_empty());
        assert!(sim.offer().is_none());
    }

    #[test]
    fn malformed_offers_and_pynetinstall_offers_are_violations() {
        for bad in [message::OFFR, b"OFFR\n\nmaybe\n\n\0".as_slice()] {
            let (mut sim, now) = sim_with(|_| {});
            let r = feed(&mut sim, &mut 0, &[bad], now);
            assert_eq!(r, [message::NACK.to_vec()]);
            assert_eq!(sim.violations().len(), 1);
        }
    }

    #[test]
    fn missing_poll_after_a_file_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let h1 = header("a.npk", 3);
        let h2 = header("b.npk", 1);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", b"", &h1, b"abc", &h2], now);
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1, "{:?}", sim.violations());
    }

    #[test]
    fn a_script_first_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let h = header(message::MODE_SCRIPT_NAME, 1);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", b"", &h], now);
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1);
    }

    #[test]
    fn skipping_the_format_step_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let h1 = header("a.npk", 1);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", &h1], now);
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1);
    }

    #[test]
    fn wrong_ack_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        sim.handle_frame(&Frame::new(Mac::BROADCAST, DEVICE, 1, 1, OFFER), now);
        assert_eq!(sim.violations().len(), 1);
        assert_eq!(
            sim.poll_transmit().map(|f| f.payload.to_vec()),
            Some(message::NACK.to_vec())
        );
        // pyNetinstall's ack = seq - 1 is wrong from the second frame on.
        let (mut sim, now) = sim_with(|_| {});
        feed(&mut sim, &mut 0, &[OFFER], now);
        sim.handle_frame(&Frame::new(Mac::ZERO, DEVICE, 2, 1, &b""[..]), now);
        assert_eq!(sim.violations().len(), 1);
    }

    #[test]
    fn empty_chunk_oversized_chunk_and_overshoot_are_violations() {
        let h1 = header("a.npk", 5000);
        let big = vec![0u8; MAX_CHUNK + 1];
        let h2 = header("a.npk", 3);
        for frames in [
            vec![OFFER, b"", b"", h1.as_slice(), big.as_slice()],
            vec![OFFER, b"", b"", h2.as_slice(), b"abcd"],
            vec![OFFER, b"", b"", h2.as_slice(), b""],
        ] {
            let (mut sim, now) = sim_with(|_| {});
            let r = feed(&mut sim, &mut 0, &frames, now);
            assert_eq!(r.last().unwrap().as_slice(), message::NACK);
            assert_eq!(sim.violations().len(), 1);
        }
    }

    #[test]
    fn stale_frames_are_ignored_silently() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        feed(&mut sim, &mut seq, &[OFFER, b"", b""], now);
        // A delayed duplicate of an older frame.
        sim.handle_frame(&Frame::new(Mac::BROADCAST, DEVICE, 1, 0, OFFER), now);
        assert_eq!(sim.poll_transmit(), None);
        assert!(sim.violations().is_empty());
    }

    #[test]
    fn frames_for_other_devices_and_own_frames_are_ignored() {
        let (mut sim, now) = sim_with(|_| {});
        let other = Mac([0x02, 0, 0, 0, 0, 0x99]);
        sim.handle_frame(&Frame::new(Mac::BROADCAST, other, 1, 0, OFFER), now);
        sim.handle_frame(&Frame::new(DEVICE, DEVICE, 1, 0, OFFER), now);
        assert_eq!(sim.poll_transmit(), None);
        assert!(sim.violations().is_empty());
        let r = feed(&mut sim, &mut 0, &[OFFER], now);
        assert_eq!(r, [message::YACK.to_vec()]);
    }

    #[test]
    fn short_non_final_chunk_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let h1 = header("a.npk", 2000);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", b"", &h1, &[1u8; 1000]], now);
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1, "{:?}", sim.violations());
    }

    #[test]
    fn the_last_chunk_is_answered_once() {
        let (mut sim, now) = sim_with(|_| {});
        let h1 = header("a.npk", 3);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", b"", &h1, b"abc"], now);
        // YACK, STRT, RETR (prepare), RETR (header), one RETR for the chunk.
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn duplicate_frames_are_deduplicated_unless_disabled() {
        for dedup in [true, false] {
            let (mut sim, now) = sim_with(|c| c.dedup = dedup);
            let mut seq = 0;
            let h1 = header("a.npk", 2 * MAX_CHUNK as u64);
            let chunk = vec![7u8; MAX_CHUNK];
            feed(&mut sim, &mut seq, &[OFFER, b"", b"", &h1, &chunk], now);
            // Re-send the chunk with the same seq.
            sim.handle_frame(
                &Frame::new(Mac::ZERO, DEVICE, seq, seq - 2, chunk.clone()),
                now,
            );
            while sim.poll_transmit().is_some() {}
            let twice = [chunk.clone(), chunk.clone()].concat();
            let expected: &[u8] = if dedup { b"" } else { &twice };
            let got: Vec<u8> = sim
                .received_files()
                .first()
                .map_or(Vec::new(), |f| f.data.clone());
            assert_eq!(got, expected, "dedup = {dedup}");
            assert!(sim.violations().is_empty());
        }
    }

    #[test]
    fn format_delay_postpones_strt() {
        let (mut sim, now) = sim_with(|c| c.format_delay = Duration::from_secs(30));
        let r = feed(&mut sim, &mut 0, &[OFFER, b""], now);
        assert_eq!(r, [message::YACK.to_vec()], "no STRT yet");
        // Periodic re-sends before the delay carry the previous reply only.
        let mut t = now;
        let mut strt_at = None;
        while strt_at.is_none() {
            t = sim.poll_timeout().unwrap();
            sim.handle_timeout(t);
            while let Some(f) = sim.poll_transmit() {
                if f.payload.as_ref() == message::STRT {
                    assert_eq!((f.seq, f.ack), (2, 2));
                    strt_at = Some(t);
                }
            }
        }
        assert!(t >= now + Duration::from_secs(30));
        assert!(sim.violations().is_empty());
    }
}
````

- [ ] **Step 20: Regenerate `app/src/lib/codes.json`**

```bash
UPDATE_CODES=1 cargo test -p mac-netinstall --test codes
```

Expected: `test result: ok. 3 passed`; `git diff --stat app/src/lib/codes.json` shows one added line (`flash_ended_early`).

- [ ] **Step 21: Run the tests to verify they pass**

Run: `cargo test --workspace && (cd app && npx vitest run tests/i18n.test.ts)`
Expected: every suite `ok` (550 Rust tests); ni-proto's unit tests `39 passed`, ni-sim's lib `16 passed`, `tests/e2e.rs` `16 passed`, ni-engine `tests/lifecycle.rs` `17 passed`; Vitest `Tests  12 passed (12)` (`ni-assets`'s `listings_reuse_verification_results_of_unchanged_files` is flaky before Task 12 — a process-wide hash counter shared by parallel tests; if it alone fails, run again)

- [ ] **Step 22: Commit**

```bash
git add app/src-tauri/tests/bridge.rs app/src/lib/codes.json app/src/locales/en.json app/src/locales/hu.json crates/ni-assets/src/assets/packages.rs crates/ni-assets/tests/network.rs crates/ni-engine/src/codes.rs crates/ni-engine/src/engine/core.rs crates/ni-engine/src/prepare.rs crates/ni-engine/tests/common/mod.rs crates/ni-engine/tests/lifecycle.rs crates/ni-proto/src/lib.rs crates/ni-proto/src/session/mod.rs crates/ni-proto/src/session/steps.rs crates/ni-sim/src/lan.rs crates/ni-sim/src/lib.rs crates/ni-sim/tests/e2e.rs crates/ni-sim/tests/lan.rs
git commit -m "feat(proto): follow the official netinstall-cli session flow" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: ni-proto + ni-sim: golden conformance against the official captures

**Files:**
- Create: `crates/ni-proto/tests/conformance.rs`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e01-keep.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e02-r.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e03-e.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e04-b.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e05-s.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e06-r-s.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e09-r-sm.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e10-e-sm.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e11-r-s-sm.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e12-k.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e13-reboot.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e14-shutdown.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e15-r-b.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/e16-e-b.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/fake.key`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/mode.rsc`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/user.rsc`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/x09-two-packages-s.txt`
- Create: `crates/ni-proto/tests/fixtures/official-7.25.1/x18-container-first.txt`
- Create: `crates/ni-proto/tests/official/mod.rs`
- Create: `crates/ni-sim/tests/official_replay.rs`
- Modify: `crates/ni-proto/src/session/mod.rs`

Sixteen runs of the official tool (spike 5) as compact text fixtures — every server frame with its MAC fields, seq, ack and payload (data frames by size only, long runs of 1024-byte chunks as one `R` line; no package bytes), and every device reply. They were derived from the spike's decoded `transcript.txt` / `frames.txt` (the elided chunk runs are rebuilt from the file sizes, checked against the visible frames). `tests/official/mod.rs` loads them and builds the matching `InstallPlan` (synthetic packages of the captured sizes, the spike's `user.rsc` / `mode.rsc` / `fake.key`). `conformance.rs` drives our session with the captured replies and compares every frame; `ni-sim/tests/official_replay.rs` feeds the official frames to our simulator and compares every reply. `InstallPlan::system_package_first` (the official tool sends `routeros` first, x18) is added for both.

- [ ] **Step 1: Write the test `crates/ni-proto/tests/conformance.rs`**

Create `crates/ni-proto/tests/conformance.rs`:

```rust
//! Golden conformance: for every captured run of the official
//! `netinstall-cli` 7.25.1, our [`ServerSession`] with the same options,
//! answered with the captured device replies, sends the same frames: MAC
//! fields, seq/ack, every control payload byte for byte, data frames of the
//! same sizes (package contents differ: ours are synthetic).

mod official;

use ni_proto::{Event, Frame, Mac, ServerSession, SessionConfig};
use official::{Line, Payload};
use std::time::Instant;

/// The session's next frame; time only moves on while it is pacing
/// (never past a reply deadline: the capture has every reply).
fn next_frame(s: &mut ServerSession, now: &mut Instant, capture: &str) -> Frame {
    if let Some(f) = s.poll_transmit(*now) {
        return f;
    }
    let at = s.poll_timeout().expect("the session has not ended");
    s.poll_transmit(at).map_or_else(
        || panic!("{capture}: the session waits for a reply the capture does not have"),
        |f| {
            *now = at;
            f
        },
    )
}

#[test]
fn the_fixtures_cover_every_option() {
    let names: Vec<String> = official::all().into_iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        [
            "e01-keep",
            "e02-r",
            "e03-e",
            "e04-b",
            "e05-s",
            "e06-r-s",
            "e09-r-sm",
            "e10-e-sm",
            "e11-r-s-sm",
            "e12-k",
            "e13-reboot",
            "e14-shutdown",
            "e15-r-b",
            "e16-e-b",
            "x09-two-packages-s",
            "x18-container-first"
        ]
    );
}

#[test]
fn our_session_sends_what_the_official_server_sends() {
    for cap in official::all() {
        let name = cap.name.as_str();
        let plan = cap.plan();
        let mut now = Instant::now();
        let mut s = ServerSession::new(cap.info(), &plan, SessionConfig::default(), now).unwrap();
        let mut sent = 0;
        for line in &cap.lines {
            match line {
                Line::Server {
                    src,
                    dst,
                    seq,
                    ack,
                    payload,
                } => {
                    let f = next_frame(&mut s, &mut now, name);
                    let at = format!("{name}: frame {seq}");
                    assert_eq!(
                        (f.src, f.dst, f.seq, f.ack),
                        (*src, *dst, *seq, *ack),
                        "{at}"
                    );
                    match payload {
                        Payload::Text(want) => assert_eq!(
                            String::from_utf8_lossy(&f.payload),
                            String::from_utf8_lossy(want),
                            "{at}"
                        ),
                        Payload::Data(len) => assert_eq!(f.payload.len(), *len, "{at}"),
                    }
                    // The wire form: the length field counts the NUL too.
                    let wire = f.encode();
                    assert_eq!(
                        usize::from(u16::from_le_bytes([wire[14], wire[15]])),
                        f.payload.len()
                    );
                    sent += 1;
                }
                Line::Device { seq, payload } => {
                    let reply = Frame::new(cap.device, Mac::ZERO, *seq, *seq, payload.clone());
                    s.handle_frame(&reply, now);
                }
            }
        }
        assert!(s.is_finished(), "{name}: more frames than the capture");
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(events.last(), Some(&Event::Done), "{name}");
        assert!(sent > 13_000, "{name}: {sent} frames compared");
    }
}
```

- [ ] **Step 2: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e01-keep.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e01-keep.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e01_default)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args 
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\nkeep\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 ""
D 13663 13663 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 "TERM\n\0"
```

- [ ] **Step 3: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e02-r.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e02-r.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e02_r)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -r
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 ""
D 13663 13663 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 "TERM\n\0"
```

- [ ] **Step 4: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e03-e.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e03-e.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e03_e)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -e
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 "FILE\nautorun.scr\n0\n\0"
D 13663 13663 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 ""
D 13664 13664 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13665 13663 ""
D 13665 13665 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13666 13664 "TERM\n\0"
```

- [ ] **Step 5: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e04-b.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e04-b.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e04_b)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -b
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\nkeep\n\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 ""
D 13663 13663 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 "TERM\n\0"
```

- [ ] **Step 6: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e05-s.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e05-s.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e05_s)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -s user.rsc
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 "FILE\nautorun.scr\n91\n\0"
D 13663 13663 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 data 91
D 13664 13664 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13665 13663 ""
D 13665 13665 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13666 13664 ""
D 13666 13666 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13667 13665 "TERM\n\0"
```

- [ ] **Step 7: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e06-r-s.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e06-r-s.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e06_r_s)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -r -s user.rsc
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 "FILE\nautorun.scr\n91\n\0"
D 13663 13663 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 data 91
D 13664 13664 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13665 13663 ""
D 13665 13665 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13666 13664 ""
D 13666 13666 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13667 13665 "TERM\n\0"
```

- [ ] **Step 8: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e09-r-sm.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e09-r-sm.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e09_r_sm)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -r -sm mode.rsc
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 "FILE\nmode.scr\n69\n\0"
D 13663 13663 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 data 69
D 13664 13664 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13665 13663 ""
D 13665 13665 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13666 13664 ""
D 13666 13666 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13667 13665 "TERM\n\0"
```

- [ ] **Step 9: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e10-e-sm.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e10-e-sm.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e10_e_sm)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -e -sm mode.rsc
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 "FILE\nmode.scr\n69\n\0"
D 13663 13663 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 data 69
D 13664 13664 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13665 13663 ""
D 13665 13665 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13666 13664 "FILE\nautorun.scr\n0\n\0"
D 13666 13666 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13667 13665 ""
D 13667 13667 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13668 13666 ""
D 13668 13668 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13669 13667 "TERM\n\0"
```

- [ ] **Step 10: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e11-r-s-sm.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e11-r-s-sm.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e11_r_s_sm)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -r -s user.rsc -sm mode.rsc
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 "FILE\nautorun.scr\n91\n\0"
D 13663 13663 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 data 91
D 13664 13664 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13665 13663 ""
D 13665 13665 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13666 13664 "FILE\nmode.scr\n69\n\0"
D 13666 13666 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13667 13665 data 69
D 13667 13667 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13668 13666 ""
D 13668 13668 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13669 13667 ""
D 13669 13669 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13670 13668 "TERM\n\0"
```

- [ ] **Step 11: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e12-k.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e12-k.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e12_k)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -k fake.key
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n-----BEGIN MIKROTIK SOFTWARE KEY------------AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIIIJJJJKKKKLLLLMMMMNNNNOOOOPPPPQQQQRRRRSSSSTTTTUUUUVVVVWWWWXXXXYYYYZZZZ0000111122223333444455==-----END MIKROTIK SOFTWARE KEY--------------\nkeep\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 ""
D 13663 13663 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 "TERM\n\0"
```

- [ ] **Step 12: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e13-reboot.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e13-reboot.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e13_reboot)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args --reboot
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\nkeep\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 ""
D 13663 13663 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 "TERM\nInstallation successful\n\0"
```

- [ ] **Step 13: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e14-shutdown.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e14-shutdown.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e14_shutdown)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args --shutdown
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\nkeep\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 ""
D 13663 13663 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 "TERM\nshutdown\n\0"
```

- [ ] **Step 14: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e15-r-b.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e15-r-b.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e15_r_b)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -r -b
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\n\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 ""
D 13663 13663 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 "TERM\n\0"
```

- [ ] **Step 15: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/e16-e-b.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/e16-e-b.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run e16_e_b)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -e -b
packages routeros-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\n\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 "FILE\nautorun.scr\n0\n\0"
D 13663 13663 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13664 13662 ""
D 13664 13664 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13665 13663 ""
D 13665 13665 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13666 13664 "TERM\n\0"
```

- [ ] **Step 16: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/fake.key`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/fake.key`:

```
-----BEGIN MIKROTIK SOFTWARE KEY------------
AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIIIJJJJKKKKLLLLMMMMNNNNOOOOPPPP
QQQQRRRRSSSSTTTTUUUUVVVVWWWWXXXXYYYYZZZZ0000111122223333444455==
-----END MIKROTIK SOFTWARE KEY--------------
```

- [ ] **Step 17: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/mode.rsc`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/mode.rsc`:

```
:log info "spike5 mode script"
/system identity set name=spike5-mode
```

- [ ] **Step 18: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/user.rsc`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/user.rsc`:

```
/system identity set name=spike5-user
/ip address add address=10.9.9.1/24 interface=ether1
```

- [ ] **Step 19: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/x09-two-packages-s.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/x09-two-packages-s.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run x09_twopkg)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args -s user.rsc
packages routeros-7.25.1-arm64.npk container-7.25.1-arm64.npk
size routeros-7.25.1-arm64.npk 13984181
size container-7.25.1-arm64.npk 1187985
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\n\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 "FILE\ncontainer-7.25.1-arm64.npk\n1187985\n\0"
D 13663 13663 "RETR"
R 13664 1160
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14824 14822 data 145
D 14824 14824 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14825 14823 ""
D 14825 14825 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14826 14824 "FILE\nautorun.scr\n91\n\0"
D 14826 14826 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14827 14825 data 91
D 14827 14827 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14828 14826 ""
D 14828 14828 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14829 14827 ""
D 14829 14829 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14830 14828 "TERM\n\0"
```

- [ ] **Step 20: Create the test data `crates/ni-proto/tests/fixtures/official-7.25.1/x18-container-first.txt`**

Create `crates/ni-proto/tests/fixtures/official-7.25.1/x18-container-first.txt`:

```text
# Official netinstall-cli 7.25.1 (MikroTik), captured in spike 5 (run x18_pkgfirst)
# against the device simulator sim.py; see
# docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md.
# S <[0:6]> <[6:12]> <seq> <ack> <payload | data N>: a server frame;
# D <seq> <ack> <payload>: the device's reply ([0:6] = device, [6:12] = zeros);
# R <seq> <count>: <count> server data frames of 1024 bytes from <seq> on,
# [0:6] zeros, ack = seq - 2, each answered RETR. File contents are not
# stored (package sizes only; scripts come from the files next to this one).
args 
packages container-7.25.1-arm64.npk routeros-7.25.1-arm64.npk
size container-7.25.1-arm64.npk 1187985
size routeros-7.25.1-arm64.npk 13984181
device 64:d1:54:aa:bb:01
hello "DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n"
S ff:ff:ff:ff:ff:ff 64:d1:54:aa:bb:01 1 0 "OFFR\n\nkeep\nremoveBranding\n\0"
D 1 1 "YACK\n"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 2 0 ""
D 2 2 "STRT"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 3 1 ""
D 3 3 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 4 2 "FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0"
D 4 4 "RETR"
R 5 13656
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13661 13659 data 437
D 13661 13661 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13662 13660 ""
D 13662 13662 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 13663 13661 "FILE\ncontainer-7.25.1-arm64.npk\n1187985\n\0"
D 13663 13663 "RETR"
R 13664 1160
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14824 14822 data 145
D 14824 14824 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14825 14823 ""
D 14825 14825 "RETR"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14826 14824 ""
D 14826 14826 "WTRM"
S 00:00:00:00:00:00 64:d1:54:aa:bb:01 14827 14825 "TERM\n\0"
```

- [ ] **Step 21: Create the test helper `crates/ni-proto/tests/official/mod.rs`**

Create `crates/ni-proto/tests/official/mod.rs`:

```rust
//! The official `netinstall-cli` 7.25.1 captures in
//! `crates/ni-proto/tests/fixtures/official-7.25.1/` (format: see the
//! comment at the top of each file). Shared by ni-proto's conformance test
//! and ni-sim's replay test (`#[path]` include).
#![allow(dead_code)] // each test uses a different part

use bytes::Bytes;
use ni_proto::message::{self, AfterInstall};
use ni_proto::{DeviceConfig, DeviceInfo, Frame, InstallPlan, Mac, PlanFile, MAX_CHUNK};
use std::path::{Path, PathBuf};

/// A server frame's payload: exact bytes, or file data of this length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    Text(Vec<u8>),
    Data(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Server {
        src: Mac,
        dst: Mac,
        seq: u16,
        ack: u16,
        payload: Payload,
    },
    Device {
        seq: u16,
        payload: Vec<u8>,
    },
}

/// One captured run.
#[derive(Debug, Clone)]
pub struct Capture {
    /// File stem, e.g. `e10-e-sm`.
    pub name: String,
    /// The official tool's options, e.g. `["-e", "-sm", "mode.rsc"]`.
    pub args: Vec<String>,
    /// Packages as given on its command line, with their sizes.
    pub packages: Vec<(String, usize)>,
    pub device: Mac,
    pub hello: Vec<u8>,
    /// Frames after the hello, in order (`R` lines expanded).
    pub lines: Vec<Line>,
}

pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ni-proto/tests/fixtures/official-7.25.1")
}

/// Every capture, by name.
pub fn all() -> Vec<Capture> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    names.sort();
    names.iter().map(|p| load(p)).collect()
}

/// `"OFFR\n\0"`-style quoted text to bytes.
fn unquote(s: &str) -> Vec<u8> {
    let inner = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("not quoted: {s}"));
    let mut out = Vec::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(u8::try_from(c).expect("ASCII"));
            continue;
        }
        out.push(match chars.next() {
            Some('n') => b'\n',
            Some('0') => 0,
            Some('\\') => b'\\',
            Some('"') => b'"',
            other => panic!("bad escape {other:?}"),
        });
    }
    out
}

fn load(path: &Path) -> Capture {
    let text = std::fs::read_to_string(path).unwrap();
    let mut cap = Capture {
        name: path.file_stem().unwrap().to_string_lossy().into_owned(),
        args: Vec::new(),
        packages: Vec::new(),
        device: Mac::ZERO,
        hello: Vec::new(),
        lines: Vec::new(),
    };
    let mut names = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
        let words: Vec<&str> = rest.split(' ').filter(|w| !w.is_empty()).collect();
        let num = |i: usize| -> u16 { words[i].parse().unwrap() };
        match tag {
            "args" => cap.args = words.iter().map(|w| w.to_string()).collect(),
            "packages" => names = words.iter().map(|w| w.to_string()).collect(),
            "size" => cap
                .packages
                .push((words[0].to_string(), words[1].parse().unwrap())),
            "device" => cap.device = words[0].parse().unwrap(),
            "hello" => cap.hello = unquote(rest),
            "S" => {
                let payload_text = rest.splitn(5, ' ').nth(4).unwrap();
                let payload = match payload_text.strip_prefix("data ") {
                    Some(n) => Payload::Data(n.parse().unwrap()),
                    None => Payload::Text(unquote(payload_text)),
                };
                cap.lines.push(Line::Server {
                    src: words[0].parse().unwrap(),
                    dst: words[1].parse().unwrap(),
                    seq: num(2),
                    ack: num(3),
                    payload,
                });
            }
            "D" => {
                assert_eq!(words[0], words[1], "a device reply has seq = ack");
                cap.lines.push(Line::Device {
                    seq: num(0),
                    payload: unquote(rest.splitn(3, ' ').nth(2).unwrap()),
                });
            }
            "R" => {
                let (first, count) = (num(0), num(1));
                for seq in first..first + count {
                    cap.lines.push(Line::Server {
                        src: Mac::ZERO,
                        dst: cap.device,
                        seq,
                        ack: seq - 2,
                        payload: Payload::Data(MAX_CHUNK),
                    });
                    cap.lines.push(Line::Device {
                        seq,
                        payload: message::RETR.to_vec(),
                    });
                }
            }
            other => panic!("{}: unknown line {other:?}", cap.name),
        }
    }
    assert_eq!(
        names,
        cap.packages
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>()
    );
    cap
}

/// NPK-looking bytes of `len` (deterministic, seeded by the name).
pub fn package(name: &str, len: usize) -> Bytes {
    let seed = name.bytes().fold(0u8, u8::wrapping_add);
    let mut v = message::NPK_MAGIC.to_vec();
    v.extend((v.len()..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    Bytes::from(v)
}

fn fixture_file(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap()
}

impl Capture {
    /// The device's hello, parsed.
    pub fn info(&self) -> DeviceInfo {
        let hello = Frame::new(self.device, Mac::BROADCAST, 1, 0, self.hello.clone());
        DeviceInfo::parse(&hello).unwrap()
    }

    /// What we install for the same options: synthetic packages of the
    /// captured sizes (system package first), the scripts and key file
    /// from the fixture directory.
    pub fn plan(&self) -> InstallPlan {
        let mut plan = InstallPlan {
            packages: self
                .packages
                .iter()
                .map(|(name, len)| PlanFile {
                    name: name.clone(),
                    data: package(name, *len),
                })
                .collect(),
            config: DeviceConfig::Keep,
            ..InstallPlan::default()
        };
        plan.system_package_first();
        let mut args = self.args.iter();
        let (mut default, mut empty, mut script) = (false, false, None);
        while let Some(a) = args.next() {
            let mut value = || fixture_file(args.next().expect("a file name"));
            match a.as_str() {
                "-r" => default = true,
                "-e" => empty = true,
                "-b" => plan.remove_branding = true,
                "-s" => script = Some(Bytes::from(value())),
                "-sm" => plan.mode_script = Some(Bytes::from(value())),
                "-k" => {
                    let key = String::from_utf8(value()).unwrap();
                    plan.license_key = key.replace(['\n', '\r'], "");
                }
                "--reboot" => plan.after_install = AfterInstall::RebootWithMessage,
                "--shutdown" => plan.after_install = AfterInstall::Shutdown,
                other => panic!("{}: unknown option {other}", self.name),
            }
        }
        plan.config = match (script, empty, default) {
            (Some(s), _, _) => DeviceConfig::Script(s),
            (None, true, _) => DeviceConfig::Empty,
            (None, false, true) => DeviceConfig::Default,
            (None, false, false) => DeviceConfig::Keep,
        };
        plan
    }
}
```

- [ ] **Step 22: Write the test `crates/ni-sim/tests/official_replay.rs`**

Create `crates/ni-sim/tests/official_replay.rs`:

```rust
//! Replay: the official `netinstall-cli` 7.25.1's frames from every
//! captured run, fed to our simulated device, draw exactly the replies the
//! device gave in the capture (the official server accepted them), and the
//! device ends up with the files, offer and `TERM` of that run.

#[path = "../../ni-proto/tests/official/mod.rs"]
mod official;

use bytes::Bytes;
use ni_proto::message::{self, AfterInstall};
use ni_proto::{Frame, Mac};
use ni_sim::{DeviceSim, SimConfig};
use official::{Line, Payload};
use std::time::Instant;

#[test]
fn our_device_answers_the_official_server_like_the_capture() {
    for cap in official::all() {
        let name = cap.name.as_str();
        let plan = cap.plan();
        let files = plan.files();
        let now = Instant::now();
        let mut cfg = SimConfig::new(cap.device, "", "");
        cfg.info = cap.info();
        let mut sim = DeviceSim::new(cfg, now);
        sim.handle_timeout(now);
        let hello = sim.poll_transmit().expect("hello");
        assert_eq!(hello.payload.as_ref(), cap.hello.as_slice(), "{name}");
        // The file being sent and the bytes of it sent so far.
        let mut current: Option<(Bytes, usize)> = None;
        for line in &cap.lines {
            match line {
                Line::Server {
                    src,
                    dst,
                    seq,
                    ack,
                    payload,
                } => {
                    let bytes = match payload {
                        Payload::Text(p) => {
                            if let Some((file, _)) = message::parse_file_header(p) {
                                let data = files.iter().find(|(n, _)| *n == file).unwrap();
                                current = Some((data.1.clone(), 0));
                            }
                            Bytes::from(p.clone())
                        }
                        Payload::Data(len) => {
                            let (data, at) = current.as_mut().expect("data after a header");
                            let chunk = data.slice(*at..*at + len);
                            *at += len;
                            chunk
                        }
                    };
                    sim.handle_frame(&Frame::new(*src, *dst, *seq, *ack, bytes), now);
                }
                Line::Device { seq, payload } => {
                    let reply = sim
                        .poll_transmit()
                        .unwrap_or_else(|| panic!("{name}: no reply to {seq}"));
                    assert_eq!(
                        (reply.src, reply.dst, reply.seq, reply.ack),
                        (cap.device, Mac::ZERO, *seq, *seq),
                        "{name}"
                    );
                    assert_eq!(
                        String::from_utf8_lossy(&reply.payload),
                        String::from_utf8_lossy(payload),
                        "{name}: reply to {seq}"
                    );
                }
            }
            assert!(
                sim.violations().is_empty(),
                "{name}: {:?}",
                sim.violations()
            );
        }
        assert_eq!(sim.poll_transmit(), None, "{name}: TERM is not answered");
        assert!(sim.is_terminated(), "{name}");
        let got: Vec<(&str, &[u8])> = sim
            .received_files()
            .iter()
            .map(|f| (f.name.as_str(), f.data.as_slice()))
            .collect();
        let want: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), &d[..])).collect();
        assert!(got == want, "{name}: received files differ");
        assert_eq!(sim.offer(), Some(&plan.offer()), "{name}");
        let after = match cap.args.last().map(String::as_str) {
            Some("--reboot") => AfterInstall::RebootWithMessage,
            Some("--shutdown") => AfterInstall::Shutdown,
            _ => AfterInstall::Reboot,
        };
        assert_eq!(sim.after_install(), Some(after), "{name}");
    }
}
```

- [ ] **Step 23: Run it to verify it fails**

Run: `cargo test -p ni-proto --test conformance`
Expected: `cargo test -p ni-proto --test conformance` does not compile: `no method named `system_package_first` found for struct `InstallPlan``

- [ ] **Step 24: Edit `crates/ni-proto/src/session/mod.rs` (with its unit tests)**

1. In `crates/ni-proto/src/session/mod.rs`, replace:

```rust
    pub fn total_bytes(&self) -> u64 {
        self.files().iter().map(|(_, d)| d.len() as u64).sum()
    }
}
```

with:

```rust
    pub fn total_bytes(&self) -> u64 {
        self.files().iter().map(|(_, d)| d.len() as u64).sum()
    }

    /// Move the system package (`routeros.npk` or
    /// `routeros-<version>-<arch>.npk`) to the front, keeping the order of
    /// the others: the official tool sends it first whatever the order on
    /// its command line.
    pub fn system_package_first(&mut self) {
        let system = |p: &PlanFile| p.name == "routeros.npk" || p.name.starts_with("routeros-");
        if let Some(i) = self.packages.iter().position(system) {
            let p = self.packages.remove(i);
            self.packages.insert(0, p);
        }
    }
}
```

2. In `crates/ni-proto/src/session/mod.rs`, replace:

```rust

    #[test]
    fn the_offer_carries_keep_key_and_branding() {
        let mut p = plan();
```

with:

```rust

    #[test]
    fn the_system_package_goes_first() {
        let mut p = InstallPlan {
            packages: vec![
                file("container-7.25.1-arm64.npk", npk(4)),
                file("wifi-qcom-7.25.1-arm64.npk", npk(4)),
                file("routeros-7.25.1-arm64.npk", npk(4)),
            ],
            ..InstallPlan::default()
        };
        p.system_package_first();
        let order: Vec<&str> = p.packages.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            order,
            [
                "routeros-7.25.1-arm64.npk",
                "container-7.25.1-arm64.npk",
                "wifi-qcom-7.25.1-arm64.npk"
            ]
        );
        let mut q = plan();
        q.packages.insert(0, file("a.npk", npk(4)));
        q.system_package_first();
        assert_eq!(q.packages[0].name, "routeros.npk");
    }

    #[test]
    fn the_offer_carries_keep_key_and_branding() {
        let mut p = plan();
```

- [ ] **Step 25: Run the tests to verify they pass**

Run: `cargo test -p ni-proto --test conformance && cargo test -p ni-sim --test official_replay`
Expected: `test result: ok. 2 passed` (conformance, ~2 s) and `test result: ok. 1 passed` (replay). To see the test bite, change `self.seq.saturating_sub(2)` to `(1)` in `ServerSession::poll_transmit`: it fails with `e01-keep: frame 2` (`left: (…, 2, 1)`, `right: (…, 2, 0)`); change it back

- [ ] **Step 26: Commit**

```bash
git add crates/ni-proto/src/session/mod.rs crates/ni-proto/tests/conformance.rs crates/ni-proto/tests/fixtures/official-7.25.1/e01-keep.txt crates/ni-proto/tests/fixtures/official-7.25.1/e02-r.txt crates/ni-proto/tests/fixtures/official-7.25.1/e03-e.txt crates/ni-proto/tests/fixtures/official-7.25.1/e04-b.txt crates/ni-proto/tests/fixtures/official-7.25.1/e05-s.txt crates/ni-proto/tests/fixtures/official-7.25.1/e06-r-s.txt crates/ni-proto/tests/fixtures/official-7.25.1/e09-r-sm.txt crates/ni-proto/tests/fixtures/official-7.25.1/e10-e-sm.txt crates/ni-proto/tests/fixtures/official-7.25.1/e11-r-s-sm.txt crates/ni-proto/tests/fixtures/official-7.25.1/e12-k.txt crates/ni-proto/tests/fixtures/official-7.25.1/e13-reboot.txt crates/ni-proto/tests/fixtures/official-7.25.1/e14-shutdown.txt crates/ni-proto/tests/fixtures/official-7.25.1/e15-r-b.txt crates/ni-proto/tests/fixtures/official-7.25.1/e16-e-b.txt crates/ni-proto/tests/fixtures/official-7.25.1/fake.key crates/ni-proto/tests/fixtures/official-7.25.1/mode.rsc crates/ni-proto/tests/fixtures/official-7.25.1/user.rsc crates/ni-proto/tests/fixtures/official-7.25.1/x09-two-packages-s.txt crates/ni-proto/tests/fixtures/official-7.25.1/x18-container-first.txt crates/ni-proto/tests/official/mod.rs crates/ni-sim/tests/official_replay.rs
git commit -m "test(proto): golden conformance against the official netinstall-cli captures" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: ni-sim: the simulator on a real UDP socket (VM check against the official tool)

**Files:**
- Create: `crates/ni-sim/examples/udp_device.rs`

A small std-only example that runs `DeviceSim` on `0.0.0.0:5000`, says hello to `255.255.255.255:5000` and writes the received files out, so the simulator can be run against the official `netinstall-cli` in a Linux network namespace (the roadmap entry of Task 13 records that run). It is cross-compiled from the Mac with rustup's musl target and `rust-lld`; no linker needs installing.

- [ ] **Step 1: Create `crates/ni-sim/examples/udp_device.rs`**

Create `crates/ni-sim/examples/udp_device.rs`:

````rust
//! A simulated RouterBOARD in Netinstall mode on a real network: the
//! [`DeviceSim`] state machine on a UDP socket bound to `0.0.0.0:5000`,
//! saying hello to `255.255.255.255:5000` and answering whatever server
//! offers to install it. Used to check the simulator against the official
//! `netinstall-cli` (Linux) in a network namespace; see the Plan 5
//! roadmap entry. Standard library only.
//!
//! ```text
//! udp_device --mac 64:D1:54:AA:BB:02 --arch arm64 --model RB5009UG+S+ \
//!            --out DIR [--after-files official|pynetinstall|never|early] [--timeout 60]
//! ```
//!
//! Received files are written to `DIR`; the last line printed is
//! `done <offer> <TERM>` (exit 0), or `timeout` / `violation: ...` (exit 1).

use ni_proto::{Frame, Mac, PORT};
use ni_sim::{AfterFiles, DeviceSim, SimConfig};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn arg(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mac: Mac = arg(&args, "--mac")
        .unwrap_or_else(|| "64:D1:54:AA:BB:02".into())
        .parse()
        .expect("--mac");
    let arch = arg(&args, "--arch").unwrap_or_else(|| "arm64".into());
    let model = arg(&args, "--model").unwrap_or_else(|| "RB5009UG+S+".into());
    let out = PathBuf::from(arg(&args, "--out").expect("--out DIR"));
    let timeout =
        Duration::from_secs(arg(&args, "--timeout").map_or(60, |s| s.parse().expect("--timeout")));
    let mut cfg = SimConfig::new(mac, &model, &arch);
    cfg.info.min_os = "7.1".into();
    cfg.after_files = match arg(&args, "--after-files").as_deref() {
        None | Some("official") => AfterFiles::Official,
        Some("pynetinstall") => AfterFiles::PyNetinstall,
        Some("never") => AfterFiles::NeverWtrm,
        Some("early") => AfterFiles::EarlyWtrm,
        Some(other) => panic!("--after-files {other}"),
    };
    std::fs::create_dir_all(&out).expect("output directory");

    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT)).expect("bind 0.0.0.0:5000");
    sock.set_broadcast(true).expect("SO_BROADCAST");
    sock.set_read_timeout(Some(Duration::from_millis(10)))
        .expect("read timeout");
    let to = SocketAddr::from((Ipv4Addr::BROADCAST, PORT));
    let start = Instant::now();
    let mut sim = DeviceSim::new(cfg, start);
    let mut buf = vec![0u8; 2048];
    let mut frames_in = 0u64;
    loop {
        let now = Instant::now();
        sim.handle_timeout(now);
        while let Some(f) = sim.poll_transmit() {
            sock.send_to(&f.encode(), to).expect("send");
        }
        if let Some(v) = sim.violations().first() {
            println!("violation: {v}");
            return ExitCode::FAILURE;
        }
        if sim.is_terminated() {
            break;
        }
        if now - start > timeout {
            println!("timeout after {frames_in} frames");
            return ExitCode::FAILURE;
        }
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            if let Ok(frame) = Frame::decode(&buf[..n]) {
                if frame.dst == mac {
                    frames_in += 1;
                }
                sim.handle_frame(&frame, Instant::now());
            }
        }
    }
    for f in sim.received_files() {
        std::fs::write(out.join(&f.name), &f.data).expect("write a received file");
        println!("file {} {}", f.name, f.data.len());
    }
    println!(
        "done {:?} {:?} after {frames_in} frames",
        sim.offer().expect("offered"),
        sim.after_install().expect("terminated")
    );
    ExitCode::SUCCESS
}
````

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo build -p ni-sim --example udp_device && cargo clippy -p ni-sim --examples -- -D warnings`
Expected: both `Finished` without warnings. Optional, for the VM: `rustup target add x86_64-unknown-linux-musl` and `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld cargo build -p ni-sim --example udp_device --release --target x86_64-unknown-linux-musl` → `file target/x86_64-unknown-linux-musl/release/examples/udp_device` says `ELF 64-bit LSB pie executable, x86-64 … static-pie linked`

- [ ] **Step 3: Commit**

```bash
git add crates/ni-sim/examples/udp_device.rs
git commit -m "feat(sim): UDP device example for checks against the official netinstall-cli" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: ni-engine: configuration modes in profiles

**Files:**
- Modify: `app/src/lib/codes.json`
- Modify: `app/src/locales/en.json`
- Modify: `app/src/locales/hu.json`
- Modify: `crates/ni-cli/src/commands.rs`
- Modify: `crates/ni-cli/tests/cli.rs`
- Modify: `crates/ni-engine/src/codes.rs`
- Modify: `crates/ni-engine/src/lib.rs`
- Modify: `crates/ni-engine/src/preflight.rs`
- Modify: `crates/ni-engine/src/prepare.rs`
- Modify: `crates/ni-engine/src/profile.rs`
- Modify: `crates/ni-engine/tests/flash.rs`

`ConfigMode` becomes `Keep` / `Default` / `Empty` / `Script { local_name }` (`custom_script` still reads, as an alias), and `Profile` gets `mode_script`, `remove_branding`, `license_key` and `after_install` (`reboot`, `reboot_message`, `shutdown`), all with defaults so Plan 4–6 profile files load unchanged. Validation adds `mode_script_with_keep` and `invalid_license_key` (`max`); `license_key_from_file` strips line breaks like the official `-k`. `prepare` builds the plan from every option (system package first), preflight checks both scripts (`script_missing`). `ni-cli` follows (`config_text` describes all options) and its profile JSON test sees the new fields. The unit tests live in the module itself (`#[cfg(test)] mod tests`), so they are written together with the code; run them right after.

- [ ] **Step 1: Update the tests in `crates/ni-cli/tests/cli.rs`**

1. In `crates/ni-cli/tests/cli.rs`, replace:

```rust
        serde_json::json!([{"name": "Lab", "version": "long-term",
            "extra_packages": ["wifi-qcom"],
            "config": {"mode": "custom_script", "local_name": "lab.rsc"}}])
    );
    let (_, out, _) = env.run(&["profiles", "list"]).await;
```

with:

```rust
        serde_json::json!([{"name": "Lab", "version": "long-term",
            "extra_packages": ["wifi-qcom"],
            "config": {"mode": "script", "local_name": "lab.rsc"},
            "mode_script": null, "remove_branding": false, "license_key": null,
            "after_install": "reboot"}])
    );
    let (_, out, _) = env.run(&["profiles", "list"]).await;
```

- [ ] **Step 2: Update the tests in `crates/ni-engine/tests/flash.rs`**

1. In `crates/ni-engine/tests/flash.rs`, replace:

```rust
    rig.engine.stop().await.unwrap();
    let mut p = profile();
    p.config = ConfigMode::CustomScript {
        local_name: local.name,
    };
```

with:

```rust
    rig.engine.stop().await.unwrap();
    let mut p = profile();
    p.config = ConfigMode::Script {
        local_name: local.name,
    };
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p ni-engine`
Expected: `tests/flash.rs` does not compile: `no variant named `Script` found for enum `ConfigMode``

- [ ] **Step 4: Edit `app/src/locales/en.json`**

1. In `app/src/locales/en.json`, replace:

```json
  "code.interface_not_ethernet": "{interface} is a {kind} interface, not a wired Ethernet adapter; RouterBOARDs can only be netinstalled over wired Ethernet.",
  "code.invalid_cooldown": "The cooldown must be between {min_secs} and {max_secs} seconds.",
  "code.ipv4_dhcp": "{service} ({interface}) gets its IPv4 address over DHCP.",
  "code.invalid_max_parallel": "Devices at once must be between {min} and {max}.",
```

with:

```json
  "code.interface_not_ethernet": "{interface} is a {kind} interface, not a wired Ethernet adapter; RouterBOARDs can only be netinstalled over wired Ethernet.",
  "code.invalid_cooldown": "The cooldown must be between {min_secs} and {max_secs} seconds.",
  "code.invalid_license_key": "The license key must be a MikroTik key file of 1 to {max} printable characters.",
  "code.ipv4_dhcp": "{service} ({interface}) gets its IPv4 address over DHCP.",
  "code.invalid_max_parallel": "Devices at once must be between {min} and {max}.",
```

2. In `app/src/locales/en.json`, replace:

```json
  "code.link_down": "{interface}: no link ({reason}).",
  "code.link_up": "{interface}: link is back.",
  "code.no_ipv4": "{interface} has no IPv4 address.",
  "code.no_mac": "The MAC address of {interface} is unknown.",
```

with:

```json
  "code.link_down": "{interface}: no link ({reason}).",
  "code.link_up": "{interface}: link is back.",
  "code.mode_script_with_keep": "A mode script cannot be combined with keeping the existing configuration: choose the default, empty or own script configuration.",
  "code.no_ipv4": "{interface} has no IPv4 address.",
  "code.no_mac": "The MAC address of {interface} is unknown.",
```

- [ ] **Step 5: Edit `app/src/locales/hu.json`**

1. In `app/src/locales/hu.json`, replace:

```json
  "code.interface_not_ethernet": "{interface}: {kind} típusú csatoló, nem vezetékes Ethernet-adapter; RouterBOARD-ra csak vezetékes Etherneten keresztül lehet telepíteni.",
  "code.invalid_cooldown": "A várakozási idő {min_secs} és {max_secs} másodperc között legyen.",
  "code.ipv4_dhcp": "{service} ({interface}) DHCP-n keresztül kap IPv4-címet.",
  "code.invalid_max_parallel": "Az egyszerre telepített eszközök száma {min} és {max} között lehet.",
```

with:

```json
  "code.interface_not_ethernet": "{interface}: {kind} típusú csatoló, nem vezetékes Ethernet-adapter; RouterBOARD-ra csak vezetékes Etherneten keresztül lehet telepíteni.",
  "code.invalid_cooldown": "A várakozási idő {min_secs} és {max_secs} másodperc között legyen.",
  "code.invalid_license_key": "A licenckulcs egy MikroTik kulcsfájl legyen, legfeljebb {max} nyomtatható karakterrel.",
  "code.ipv4_dhcp": "{service} ({interface}) DHCP-n keresztül kap IPv4-címet.",
  "code.invalid_max_parallel": "Az egyszerre telepített eszközök száma {min} és {max} között lehet.",
```

2. In `app/src/locales/hu.json`, replace:

```json
  "code.link_down": "{interface}: nincs kapcsolat ({reason}).",
  "code.link_up": "{interface}: a kapcsolat helyreállt.",
  "code.no_ipv4": "{interface}: nincs IPv4-címe.",
  "code.no_mac": "{interface}: a MAC-cím ismeretlen.",
```

with:

```json
  "code.link_down": "{interface}: nincs kapcsolat ({reason}).",
  "code.link_up": "{interface}: a kapcsolat helyreállt.",
  "code.mode_script_with_keep": "Mode script nem használható a meglévő konfiguráció megtartásával: válaszd a gyári, az üres vagy a saját szkriptes konfigurációt.",
  "code.no_ipv4": "{interface}: nincs IPv4-címe.",
  "code.no_mac": "{interface}: a MAC-cím ismeretlen.",
```

- [ ] **Step 6: Edit `crates/ni-cli/src/commands.rs`**

1. In `crates/ni-cli/src/commands.rs`, replace:

```rust
use ni_engine::preflight::{self, PreflightReport};
use ni_engine::profile::spec_to_string;
use ni_engine::{default_profiles_root, ConfigMode, EngineConfig, Failure, Profile, ProfileStore};
use ni_net::{list_interfaces, Interface};
use serde_json::json;
```

with:

```rust
use ni_engine::preflight::{self, PreflightReport};
use ni_engine::profile::spec_to_string;
use ni_engine::{
    default_profiles_root, AfterInstall, ConfigMode, EngineConfig, Failure, Profile, ProfileStore,
};
use ni_net::{list_interfaces, Interface};
use serde_json::json;
```

2. In `crates/ni-cli/src/commands.rs`, replace:

```rust
    if let Some(script) = &recipe.script {
        let local = register_script(assets, script).await?;
        p.config = ConfigMode::CustomScript { local_name: local };
    }
    p.validate()?;
```

with:

```rust
    if let Some(script) = &recipe.script {
        let local = register_script(assets, script).await?;
        p.config = ConfigMode::Script { local_name: local };
    }
    p.validate()?;
```

3. In `crates/ni-cli/src/commands.rs`, replace:

```rust
}

fn config_text(c: &ConfigMode) -> String {
    match c {
        ConfigMode::Default => "default config".into(),
        ConfigMode::CustomScript { local_name } => format!("script {local_name}"),
    }
}

```

with:

```rust
}

/// `default config`, `script lab.rsc, mode script m.rsc, shutdown`, ...
fn config_text(p: &Profile) -> String {
    let mut out = match &p.config {
        ConfigMode::Keep => "keep config".to_string(),
        ConfigMode::Default => "default config".to_string(),
        ConfigMode::Empty => "empty config".to_string(),
        ConfigMode::Script { local_name } => format!("script {local_name}"),
    };
    if let Some(m) = &p.mode_script {
        out.push_str(&format!(", mode script {m}"));
    }
    if p.remove_branding {
        out.push_str(", remove branding");
    }
    if p.license_key.is_some() {
        out.push_str(", license key");
    }
    match p.after_install {
        AfterInstall::Reboot => {}
        AfterInstall::RebootMessage => out.push_str(", reboot with message"),
        AfterInstall::Shutdown => out.push_str(", shutdown"),
    }
    out
}

```

4. In `crates/ni-cli/src/commands.rs`, replace:

```rust
                        spec_to_string(&p.version),
                        packages.join(","),
                        config_text(&p.config)
                    );
                }
```

with:

```rust
                        spec_to_string(&p.version),
                        packages.join(","),
                        config_text(p)
                    );
                }
```

- [ ] **Step 7: Edit `crates/ni-engine/src/codes.rs`**

1. In `crates/ni-engine/src/codes.rs`, replace:

```rust
    "interface_not_ethernet",
    "invalid_cooldown",
    "ipv4_dhcp",
    "invalid_max_parallel",
```

with:

```rust
    "interface_not_ethernet",
    "invalid_cooldown",
    "invalid_license_key",
    "ipv4_dhcp",
    "invalid_max_parallel",
```

2. In `crates/ni-engine/src/codes.rs`, replace:

```rust
    "link_down",
    "link_up",
    "no_ipv4",
    "no_mac",
```

with:

```rust
    "link_down",
    "link_up",
    "mode_script_with_keep",
    "no_ipv4",
    "no_mac",
```

- [ ] **Step 8: Edit `crates/ni-engine/src/lib.rs`**

1. In `crates/ni-engine/src/lib.rs`, replace:

```rust
};
pub use preflight::{Finding, PreflightReport, Severity};
pub use profile::{default_profiles_root, ConfigMode, Listing, Profile, ProfileStore};
```

with:

```rust
};
pub use preflight::{Finding, PreflightReport, Severity};
pub use profile::{
    default_profiles_root, license_key_from_file, AfterInstall, ConfigMode, Listing, Profile,
    ProfileStore, MAX_LICENSE_KEY,
};
```

- [ ] **Step 9: Edit `crates/ni-engine/src/preflight.rs` (with its unit tests)**

1. In `crates/ni-engine/src/preflight.rs`, replace:

```rust
};
use crate::event::Failure;
use crate::profile::{ConfigMode, Profile};
use ni_assets::Assets;
use ni_bootp::{BootpConfig, BootpResponder};
```

with:

```rust
};
use crate::event::Failure;
use crate::profile::Profile;
use ni_assets::Assets;
use ni_bootp::{BootpConfig, BootpResponder};
```

2. In `crates/ni-engine/src/preflight.rs`, replace:

```rust
        }
    }
    if let ConfigMode::CustomScript { local_name } = &profile.config {
        if let Err(e) = assets.local_file(local_name).await {
            out.push(Finding::error(
```

with:

```rust
        }
    }
    for local_name in profile.scripts() {
        if let Err(e) = assets.local_file(local_name).await {
            out.push(Finding::error(
```

3. In `crates/ni-engine/src/preflight.rs`, replace:

```rust
    use super::*;
    use crate::config::test_iface as iface;
    use ni_assets::testutil::{assets, mirror, tempdir, TestServer};
    use ni_assets::{Channel, VersionSpec};
```

with:

```rust
    use super::*;
    use crate::config::test_iface as iface;
    use crate::profile::ConfigMode;
    use ni_assets::testutil::{assets, mirror, tempdir, TestServer};
    use ni_assets::{Channel, VersionSpec};
```

4. In `crates/ni-engine/src/preflight.rs`, replace:

```rust
            .unwrap();
        assert_eq!(check_assets(&a, &profile).await, []);
        profile.config = ConfigMode::CustomScript {
            local_name: "setup.rsc".into(),
        };
        let findings = check_assets(&a, &profile).await;
        assert_eq!(codes(&findings), [("script_missing", Severity::Error)]);
        profile.version = VersionSpec::Channel(Channel::Testing);
        profile.config = ConfigMode::Default;
```

with:

```rust
            .unwrap();
        assert_eq!(check_assets(&a, &profile).await, []);
        profile.config = ConfigMode::Script {
            local_name: "setup.rsc".into(),
        };
        profile.mode_script = Some("mode.rsc".into());
        let findings = check_assets(&a, &profile).await;
        assert_eq!(
            codes(&findings),
            [
                ("script_missing", Severity::Error),
                ("script_missing", Severity::Error)
            ]
        );
        assert_eq!(findings[1].failure.param("name"), Some("mode.rsc"));
        profile.mode_script = None;
        profile.version = VersionSpec::Channel(Channel::Testing);
        profile.config = ConfigMode::Default;
```

- [ ] **Step 10: Edit `crates/ni-engine/src/prepare.rs` (with its unit tests)**

1. In `crates/ni-engine/src/prepare.rs`, replace:

```rust
/// it already; see [`resolve_or_remembered`] for offline), check it against
/// `kernel_version` (the boot kernels the device runs), then fetch packages
/// and the script. Returns the version installed and the plan. Errors:
/// `assets_failed` (`cause`), `profile_version_mismatch` (`version`,
/// `kernel_version`), `invalid_plan` (`detail`).
```

with:

```rust
/// it already; see [`resolve_or_remembered`] for offline), check it against
/// `kernel_version` (the boot kernels the device runs), then fetch packages
/// and the scripts. The plan carries the profile's configuration options
/// and sends the system package first. Returns the version installed and
/// the plan. Errors:
/// `assets_failed` (`cause`), `profile_version_mismatch` (`version`,
/// `kernel_version`), `invalid_plan` (`detail`).
```

2. In `crates/ni-engine/src/prepare.rs`, replace:

```rust
        .map_err(fail)?;
    let config = match &profile.config {
        ConfigMode::Default => DeviceConfig::Default,
        ConfigMode::CustomScript { local_name } => {
            DeviceConfig::Script(assets.local_file(local_name).await.map_err(fail)?)
        }
    };
    let plan = InstallPlan {
        packages,
        config,
        ..InstallPlan::default()
    };
    plan.validate()
        .map_err(|e| Failure::new("invalid_plan").with("detail", e))?;
```

with:

```rust
        .map_err(fail)?;
    let config = match &profile.config {
        ConfigMode::Keep => DeviceConfig::Keep,
        ConfigMode::Default => DeviceConfig::Default,
        ConfigMode::Empty => DeviceConfig::Empty,
        ConfigMode::Script { local_name } => {
            DeviceConfig::Script(assets.local_file(local_name).await.map_err(fail)?)
        }
    };
    let mode_script = match &profile.mode_script {
        Some(name) => Some(assets.local_file(name).await.map_err(fail)?),
        None => None,
    };
    let mut plan = InstallPlan {
        packages,
        config,
        mode_script,
        remove_branding: profile.remove_branding,
        license_key: profile.license_key.clone().unwrap_or_default(),
        after_install: profile.after_install.into(),
    };
    plan.system_package_first();
    plan.validate()
        .map_err(|e| Failure::new("invalid_plan").with("detail", e))?;
```

3. In `crates/ni-engine/src/prepare.rs`, replace:

```rust
        let local = a.register_local_file(&script).await.unwrap();
        let mut p = profile();
        p.config = ConfigMode::CustomScript {
            local_name: local.name,
        };
```

with:

```rust
        let local = a.register_local_file(&script).await.unwrap();
        let mut p = profile();
        p.config = ConfigMode::Script {
            local_name: local.name,
        };
```

4. In `crates/ni-engine/src/prepare.rs`, replace:

```rust
            DeviceConfig::Script(Bytes::from_static(b"/system identity set name=x\n"))
        );
    }

```

with:

```rust
            DeviceConfig::Script(Bytes::from_static(b"/system identity set name=x\n"))
        );
    }

    #[tokio::test]
    async fn plan_carries_the_configuration_options() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("prepare-options");
        let a = assets(&srv, &dir);
        let mode = dir.join("mode.rsc");
        std::fs::write(&mode, b":log info mode\n").unwrap();
        let mut p = profile();
        p.config = ConfigMode::Empty;
        p.mode_script = Some(a.register_local_file(&mode).await.unwrap().name);
        p.remove_branding = true;
        p.license_key = Some("KEY-1".into());
        p.after_install = crate::profile::AfterInstall::Shutdown;
        let (_, plan) = prepare(&a, &p, None, None, "arm64").await.unwrap();
        assert_eq!(plan.config, DeviceConfig::Empty);
        assert_eq!(plan.mode_script.as_deref(), Some(&b":log info mode\n"[..]));
        assert!(plan.remove_branding);
        assert_eq!(plan.license_key, "KEY-1");
        assert_eq!(plan.after_install, ni_proto::AfterInstall::Shutdown);
        let files: Vec<String> = plan.files().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            files,
            [
                "routeros-7.25.1-arm64.npk",
                "wireless-7.25.1-arm64.npk",
                "mode.scr",
                "autorun.scr"
            ]
        );
        p.config = ConfigMode::Keep;
        p.mode_script = None;
        let (_, plan) = prepare(&a, &p, None, None, "arm64").await.unwrap();
        assert!(plan.offer().keep_config);
        p.mode_script = Some("missing.rsc".into());
        p.config = ConfigMode::Default;
        let err = prepare(&a, &p, None, None, "arm64").await.unwrap_err();
        assert_eq!(err.code, "assets_failed");
    }

```

- [ ] **Step 11: Edit `crates/ni-engine/src/profile.rs` (with its unit tests)**

1. In `crates/ni-engine/src/profile.rs`, replace:

````rust
//!   "version": "stable",
//!   "extra_packages": ["wifi-qcom"],
//!   "config": { "mode": "custom_script", "local_name": "setup.rsc" }
//! }
//! ```
//!
//! `version` is a channel (`stable`, `long-term`, `testing`,
//! `development`) or an exact version (`7.25.1`). A custom script names a
//! file registered with `ni_assets::Assets::register_local_file`.

use crate::event::Failure;
````

with:

````rust
//!   "version": "stable",
//!   "extra_packages": ["wifi-qcom"],
//!   "config": { "mode": "script", "local_name": "setup.rsc" },
//!   "mode_script": "mode.rsc",
//!   "remove_branding": false,
//!   "license_key": null,
//!   "after_install": "reboot"
//! }
//! ```
//!
//! `version` is a channel (`stable`, `long-term`, `testing`,
//! `development`) or an exact version (`7.25.1`). Scripts name files
//! registered with `ni_assets::Assets::register_local_file`. The
//! configuration options are the official `netinstall-cli`'s (`-r`, `-e`,
//! `-s`, `-sm`, `-b`, `-k`, `--reboot`, `--shutdown`); see
//! `docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md`.
//!
//! Older profiles (Plans 4–6) load unchanged: `{"mode": "default"}` is
//! still the default configuration, `{"mode": "custom_script", ...}` reads
//! as `script`, and the fields added later take their defaults. They are
//! written in the new form the next time they are saved.

use crate::event::Failure;
````

2. In `crates/ni-engine/src/profile.rs`, replace:

```rust
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ConfigMode {
    /// RouterOS applies its default configuration.
    #[default]
    Default,
    /// Upload this registered local `.rsc` file as `autorun.scr`.
    CustomScript { local_name: String },
    // Plan 5 adds `Keep`, `NoDefault` and `Empty` once the official tool's
    // encoding (OFFR fields / `mode.scr`) is captured. Not implemented.
}

```

with:

```rust
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ConfigMode {
    /// Keep the existing configuration (the official tool's default).
    Keep,
    /// RouterOS applies its default configuration (`-r`).
    #[default]
    Default,
    /// Start without any configuration (`-e`).
    Empty,
    /// Run this registered local `.rsc` file on first boot (`-s`),
    /// uploaded as `autorun.scr`. Plans 4–6 called it `custom_script`.
    #[serde(alias = "custom_script")]
    Script { local_name: String },
}

impl ConfigMode {
    /// `keep`, `default`, `empty` or `script` (as in the JSON).
    pub fn name(&self) -> &'static str {
        match self {
            ConfigMode::Keep => "keep",
            ConfigMode::Default => "default",
            ConfigMode::Empty => "empty",
            ConfigMode::Script { .. } => "script",
        }
    }
}

/// What the device does when the install is done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterInstall {
    /// Reboot (the official tool's default).
    #[default]
    Reboot,
    /// Reboot with the message "Installation successful" (`--reboot`).
    RebootMessage,
    /// Shut down (`--shutdown`).
    Shutdown,
}

impl From<AfterInstall> for ni_proto::AfterInstall {
    fn from(a: AfterInstall) -> Self {
        match a {
            AfterInstall::Reboot => ni_proto::AfterInstall::Reboot,
            AfterInstall::RebootMessage => ni_proto::AfterInstall::RebootWithMessage,
            AfterInstall::Shutdown => ni_proto::AfterInstall::Shutdown,
        }
    }
}

/// Longest license key, in bytes (after line breaks are removed).
pub const MAX_LICENSE_KEY: usize = ni_proto::message::MAX_LICENSE_KEY;

/// A license key from a key file's contents (the official tool's `-k`):
/// line breaks removed. Error: `invalid_license_key` (`max`) unless the
/// rest is 1..=[`MAX_LICENSE_KEY`] printable ASCII characters.
pub fn license_key_from_file(data: &[u8]) -> Result<String, Failure> {
    let key: Vec<u8> = data
        .iter()
        .copied()
        .filter(|b| !matches!(b, b'\n' | b'\r'))
        .collect();
    let key = String::from_utf8(key).unwrap_or_default();
    check_license_key(&key)?;
    Ok(key)
}

fn check_license_key(key: &str) -> Result<(), Failure> {
    let printable = key.bytes().all(|b| (0x20..0x7f).contains(&b));
    if key.is_empty() || key.len() > MAX_LICENSE_KEY || !printable {
        return Err(Failure::new("invalid_license_key").with("max", MAX_LICENSE_KEY));
    }
    Ok(())
}

```

3. In `crates/ni-engine/src/profile.rs`, replace:

```rust
    #[serde(default)]
    pub config: ConfigMode,
}

```

with:

```rust
    #[serde(default)]
    pub config: ConfigMode,
    /// The official tool's mode script (`-sm`): a registered local `.rsc`
    /// file, uploaded as `mode.scr`. Not with [`ConfigMode::Keep`].
    #[serde(default)]
    pub mode_script: Option<String>,
    /// The official tool's `-b` (what devices do with it is unverified).
    #[serde(default)]
    pub remove_branding: bool,
    /// Software license key ([`license_key_from_file`]).
    #[serde(default)]
    pub license_key: Option<String>,
    #[serde(default)]
    pub after_install: AfterInstall,
}

```

4. In `crates/ni-engine/src/profile.rs`, replace:

```rust
            extra_packages: Vec::new(),
            config: ConfigMode::Default,
        }
    }

    /// Check the profile can be stored and used. Codes: `invalid_name`,
    /// `invalid_package_name`, `duplicate_package`, `too_many_packages`,
    /// `invalid_script_name`.
    pub fn validate(&self) -> Result<(), Failure> {
        if !valid_name(&self.name) {
```

with:

```rust
            extra_packages: Vec::new(),
            config: ConfigMode::Default,
            mode_script: None,
            remove_branding: false,
            license_key: None,
            after_install: AfterInstall::Reboot,
        }
    }

    /// The registered local files the profile uses: its script, then its
    /// mode script.
    pub fn scripts(&self) -> Vec<&str> {
        let script = match &self.config {
            ConfigMode::Script { local_name } => Some(local_name.as_str()),
            _ => None,
        };
        script
            .into_iter()
            .chain(self.mode_script.as_deref())
            .collect()
    }

    /// Check the profile can be stored and used. Codes: `invalid_name`,
    /// `invalid_package_name`, `duplicate_package`, `too_many_packages`,
    /// `invalid_script_name` (`name`), `mode_script_with_keep` (the
    /// official tool refuses it too), `invalid_license_key` (`max`).
    pub fn validate(&self) -> Result<(), Failure> {
        if !valid_name(&self.name) {
```

5. In `crates/ni-engine/src/profile.rs`, replace:

```rust
            }
        }
        if let ConfigMode::CustomScript { local_name } = &self.config {
            let ok = !local_name.is_empty()
                && local_name.len() <= MAX_LOCAL_NAME
```

with:

```rust
            }
        }
        for local_name in self.scripts() {
            let ok = !local_name.is_empty()
                && local_name.len() <= MAX_LOCAL_NAME
```

6. In `crates/ni-engine/src/profile.rs`, replace:

```rust
                return Err(Failure::new("invalid_script_name").with("name", local_name));
            }
        }
        Ok(())
```

with:

```rust
                return Err(Failure::new("invalid_script_name").with("name", local_name));
            }
        }
        if self.mode_script.is_some() && self.config == ConfigMode::Keep {
            return Err(Failure::new("mode_script_with_keep"));
        }
        if let Some(key) = &self.license_key {
            check_license_key(key)?;
        }
        Ok(())
```

7. In `crates/ni-engine/src/profile.rs`, replace:

```rust
            version: VersionSpec::Channel(Channel::Stable),
            extra_packages: vec!["wifi-qcom".into()],
            config: ConfigMode::CustomScript {
                local_name: "setup.rsc".into(),
            },
        }
    }
```

with:

```rust
            version: VersionSpec::Channel(Channel::Stable),
            extra_packages: vec!["wifi-qcom".into()],
            config: ConfigMode::Script {
                local_name: "setup.rsc".into(),
            },
            mode_script: Some("mode.rsc".into()),
            remove_branding: true,
            license_key: Some("KEY-1".into()),
            after_install: AfterInstall::Shutdown,
        }
    }
```

8. In `crates/ni-engine/src/profile.rs`, replace:

```rust
            json!({"name": "hAP ax2 stable", "version": "stable",
                   "extra_packages": ["wifi-qcom"],
                   "config": {"mode": "custom_script", "local_name": "setup.rsc"}})
        );
        let minimal: Profile =
```

with:

```rust
            json!({"name": "hAP ax2 stable", "version": "stable",
                   "extra_packages": ["wifi-qcom"],
                   "config": {"mode": "script", "local_name": "setup.rsc"},
                   "mode_script": "mode.rsc", "remove_branding": true,
                   "license_key": "KEY-1", "after_install": "shutdown"})
        );
        let minimal: Profile =
```

9. In `crates/ni-engine/src/profile.rs`, replace:

```rust
            serde_json::from_value::<Profile>(json!({"name": "x", "version": "6.49"})).is_err()
        );
        assert!(
            serde_json::from_value::<Profile>(json!({"name": "x", "version": "7.1",
                "config": {"mode": "keep"}}))
            .is_err(),
            "keep is Plan 5"
        );
    }

```

with:

```rust
            serde_json::from_value::<Profile>(json!({"name": "x", "version": "6.49"})).is_err()
        );
        for (mode, want) in [
            ("keep", ConfigMode::Keep),
            ("default", ConfigMode::Default),
            ("empty", ConfigMode::Empty),
        ] {
            let p: Profile = serde_json::from_value(
                json!({"name": "x", "version": "7.1", "config": {"mode": mode}}),
            )
            .unwrap();
            assert_eq!((p.config.name(), &p.config), (mode, &want));
        }
        let after: Vec<AfterInstall> = ["reboot", "reboot_message", "shutdown"]
            .iter()
            .map(|a| serde_json::from_value(json!(a)).unwrap())
            .collect();
        assert_eq!(
            after,
            [
                AfterInstall::Reboot,
                AfterInstall::RebootMessage,
                AfterInstall::Shutdown
            ]
        );
    }

    /// A profile file written by Plans 4–6.
    #[test]
    fn old_profiles_load_and_are_saved_in_the_new_form() {
        let dir = tempdir("profiles-old");
        let store = ProfileStore::new(dir.to_path_buf());
        std::fs::write(
            dir.join("lab.json"),
            br#"{"name": "Lab", "version": "stable", "extra_packages": [],
                "config": {"mode": "custom_script", "local_name": "lab.rsc"}}"#,
        )
        .unwrap();
        let p = store.load("Lab").unwrap();
        assert_eq!(
            p.config,
            ConfigMode::Script {
                local_name: "lab.rsc".into()
            }
        );
        assert_eq!(
            (
                p.mode_script.as_deref(),
                p.remove_branding,
                p.license_key.as_deref()
            ),
            (None, false, None)
        );
        assert_eq!(p.after_install, AfterInstall::Reboot);
        store.save(&p).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("lab.json")).unwrap()).unwrap();
        assert_eq!(
            v["config"],
            json!({"mode": "script", "local_name": "lab.rsc"})
        );
        assert_eq!(v["after_install"], "reboot");
    }

    #[test]
    fn license_keys_come_from_key_files() {
        let file = b"-----BEGIN MIKROTIK SOFTWARE KEY------------\r\nAAAA\nBBBB==\n-----END\n";
        assert_eq!(
            license_key_from_file(file).unwrap(),
            "-----BEGIN MIKROTIK SOFTWARE KEY------------AAAABBBB==-----END"
        );
        for bad in [
            &b""[..],
            b"\n\r\n",
            b"key\0x",
            "kulcs\u{e9}".as_bytes(),
            &[b'k'; MAX_LICENSE_KEY + 1],
        ] {
            let e = license_key_from_file(bad).unwrap_err();
            assert_eq!(e.code, "invalid_license_key", "{bad:?}");
            assert_eq!(e.param("max"), Some("4096"));
        }
        assert!(license_key_from_file(&[b'k'; MAX_LICENSE_KEY]).is_ok());
    }

```

10. In `crates/ni-engine/src/profile.rs`, replace:

```rust
        for bad in ["", "setup.txt", "Setup.rsc", "../x.rsc", ".rsc"] {
            let mut p = sample();
            p.config = ConfigMode::CustomScript {
                local_name: bad.into(),
            };
            assert_eq!(code(p), "invalid_script_name", "{bad:?}");
        }
    }

```

with:

```rust
        for bad in ["", "setup.txt", "Setup.rsc", "../x.rsc", ".rsc"] {
            let mut p = sample();
            p.config = ConfigMode::Script {
                local_name: bad.into(),
            };
            assert_eq!(code(p), "invalid_script_name", "{bad:?}");
            let mut p = sample();
            p.mode_script = Some(bad.into());
            assert_eq!(code(p), "invalid_script_name", "mode script {bad:?}");
        }
        let mut p = sample();
        p.config = ConfigMode::Keep;
        assert_eq!(code(p.clone()), "mode_script_with_keep");
        p.mode_script = None;
        assert_eq!(p.validate(), Ok(()));
        for mode in [ConfigMode::Default, ConfigMode::Empty] {
            let mut p = sample();
            p.config = mode;
            assert_eq!(p.validate(), Ok(()), "a mode script with -r / -e");
        }
        let mut p = sample();
        p.license_key = Some("two\nlines".into());
        assert_eq!(code(p), "invalid_license_key");
        assert_eq!(sample().scripts(), ["setup.rsc", "mode.rsc"]);
    }

```

- [ ] **Step 12: Regenerate `app/src/lib/codes.json`**

```bash
UPDATE_CODES=1 cargo test -p mac-netinstall --test codes
```

Expected: `test result: ok. 3 passed`; `git diff --stat app/src/lib/codes.json` shows two added lines (`invalid_license_key`, `mode_script_with_keep`).

- [ ] **Step 13: Run the tests to verify they pass**

Run: `cargo test -p ni-engine && cargo test -p ni-cli && cargo test -p mac-netinstall --test codes && (cd app && npx vitest run tests/i18n.test.ts)`
Expected: every suite `ok`; ni-engine's unit tests `60 passed`, `tests/flash.rs` `3 passed`; Vitest `Tests  12 passed (12)`

- [ ] **Step 14: Commit**

```bash
git add app/src/lib/codes.json app/src/locales/en.json app/src/locales/hu.json crates/ni-cli/src/commands.rs crates/ni-cli/tests/cli.rs crates/ni-engine/src/codes.rs crates/ni-engine/src/lib.rs crates/ni-engine/src/preflight.rs crates/ni-engine/src/prepare.rs crates/ni-engine/src/profile.rs crates/ni-engine/tests/flash.rs
git commit -m "feat(engine): configuration modes in profiles (keep, default, empty, script, mode script, branding, license key, after install)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: ni-engine: no server MAC, own frames by address, config per device, min_os warning

**Files:**
- Modify: `app/src-tauri/tests/bridge.rs`
- Modify: `app/src/App.svelte`
- Modify: `app/src/lib/codes.json`
- Modify: `app/src/lib/types.ts`
- Modify: `app/src/locales/en.json`
- Modify: `app/src/locales/hu.json`
- Modify: `app/tests/App.test.ts`
- Modify: `app/tests/state.test.ts`
- Modify: `crates/ni-cli/src/cli.rs`
- Modify: `crates/ni-cli/src/commands.rs`
- Modify: `crates/ni-cli/src/flash.rs`
- Modify: `crates/ni-cli/src/output.rs`
- Modify: `crates/ni-cli/tests/cli.rs`
- Modify: `crates/ni-engine/src/codes.rs`
- Modify: `crates/ni-engine/src/config.rs`
- Modify: `crates/ni-engine/src/engine/core.rs`
- Modify: `crates/ni-engine/src/engine/device.rs`
- Modify: `crates/ni-engine/src/engine/mod.rs`
- Modify: `crates/ni-engine/src/preflight.rs`
- Modify: `crates/ni-engine/tests/common/mod.rs`
- Modify: `crates/ni-engine/tests/flash.rs`
- Modify: `crates/ni-engine/tests/lifecycle.rs`

Our frames no longer carry our MAC, so the engine stops needing one: `EngineConfig::server_mac`, `NetPlan::server_mac`, the `no_mac` failure (and its hint, translations and the GUI's local-code list) and `ni-cli`'s hidden `--server-mac` go. Looped-back broadcasts are dropped by source address (server IP and our netinstall port) instead of by MAC. `DeviceView` gets `config` (the profile's mode), and a hello whose `min_os` is newer than the version being installed gives a `min_os_newer` warning (`mac`, `min_os`, `version`) — the official tool installs anyway, and so do we.

- [ ] **Step 1: Update the tests in `app/src-tauri/tests/bridge.rs`**

1. In `app/src-tauri/tests/bridge.rs`, replace:

```rust
        .unwrap()
        .with_config_hook(Arc::new(move |cfg| {
            cfg.server_mac = Some(Mac([2, 0, 0, 0, 0, 1]));
            cfg.ports = Ports {
                bootp: 0,
```

with:

```rust
        .unwrap()
        .with_config_hook(Arc::new(move |cfg| {
            cfg.ports = Ports {
                bootp: 0,
```

- [ ] **Step 2: Update the tests in `app/tests/App.test.ts`**

1. In `app/tests/App.test.ts`, replace:

```ts
    info: { model: "RB5009UG+S+", arch: "arm64", license_id: "X", min_os: "7" },
    profile: "lab",
    version: "7.25.1",
    attempt: 1,
```

with:

```ts
    info: { model: "RB5009UG+S+", arch: "arm64", license_id: "X", min_os: "7" },
    profile: "lab",
    config: "default",
    version: "7.25.1",
    attempt: 1,
```

- [ ] **Step 3: Update the tests in `app/tests/state.test.ts`**

1. In `app/tests/state.test.ts`, replace:

```ts
        info: null,
        profile: null,
        version: null,
        attempt: 1,
```

with:

```ts
        info: null,
        profile: null,
        config: null,
        version: null,
        attempt: 1,
```

- [ ] **Step 4: Update the tests in `crates/ni-cli/tests/cli.rs`**

1. In `crates/ni-cli/tests/cli.rs`, replace:

```rust
            "--iface",
            "lo0",
            "--server-mac",
            "02:00:00:00:00:01",
            "--bootp-port",
            "0",
```

with:

```rust
            "--iface",
            "lo0",
            "--bootp-port",
            "0",
```

2. In `crates/ni-cli/tests/cli.rs`, replace:

```rust
        "--package",
        "wireless",
        "--server-mac",
        "02:00:00:00:00:01",
        "--bootp-port",
        "0",
```

with:

```rust
        "--package",
        "wireless",
        "--bootp-port",
        "0",
```

- [ ] **Step 5: Update the test helper `crates/ni-engine/tests/common/mod.rs`**

1. In `crates/ni-engine/tests/common/mod.rs`, replace:

```rust
use tokio::sync::broadcast;

pub const SERVER_MAC: Mac = Mac([0x02, 0, 0, 0, 0, 0x01]);
pub const A: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x0a]);
pub const B: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x0b]);
```

with:

```rust
use tokio::sync::broadcast;

pub const A: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x0a]);
pub const B: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x0b]);
```

2. In `crates/ni-engine/tests/common/mod.rs`, replace:

```rust
pub fn config(lan: &SimLan) -> EngineConfig {
    let mut cfg = EngineConfig::new("lo0");
    cfg.server_mac = Some(SERVER_MAC);
    cfg.ports = Ports {
        bootp: 0,
```

with:

```rust
pub fn config(lan: &SimLan) -> EngineConfig {
    let mut cfg = EngineConfig::new("lo0");
    cfg.ports = Ports {
        bootp: 0,
```

- [ ] **Step 6: Update the tests in `crates/ni-engine/tests/flash.rs`**

1. In `crates/ni-engine/tests/flash.rs`, replace:

```rust

use common::*;
use ni_engine::{ConfigMode, DeviceState, EngineEvent, WaitReason};
use ni_proto::message::CONFIG_SCRIPT_NAME;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
```

with:

```rust

use common::*;
use ni_engine::{AfterInstall, ConfigMode, DeviceState, EngineEvent, WaitReason};
use ni_proto::message::CONFIG_SCRIPT_NAME;
use ni_proto::Offer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
```

2. In `crates/ni-engine/tests/flash.rs`, replace:

```rust
    assert_eq!(snap.devices[0].state, DeviceState::Done);
    assert_eq!(snap.devices[0].profile.as_deref(), Some("test"));
    assert_eq!(snap.devices[0].boot_arch.as_deref(), Some("arm64"));
}

```

with:

```rust
    assert_eq!(snap.devices[0].state, DeviceState::Done);
    assert_eq!(snap.devices[0].profile.as_deref(), Some("test"));
    assert_eq!(snap.devices[0].config.as_deref(), Some("script"));
    assert_eq!(snap.devices[0].boot_arch.as_deref(), Some("arm64"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_profile_options_reach_the_device() {
    let mut p = profile();
    p.config = ConfigMode::Keep;
    p.remove_branding = true;
    p.license_key = Some("KEY-1".into());
    p.after_install = AfterInstall::Shutdown;
    let mut rig = rig(|cfg| cfg.profile = Some(p)).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
    let report = rig.lan.wait_terminated(A, WAIT).await.unwrap();
    assert_eq!(
        report.offer,
        Some(Offer {
            license_key: "KEY-1".into(),
            keep_config: true,
            remove_branding: true
        })
    );
    assert_eq!(report.after_install, Some(ni_proto::AfterInstall::Shutdown));
    assert_eq!(report.received.len(), 2, "packages only");
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!(snap.devices[0].config.as_deref(), Some("keep"));
}

```

- [ ] **Step 7: Update the tests in `crates/ni-engine/tests/lifecycle.rs`**

1. In `crates/ni-engine/tests/lifecycle.rs`, replace:

```rust

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_device_that_netboots_again_is_reflashed() {
    let mut rig = rig(|_| {}).await;
```

with:

```rust

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_min_os_is_only_a_warning() {
    let mut rig = rig(|_| {}).await;
    let mut newer = sim_config(A);
    newer.info.min_os = "7.99".into();
    let sim = boot(&rig, A, newer).await;
    rig.lan.add(sim).await;
    let w = next(
        &mut rig.events,
        |e| matches!(e, EngineEvent::Warning(f) if f.code == "min_os_newer"),
    )
    .await;
    let EngineEvent::Warning(f) = w else {
        unreachable!()
    };
    assert_eq!(f.param("min_os"), Some("7.99"));
    assert_eq!(f.param("version"), Some("7.25.1"));
    assert_eq!(f.param("mac"), Some("48:A9:8A:00:00:0A"));
    state(&mut rig.events, A, "done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_device_that_netboots_again_is_reflashed() {
    let mut rig = rig(|_| {}).await;
```

- [ ] **Step 8: Run it to verify it fails**

Run: `cargo test -p ni-engine --test flash`
Expected: `cargo test -p ni-engine --test flash` does not compile: `no field `config` on type `DeviceView``

- [ ] **Step 9: Edit `app/src/App.svelte`**

1. In `app/src/App.svelte`, replace:

```svelte
    "no_ipv4",
    "server_ip_not_on_interface",
    "no_mac",
    "invalid_netmask",
    "server_ip_not_host",
```

with:

```svelte
    "no_ipv4",
    "server_ip_not_on_interface",
    "invalid_netmask",
    "server_ip_not_host",
```

- [ ] **Step 10: Edit `app/src/lib/types.ts`**

1. In `app/src/lib/types.ts`, replace:

```ts
  pool_start: string;
  pool_end: string;
  server_mac: string;
}

```

with:

```ts
  pool_start: string;
  pool_end: string;
}

```

2. In `app/src/lib/types.ts`, replace:

```ts
  info: DeviceInfoView | null;
  profile: string | null;
  version: string | null;
  attempt: number;
```

with:

```ts
  info: DeviceInfoView | null;
  profile: string | null;
  /** The profile's configuration mode: keep, default, empty or script. */
  config: string | null;
  version: string | null;
  attempt: number;
```

- [ ] **Step 11: Edit `app/src/locales/en.json`**

1. In `app/src/locales/en.json`, replace:

```json
  "code.link_down": "{interface}: no link ({reason}).",
  "code.link_up": "{interface}: link is back.",
  "code.mode_script_with_keep": "A mode script cannot be combined with keeping the existing configuration: choose the default, empty or own script configuration.",
  "code.no_ipv4": "{interface} has no IPv4 address.",
  "code.no_mac": "The MAC address of {interface} is unknown.",
  "code.no_manual_ipv4": "{service} ({interface}) has no manual IPv4 address in macOS.",
  "code.no_profile": "Choose a profile first.",
```

with:

```json
  "code.link_down": "{interface}: no link ({reason}).",
  "code.link_up": "{interface}: link is back.",
  "code.min_os_newer": "{mac} reports needing RouterOS {min_os} or newer; installing {version} anyway (the official tool does not check this either).",
  "code.mode_script_with_keep": "A mode script cannot be combined with keeping the existing configuration: choose the default, empty or own script configuration.",
  "code.no_ipv4": "{interface} has no IPv4 address.",
  "code.no_manual_ipv4": "{service} ({interface}) has no manual IPv4 address in macOS.",
  "code.no_profile": "Choose a profile first.",
```

2. In `app/src/locales/en.json`, replace:

```json
  "hint.unsuitable_interface": "Choose a wired Ethernet adapter (built-in, USB, Thunderbolt or a dock).",
  "hint.interface_not_ethernet": "Choose a wired Ethernet adapter (built-in, USB, Thunderbolt or a dock). Wi-Fi, Thunderbolt ports (IP over Thunderbolt), bridges and virtual interfaces cannot netinstall a RouterBOARD.",
  "hint.no_mac": "macOS hides this interface's hardware address. Use a wired Ethernet adapter.",
  "hint.default_route": "Devices on that network could get addresses from this app. Use a separate adapter connected only to the router.",
  "hint.server_ip_not_on_interface": "Set this address on the interface in System Settings → Network, or change the server IP in Settings.",
```

with:

```json
  "hint.unsuitable_interface": "Choose a wired Ethernet adapter (built-in, USB, Thunderbolt or a dock).",
  "hint.interface_not_ethernet": "Choose a wired Ethernet adapter (built-in, USB, Thunderbolt or a dock). Wi-Fi, Thunderbolt ports (IP over Thunderbolt), bridges and virtual interfaces cannot netinstall a RouterBOARD.",
  "hint.default_route": "Devices on that network could get addresses from this app. Use a separate adapter connected only to the router.",
  "hint.server_ip_not_on_interface": "Set this address on the interface in System Settings → Network, or change the server IP in Settings.",
```

- [ ] **Step 12: Edit `app/src/locales/hu.json`**

1. In `app/src/locales/hu.json`, replace:

```json
  "code.link_down": "{interface}: nincs kapcsolat ({reason}).",
  "code.link_up": "{interface}: a kapcsolat helyreállt.",
  "code.mode_script_with_keep": "Mode script nem használható a meglévő konfiguráció megtartásával: válaszd a gyári, az üres vagy a saját szkriptes konfigurációt.",
  "code.no_ipv4": "{interface}: nincs IPv4-címe.",
  "code.no_mac": "{interface}: a MAC-cím ismeretlen.",
  "code.no_manual_ipv4": "{service} ({interface}): nincs kézzel beállított IPv4-cím a macOS-ben.",
  "code.no_profile": "Előbb válassz profilt.",
```

with:

```json
  "code.link_down": "{interface}: nincs kapcsolat ({reason}).",
  "code.link_up": "{interface}: a kapcsolat helyreállt.",
  "code.min_os_newer": "{mac} legalább RouterOS {min_os} verziót igényel; a(z) {version} verzió ennek ellenére települ (a hivatalos eszköz sem ellenőrzi ezt).",
  "code.mode_script_with_keep": "Mode script nem használható a meglévő konfiguráció megtartásával: válaszd a gyári, az üres vagy a saját szkriptes konfigurációt.",
  "code.no_ipv4": "{interface}: nincs IPv4-címe.",
  "code.no_manual_ipv4": "{service} ({interface}): nincs kézzel beállított IPv4-cím a macOS-ben.",
  "code.no_profile": "Előbb válassz profilt.",
```

2. In `app/src/locales/hu.json`, replace:

```json
  "hint.unsuitable_interface": "Válassz vezetékes Ethernet-adaptert (beépített, USB, Thunderbolt vagy dokkoló).",
  "hint.interface_not_ethernet": "Válassz vezetékes Ethernet-adaptert (beépített, USB, Thunderbolt vagy dokkoló). Wi-Fi, Thunderbolt-port (IP Thunderbolton), híd vagy virtuális csatoló nem alkalmas RouterBOARD telepítésére.",
  "hint.no_mac": "A macOS elrejti ennek a csatolónak a hardvercímét. Használj vezetékes Ethernet-adaptert.",
  "hint.default_route": "Az azon a hálózaton lévő gépek is kaphatnak címet ettől az apptól. Használj külön adaptert, amely csak a routerhez csatlakozik.",
  "hint.server_ip_not_on_interface": "Állítsd be ezt a címet a csatolón (Rendszerbeállítások → Hálózat), vagy módosítsd a szerver IP-címét a Beállításokban.",
```

with:

```json
  "hint.unsuitable_interface": "Válassz vezetékes Ethernet-adaptert (beépített, USB, Thunderbolt vagy dokkoló).",
  "hint.interface_not_ethernet": "Válassz vezetékes Ethernet-adaptert (beépített, USB, Thunderbolt vagy dokkoló). Wi-Fi, Thunderbolt-port (IP Thunderbolton), híd vagy virtuális csatoló nem alkalmas RouterBOARD telepítésére.",
  "hint.default_route": "Az azon a hálózaton lévő gépek is kaphatnak címet ettől az apptól. Használj külön adaptert, amely csak a routerhez csatlakozik.",
  "hint.server_ip_not_on_interface": "Állítsd be ezt a címet a csatolón (Rendszerbeállítások → Hálózat), vagy módosítsd a szerver IP-címét a Beállításokban.",
```

- [ ] **Step 13: Edit `crates/ni-cli/src/cli.rs` (with its unit tests)**

1. In `crates/ni-cli/src/cli.rs`, replace:

```rust

/// Hidden testing flags that redirect downloads or frames
/// (`--download-base`, `--upgrade-base`, `--netinstall-dest`,
/// `--server-mac`) are refused unless [`TESTING_ENV`] is `1`: pointed at
/// the wrong place they would install foreign files or impersonate
/// another machine. Error: `testing_flag_not_allowed` (`flag`, `env`).
pub fn check_testing_flags(cli: &Cli, allowed: bool) -> Result<(), ni_engine::Failure> {
    if allowed {
```

with:

```rust

/// Hidden testing flags that redirect downloads or frames
/// (`--download-base`, `--upgrade-base`, `--netinstall-dest`) are refused
/// unless [`TESTING_ENV`] is `1`: pointed at the wrong place they would
/// install foreign files or send frames elsewhere. Error: `testing_flag_not_allowed` (`flag`, `env`).
pub fn check_testing_flags(cli: &Cli, allowed: bool) -> Result<(), ni_engine::Failure> {
    if allowed {
```

2. In `crates/ni-cli/src/cli.rs`, replace:

```rust
            net.is_some_and(|n| n.netinstall_dest.is_some()),
        ),
        ("--server-mac", net.is_some_and(|n| n.server_mac.is_some())),
    ];
    match used.iter().find(|(_, on)| *on) {
```

with:

```rust
            net.is_some_and(|n| n.netinstall_dest.is_some()),
        ),
    ];
    match used.iter().find(|(_, on)| *on) {
```

3. In `crates/ni-cli/src/cli.rs`, replace:

```rust
    #[arg(long, value_parser = parse_pool)]
    pub pool: Option<(Ipv4Addr, Ipv4Addr)>,
    /// Testing: our Netinstall source MAC (for interfaces without one).
    #[arg(long, hide = true)]
    pub server_mac: Option<Mac>,
    /// Testing: listener address [default: 0.0.0.0].
    #[arg(long, hide = true)]
```

with:

```rust
    #[arg(long, value_parser = parse_pool)]
    pub pool: Option<(Ipv4Addr, Ipv4Addr)>,
    /// Testing: listener address [default: 0.0.0.0].
    #[arg(long, hide = true)]
```

4. In `crates/ni-cli/src/cli.rs`, replace:

```rust
            "--version",
            "7.25.1",
            "--server-mac",
            "02:00:00:00:00:01",
            "--bootp-port",
            "0",
```

with:

```rust
            "--version",
            "7.25.1",
            "--bootp-port",
            "0",
```

5. In `crates/ni-cli/src/cli.rs`, replace:

```rust
            &["--download-base", "http://x/", "interfaces"][..],
            &["--upgrade-base", "http://x/", "interfaces"],
            &[
                "flash",
                "--iface",
                "en7",
                "--version",
                "stable",
                "--server-mac",
                "02:00:00:00:00:01",
            ],
            &[
                "preflight",
```

with:

```rust
            &["--download-base", "http://x/", "interfaces"][..],
            &["--upgrade-base", "http://x/", "interfaces"],
            &[
                "preflight",
```

- [ ] **Step 14: Edit `crates/ni-cli/src/commands.rs` (with its unit tests)**

1. In `crates/ni-cli/src/commands.rs`, replace:

```rust
    cfg.netmask = net.netmask;
    cfg.pool = net.pool;
    cfg.server_mac = net.server_mac;
    let p = &mut cfg.ports;
    if let Some(ip) = net.bind_ip {
```

with:

```rust
    cfg.netmask = net.netmask;
    cfg.pool = net.pool;
    let p = &mut cfg.ports;
    if let Some(ip) = net.bind_ip {
```

2. In `crates/ni-cli/src/commands.rs`, replace:

```rust
        if let Some(n) = &report.network {
            println!(
                "server {} / {}, pool {} - {}, MAC {}",
                n.server_ip, n.netmask, n.pool_start, n.pool_end, n.server_mac
            );
        }
```

with:

```rust
        if let Some(n) = &report.network {
            println!(
                "server {} / {}, pool {} - {}",
                n.server_ip, n.netmask, n.pool_start, n.pool_end
            );
        }
```

3. In `crates/ni-cli/src/commands.rs`, replace:

```rust
            "--iface",
            "lo0",
            "--server-mac",
            "02:00:00:00:00:01",
            "--bootp-port",
            "6700",
```

with:

```rust
            "--iface",
            "lo0",
            "--bootp-port",
            "6700",
```

4. In `crates/ni-cli/src/commands.rs`, replace:

```rust
        assert_eq!(cfg.interface, "lo0");
        assert_eq!(cfg.server_ip, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(cfg.server_mac, Some(ni_proto::Mac([2, 0, 0, 0, 0, 1])));
        assert_eq!(
            (cfg.ports.bootp, cfg.ports.tftp, cfg.ports.netinstall),
```

with:

```rust
        assert_eq!(cfg.interface, "lo0");
        assert_eq!(cfg.server_ip, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(
            (cfg.ports.bootp, cfg.ports.tftp, cfg.ports.netinstall),
```

5. In `crates/ni-cli/src/commands.rs`, replace:

```rust
            netmask: None,
            pool: None,
            server_mac: None,
            bind_ip: None,
            bootp_port: None,
```

with:

```rust
            netmask: None,
            pool: None,
            bind_ip: None,
            bootp_port: None,
```

- [ ] **Step 15: Edit `crates/ni-cli/src/flash.rs` (with its unit tests)**

1. In `crates/ni-cli/src/flash.rs`, replace:

```rust
            }),
            profile: None,
            version: None,
            attempt: 1,
```

with:

```rust
            }),
            profile: None,
            config: None,
            version: None,
            attempt: 1,
```

- [ ] **Step 16: Edit `crates/ni-cli/src/output.rs` (with its unit tests)**

1. In `crates/ni-cli/src/output.rs`, replace:

```rust
        "no_ipv4" => Some("give the interface a static IPv4 address in System Settings > Network"),
        "unsuitable_interface" => Some("RouterBOARDs boot over wired Ethernet only"),
        "no_mac" => Some(
            "the interface's real MAC address is unknown (macOS hides it from some apps); \
             use a wired Ethernet interface",
        ),
        _ => None,
    }
```

with:

```rust
        "no_ipv4" => Some("give the interface a static IPv4 address in System Settings > Network"),
        "unsuitable_interface" => Some("RouterBOARDs boot over wired Ethernet only"),
        _ => None,
    }
```

2. In `crates/ni-cli/src/output.rs`, replace:

```rust
        );
        assert_eq!(hint("flash_timeout"), None);
        assert!(hint("no_mac").is_some_and(|h| h.contains("MAC")));
    }
}
```

with:

```rust
        );
        assert_eq!(hint("flash_timeout"), None);
    }
}
```

- [ ] **Step 17: Edit `crates/ni-engine/src/codes.rs`**

1. In `crates/ni-engine/src/codes.rs`, replace:

```rust
    "mode_script_with_keep",
    "no_ipv4",
    "no_mac",
    "no_manual_ipv4",
    "no_profile",
```

with:

```rust
    "mode_script_with_keep",
    "no_ipv4",
    "min_os_newer",
    "no_manual_ipv4",
    "no_profile",
```

- [ ] **Step 18: Edit `crates/ni-engine/src/config.rs` (with its unit tests)**

1. In `crates/ni-engine/src/config.rs`, replace:

```rust
    /// First and last address handed out; default: see [`auto_pool`].
    pub pool: Option<(Ipv4Addr, Ipv4Addr)>,
    /// Source MAC of our Netinstall frames; default: the interface's MAC.
    pub server_mac: Option<Mac>,
    /// The active profile: given to new devices when auto-repeat is on.
    /// Its version also selects the boot kernels (stable without one).
```

with:

```rust
    /// First and last address handed out; default: see [`auto_pool`].
    pub pool: Option<(Ipv4Addr, Ipv4Addr)>,
    /// The active profile: given to new devices when auto-repeat is on.
    /// Its version also selects the boot kernels (stable without one).
```

2. In `crates/ni-engine/src/config.rs`, replace:

```rust
            netmask: None,
            pool: None,
            server_mac: None,
            profile: None,
            auto_repeat: false,
```

with:

```rust
            netmask: None,
            pool: None,
            profile: None,
            auto_repeat: false,
```

3. In `crates/ni-engine/src/config.rs`, replace:

```rust
    pub pool_start: Ipv4Addr,
    pub pool_end: Ipv4Addr,
    #[serde(with = "crate::event::mac_str")]
    pub server_mac: Mac,
}

```

with:

```rust
    pub pool_start: Ipv4Addr,
    pub pool_end: Ipv4Addr,
}

```

4. In `crates/ni-engine/src/config.rs`, replace:

```rust

/// Resolve `cfg` against the interface list. Codes: `unknown_interface`,
/// `no_ipv4`, `server_ip_not_on_interface`, `no_mac` (neither a configured
/// server MAC nor a known interface MAC; placeholders such as
/// `02:00:00:00:00:00` count as unknown), `pool_includes_reserved`.
///
/// A configured `server_ip` that is not on the interface is accepted
```

with:

```rust

/// Resolve `cfg` against the interface list. Codes: `unknown_interface`,
/// `no_ipv4`, `server_ip_not_on_interface`, `pool_includes_reserved`.
///
/// No MAC address is needed: our Netinstall frames carry the device's
/// fields, never our own MAC (as the official tool's), so an interface
/// whose MAC macOS masks works too.
///
/// A configured `server_ip` that is not on the interface is accepted
```

5. In `crates/ni-engine/src/config.rs`, replace:

```rust
        None => auto_pool(server_ip, netmask)?,
    };
    // Never fall back to a placeholder: macOS may mask link addresses as
    // 02:00:00:00:00:00, and frames from it would collide between Macs.
    let server_mac = cfg
        .server_mac
        .or(iface.mac)
        .and_then(ni_net::known_mac)
        .ok_or_else(|| Failure::new("no_mac").with("interface", &iface.name))?;
    Ok(NetPlan {
        server_ip,
```

with:

```rust
        None => auto_pool(server_ip, netmask)?,
    };
    Ok(NetPlan {
        server_ip,
```

6. In `crates/ni-engine/src/config.rs`, replace:

```rust
        pool_start,
        pool_end,
        server_mac,
    })
}
```

with:

```rust
        pool_start,
        pool_end,
    })
}
```

7. In `crates/ni-engine/src/config.rs`, replace:

```rust
                pool_start: ip(2),
                pool_end: ip(254),
                server_mac: MAC,
            }
        );
```

with:

```rust
                pool_start: ip(2),
                pool_end: ip(254),
            }
        );
```

8. In `crates/ni-engine/src/config.rs`, replace:

```rust
        cfg.server_ip = Some(ip(2));
        cfg.pool = Some((ip(100), ip(110)));
        cfg.server_mac = Some(Mac([2; 6]));
        let plan = resolve_network(&ifaces, &cfg).unwrap();
        assert_eq!(
            (
                plan.server_ip,
                plan.pool_start,
                plan.pool_end,
                plan.server_mac
            ),
            (ip(2), ip(100), ip(110), Mac([2; 6]))
        );
        let code = |cfg: &EngineConfig| resolve_network(&ifaces, cfg).unwrap_err().code;
```

with:

```rust
        cfg.server_ip = Some(ip(2));
        cfg.pool = Some((ip(100), ip(110)));
        let plan = resolve_network(&ifaces, &cfg).unwrap();
        assert_eq!(
            (plan.server_ip, plan.pool_start, plan.pool_end),
            (ip(2), ip(100), ip(110))
        );
        let code = |cfg: &EngineConfig| resolve_network(&ifaces, cfg).unwrap_err().code;
```

9. In `crates/ni-engine/src/config.rs`, replace:

```rust
        assert_eq!(code(&EngineConfig::new("en9")), "unknown_interface");
        assert_eq!(code(&EngineConfig::new("en8")), "no_ipv4");
        let ifaces = [iface("en8", &[(ip(1), MASK24)], None)];
        let err = resolve_network(&ifaces, &EngineConfig::new("en8")).unwrap_err();
        assert_eq!(err, Failure::new("no_mac").with("interface", "en8"));
    }

    #[test]
    fn placeholder_macs_count_as_unknown() {
        // macOS masks link addresses as 02:00:00:00:00:00; never send
        // frames from it (or from all-zero).
        for placeholder in [ni_net::MASKED_MAC, Mac::ZERO] {
            let ifaces = [iface("en8", &[(ip(1), MASK24)], Some(placeholder))];
            let err = resolve_network(&ifaces, &EngineConfig::new("en8")).unwrap_err();
            assert_eq!(err, Failure::new("no_mac").with("interface", "en8"));
            let mut cfg = EngineConfig::new("en8");
            cfg.server_mac = Some(placeholder);
            let err = resolve_network(&ifaces, &cfg).unwrap_err();
            assert_eq!(err.code, "no_mac");
            // An explicit real MAC still works (the loopback tests use one).
            cfg.server_mac = Some(MAC);
            assert_eq!(resolve_network(&ifaces, &cfg).unwrap().server_mac, MAC);
        }
    }
```

with:

```rust
        assert_eq!(code(&EngineConfig::new("en9")), "unknown_interface");
        assert_eq!(code(&EngineConfig::new("en8")), "no_ipv4");
    }

    #[test]
    fn an_unknown_or_masked_mac_is_fine() {
        // Our frames never carry our MAC, so macOS masking it
        // (02:00:00:00:00:00) or not reporting one does not matter.
        for mac in [None, Some(ni_net::MASKED_MAC), Some(Mac::ZERO)] {
            let ifaces = [iface("en8", &[(ip(1), MASK24)], mac)];
            let plan = resolve_network(&ifaces, &EngineConfig::new("en8")).unwrap();
            assert_eq!(plan.server_ip, ip(1));
        }
    }
```

- [ ] **Step 19: Edit `crates/ni-engine/src/engine/core.rs` (with its unit tests)**

1. In `crates/ni-engine/src/engine/core.rs`, replace:

```rust
            return;
        }
        let state = match result {
            Err(f) => DeviceState::failed(f),
            Ok((version, plan)) => {
                let info = dev.info.clone().expect("prepared devices have said hello");
                dev.version = Some(version.clone());
                match ServerSession::new(info, &plan, session_cfg, Instant::now()) {
```

with:

```rust
            return;
        }
        let mut newer = None;
        let state = match result {
            Err(f) => DeviceState::failed(f),
            Ok((version, plan)) => {
                let info = dev.info.clone().expect("prepared devices have said hello");
                // The official tool does not enforce the device's minimum
                // version either; say so and go on.
                newer = Version::parse(&info.min_os)
                    .ok()
                    .filter(|min| *min > version)
                    .map(|min| {
                        Failure::new("min_os_newer")
                            .with("mac", mac)
                            .with("min_os", min)
                            .with("version", &version)
                    });
                dev.version = Some(version.clone());
                match ServerSession::new(info, &plan, session_cfg, Instant::now()) {
```

2. In `crates/ni-engine/src/engine/core.rs`, replace:

```rust
            }
        };
        self.set_state(mac, state);
        self.admit();
```

with:

```rust
            }
        };
        if let Some(f) = newer {
            self.warning(f);
        }
        self.set_state(mac, state);
        self.admit();
```

3. In `crates/ni-engine/src/engine/core.rs`, replace:

```rust
    fn on_datagram(&mut self, r: io::Result<(usize, SocketAddr)>, buf: &[u8], io: &mut Option<Io>) {
        let now = Instant::now();
        let n = match r {
            Ok((n, _)) => {
                if let Some(io) = io.as_mut() {
                    io.recv_errors.on_success();
                }
                n
            }
            Err(e) => {
```

with:

```rust
    fn on_datagram(&mut self, r: io::Result<(usize, SocketAddr)>, buf: &[u8], io: &mut Option<Io>) {
        let now = Instant::now();
        let (n, from) = match r {
            Ok((n, from)) => {
                if let Some(io) = io.as_mut() {
                    io.recv_errors.on_success();
                }
                (n, from)
            }
            Err(e) => {
```

4. In `crates/ni-engine/src/engine/core.rs`, replace:

```rust
        };
        let Some(run) = self.run.as_mut() else { return };
        if frame.src == run.net.server_mac {
            return; // our own broadcast
        }
        if let Some(session) = run
```

with:

```rust
        };
        let Some(run) = self.run.as_mut() else { return };
        // Our own broadcasts come back from our address and port. (Their
        // MAC fields are the device's, so they cannot be told apart by
        // MAC; sessions would ignore them anyway: `ack` is `seq - 2`.)
        let own_port = io
            .as_ref()
            .and_then(|io| io.socket.local_addr().ok())
            .map(|a| a.port());
        if from.ip() == IpAddr::V4(run.net.server_ip) && Some(from.port()) == own_port {
            return;
        }
        if let Some(session) = run
```

- [ ] **Step 20: Edit `crates/ni-engine/src/engine/device.rs` (with its unit tests)**

1. In `crates/ni-engine/src/engine/device.rs`, replace:

```rust
    }

    pub fn view(&self) -> Option<DeviceView> {
        Some(DeviceView {
```

with:

```rust
    }

    /// The profile of the current attempt, else the assigned one.
    fn profile(&self) -> Option<&Profile> {
        self.using.as_ref().or(self.assigned.as_ref())
    }

    pub fn view(&self) -> Option<DeviceView> {
        Some(DeviceView {
```

2. In `crates/ni-engine/src/engine/device.rs`, replace:

```rust
            boot_arch: self.boot_arch.map(|a| a.as_str().to_string()),
            info: self.info.as_ref().map(DeviceInfoView::from),
            profile: self
                .using
                .as_ref()
                .or(self.assigned.as_ref())
                .map(|p| p.name.clone()),
            version: self.version.as_ref().map(|v| v.to_string()),
            attempt: self.attempt,
```

with:

```rust
            boot_arch: self.boot_arch.map(|a| a.as_str().to_string()),
            info: self.info.as_ref().map(DeviceInfoView::from),
            profile: self.profile().map(|p| p.name.clone()),
            config: self.profile().map(|p| p.config.name().to_string()),
            version: self.version.as_ref().map(|v| v.to_string()),
            attempt: self.attempt,
```

3. In `crates/ni-engine/src/engine/device.rs`, replace:

```rust
        );
        assert_eq!(unix_ms(UNIX_EPOCH - Duration::from_secs(1)), 0);
    }

```

with:

```rust
        );
        assert_eq!(unix_ms(UNIX_EPOCH - Duration::from_secs(1)), 0);
        assert_eq!((v.profile, v.config), (None, None));
        let mut p = Profile::new(
            "lab",
            ni_assets::VersionSpec::Exact("7.25.1".parse().unwrap()),
        );
        p.config = crate::ConfigMode::Empty;
        d.assigned = Some(p);
        let v = d.view().unwrap();
        assert_eq!(v.profile.as_deref(), Some("lab"));
        assert_eq!(v.config.as_deref(), Some("empty"));
    }

```

- [ ] **Step 21: Edit `crates/ni-engine/src/engine/mod.rs`**

1. In `crates/ni-engine/src/engine/mod.rs`, replace:

```rust
    /// Name of the profile assigned with `flash` (or in use).
    pub profile: Option<String>,
    /// RouterOS version of the current (or last) install attempt, once
    /// resolved.
```

with:

```rust
    /// Name of the profile assigned with `flash` (or in use).
    pub profile: Option<String>,
    /// That profile's configuration mode: `keep`, `default`, `empty` or
    /// `script`.
    pub config: Option<String>,
    /// RouterOS version of the current (or last) install attempt, once
    /// resolved.
```

- [ ] **Step 22: Edit `crates/ni-engine/src/preflight.rs` (with its unit tests)**

1. In `crates/ni-engine/src/preflight.rs`, replace:

```rust

/// Interface and address checks. Errors: `unknown_interface`,
/// `interface_down`, `no_ipv4`, `server_ip_not_on_interface`, `no_mac`,
/// `invalid_netmask`, `server_ip_not_host`, `pool_inverted`,
/// `pool_outside_subnet`, `pool_includes_reserved` (the network or
```

with:

```rust

/// Interface and address checks. Errors: `unknown_interface`,
/// `interface_down`, `no_ipv4`, `server_ip_not_on_interface`,
/// `invalid_netmask`, `server_ip_not_host`, `pool_inverted`,
/// `pool_outside_subnet`, `pool_includes_reserved` (the network or
```

2. In `crates/ni-engine/src/preflight.rs`, replace:

```rust
        let held = bind_udp(&BindOptions::new(0).addr(Ipv4Addr::LOCALHOST)).unwrap();
        let mut cfg = EngineConfig::new("lo0");
        cfg.server_mac = Some(MAC);
        cfg.ports.bind_ip = Ipv4Addr::LOCALHOST;
        cfg.ports.bootp = 0;
```

with:

```rust
        let held = bind_udp(&BindOptions::new(0).addr(Ipv4Addr::LOCALHOST)).unwrap();
        let mut cfg = EngineConfig::new("lo0");
        cfg.ports.bind_ip = Ipv4Addr::LOCALHOST;
        cfg.ports.bootp = 0;
```

3. In `crates/ni-engine/src/preflight.rs`, replace:

```rust
    async fn run_on_loopback_warns_about_the_interface_kind() {
        let mut cfg = EngineConfig::new("lo0");
        cfg.server_mac = Some(MAC);
        cfg.ports.bootp = 0;
        cfg.ports.tftp = 0;
```

with:

```rust
    async fn run_on_loopback_warns_about_the_interface_kind() {
        let mut cfg = EngineConfig::new("lo0");
        cfg.ports.bootp = 0;
        cfg.ports.tftp = 0;
```

- [ ] **Step 23: Regenerate `app/src/lib/codes.json`**

```bash
UPDATE_CODES=1 cargo test -p mac-netinstall --test codes
```

Expected: `test result: ok. 3 passed`; `git diff --stat app/src/lib/codes.json` shows `min_os_newer` added, `no_mac` removed.

- [ ] **Step 24: Run the tests to verify they pass**

Run: `cargo test --workspace && (cd app && npm run check && npx vitest run)`
Expected: every suite `ok` (559 Rust tests) (`ni-assets`'s `listings_reuse_verification_results_of_unchanged_files` is flaky before Task 12 — a process-wide hash counter shared by parallel tests; if it alone fails, run again); svelte-check `0 ERRORS 0 WARNINGS`; Vitest `Tests  123 passed (123)`

- [ ] **Step 25: Commit**

```bash
git add app/src-tauri/tests/bridge.rs app/src/App.svelte app/src/lib/codes.json app/src/lib/types.ts app/src/locales/en.json app/src/locales/hu.json app/tests/App.test.ts app/tests/state.test.ts crates/ni-cli/src/cli.rs crates/ni-cli/src/commands.rs crates/ni-cli/src/flash.rs crates/ni-cli/src/output.rs crates/ni-cli/tests/cli.rs crates/ni-engine/src/codes.rs crates/ni-engine/src/config.rs crates/ni-engine/src/engine/core.rs crates/ni-engine/src/engine/device.rs crates/ni-engine/src/engine/mod.rs crates/ni-engine/src/preflight.rs crates/ni-engine/tests/common/mod.rs crates/ni-engine/tests/flash.rs crates/ni-engine/tests/lifecycle.rs
git commit -m "feat(engine): no server MAC needed; own frames by address; config mode per device; min_os warning" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: ni-cli: configuration flags

**Files:**
- Modify: `crates/ni-cli/src/cli.rs`
- Modify: `crates/ni-cli/src/commands.rs`
- Modify: `crates/ni-cli/src/flash.rs`
- Modify: `crates/ni-cli/tests/cli.rs`

`profiles save` and `flash --version` take `--config keep|default|empty`, `--script`, `--mode-script`, `--remove-branding`, `--license-key FILE` and `--after reboot|reboot-message|shutdown` (a flattened `ConfigArgs`; all of them conflict with `flash --profile`). `--script` with `--config keep` or `empty` is `script_with_config` (the official CLI refuses `-e -s` too); an unreadable key file is `license_key_failed`.

- [ ] **Step 1: Update the tests in `crates/ni-cli/tests/cli.rs`**

1. In `crates/ni-cli/tests/cli.rs`, replace:

```rust
    );
    assert!(env.logs().contains("ni-cli started"));
}

```

with:

```rust
    );
    assert!(env.logs().contains("ni-cli started"));
}

#[tokio::test]
async fn profiles_take_the_configuration_options() {
    let env = Env::new("cli-profile-options");
    let mode = env.dir.join("Mode.rsc");
    std::fs::write(&mode, b":log info mode\n").unwrap();
    let key = env.dir.join("device.key");
    std::fs::write(
        &key,
        b"-----BEGIN KEY-----\nAAAA\r\nBBBB\n-----END KEY-----\n",
    )
    .unwrap();
    let (mode, key) = (mode.display().to_string(), key.display().to_string());
    let save = |extra: &[&str]| {
        let mut args = vec![
            "--json",
            "profiles",
            "save",
            "--name",
            "Opt",
            "--version",
            "7.25.1",
        ];
        args.extend_from_slice(extra);
        args.into_iter().map(String::from).collect::<Vec<String>>()
    };
    let args = save(&[
        "--config",
        "empty",
        "--mode-script",
        &mode,
        "--remove-branding",
        "--license-key",
        &key,
        "--after",
        "shutdown",
    ]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, out, err) = env.run(&args).await;
    assert_eq!(code, 0, "{err}");
    let p: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(p["config"], serde_json::json!({"mode": "empty"}));
    assert_eq!(p["mode_script"], "mode.rsc");
    assert_eq!(p["remove_branding"], true);
    assert_eq!(
        p["license_key"],
        "-----BEGIN KEY-----AAAABBBB-----END KEY-----"
    );
    assert_eq!(p["after_install"], "shutdown");
    let (_, out, _) = env.run(&["profiles", "list"]).await;
    assert!(
        out.contains("empty config, mode script mode.rsc, remove branding, license key, shutdown"),
        "{out}"
    );
    for (extra, code_name) in [
        (
            vec!["--config", "keep", "--mode-script", mode.as_str()],
            "mode_script_with_keep",
        ),
        (
            vec!["--config", "empty", "--script", mode.as_str()],
            "script_with_config",
        ),
        (
            vec!["--license-key", "/nonexistent/k.key"],
            "license_key_failed",
        ),
    ] {
        let args = save(&extra);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, _, err) = env.run(&args).await;
        assert_eq!(code, 1, "{extra:?}");
        assert!(err.contains(code_name), "{extra:?}: {err}");
    }
}

```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ni-cli`
Expected: `profiles_take_the_configuration_options ... FAILED` (the flags are unknown: exit code 2), `test result: FAILED. 11 passed; 1 failed`

- [ ] **Step 3: Edit `crates/ni-cli/src/cli.rs` (with its unit tests)**

1. In `crates/ni-cli/src/cli.rs`, replace:

```rust
//! Command-line arguments.

use clap::{Args, Parser, Subcommand};
use ni_assets::VersionSpec;
use ni_proto::Mac;
```

with:

```rust
//! Command-line arguments.

use clap::{Args, Parser, Subcommand, ValueEnum};
use ni_assets::VersionSpec;
use ni_proto::Mac;
```

2. In `crates/ni-cli/src/cli.rs`, replace:

```rust
    #[arg(long = "package")]
    pub packages: Vec<String>,
    /// RouterOS script (.rsc) to run on first boot.
    #[arg(long)]
    pub script: Option<PathBuf>,
}

```

with:

```rust
    #[arg(long = "package")]
    pub packages: Vec<String>,
    #[command(flatten)]
    pub config: ConfigArgs,
}

/// The device configuration options; the official netinstall-cli's
/// `-r` / `-e` / `-s` / `-sm` / `-b` / `-k` / `--reboot` / `--shutdown`.
#[derive(Debug, Clone, Default, Args)]
pub struct ConfigArgs {
    /// The device's configuration after the install: keep the existing
    /// one, RouterOS's default configuration, or none at all [default:
    /// default; with --script: the script].
    #[arg(long, value_enum)]
    pub config: Option<ConfigArg>,
    /// RouterOS script (.rsc) to run on first boot (not with --config keep
    /// or empty).
    #[arg(long)]
    pub script: Option<PathBuf>,
    /// Advanced: the official tool's mode script (.rsc, sent as mode.scr;
    /// not with --config keep).
    #[arg(long, value_name = "FILE")]
    pub mode_script: Option<PathBuf>,
    /// Remove branding (the official tool's -b).
    #[arg(long)]
    pub remove_branding: bool,
    /// Software license key file (the official tool's -k).
    #[arg(long, value_name = "FILE")]
    pub license_key: Option<PathBuf>,
    /// When the install is done: reboot, reboot with the message
    /// "Installation successful", or shut down [default: reboot].
    #[arg(long, value_enum)]
    pub after: Option<AfterArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigArg {
    Keep,
    Default,
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AfterArg {
    Reboot,
    RebootMessage,
    Shutdown,
}

```

3. In `crates/ni-cli/src/cli.rs`, replace:

```rust
    pub net: NetArgs,
    /// Use this stored profile.
    #[arg(long, conflicts_with_all = ["version", "packages", "script"], required_unless_present = "version")]
    pub profile: Option<String>,
    /// Channel or version to install (instead of --profile).
```

with:

```rust
    pub net: NetArgs,
    /// Use this stored profile.
    #[arg(long, required_unless_present = "version", conflicts_with_all = [
        "version", "packages", "config", "script", "mode_script", "remove_branding",
        "license_key", "after",
    ])]
    pub profile: Option<String>,
    /// Channel or version to install (instead of --profile).
```

4. In `crates/ni-cli/src/cli.rs`, replace:

```rust
    #[arg(long = "package")]
    pub packages: Vec<String>,
    /// RouterOS script (.rsc) to run on first boot (with --version).
    #[arg(long)]
    pub script: Option<PathBuf>,
    /// Keep going: flash every device that appears, until Ctrl-C.
    #[arg(long)]
```

with:

```rust
    #[arg(long = "package")]
    pub packages: Vec<String>,
    /// Configuration options (with --version).
    #[command(flatten)]
    pub config: ConfigArgs,
    /// Keep going: flash every device that appears, until Ctrl-C.
    #[arg(long)]
```

5. In `crates/ni-cli/src/cli.rs`, replace:

```rust
        assert_eq!(f.version, Some("stable".parse().unwrap()));
        assert_eq!(f.packages, ["wifi-qcom", "container"]);
        assert_eq!(f.script, Some(PathBuf::from("setup.rsc")));
        assert!(f.auto_repeat);
        assert_eq!(f.count, Some(3));
```

with:

```rust
        assert_eq!(f.version, Some("stable".parse().unwrap()));
        assert_eq!(f.packages, ["wifi-qcom", "container"]);
        assert_eq!(f.config.script, Some(PathBuf::from("setup.rsc")));
        assert!(f.auto_repeat);
        assert_eq!(f.count, Some(3));
```

6. In `crates/ni-cli/src/cli.rs`, replace:

```rust
                Ipv4Addr::new(192, 168, 88, 20)
            ))
        );
    }
```

with:

```rust
                Ipv4Addr::new(192, 168, 88, 20)
            ))
        );
    }

    #[test]
    fn configuration_options_parse_and_conflict_with_a_profile() {
        let cli = parse(&[
            "flash",
            "--iface",
            "en7",
            "--version",
            "stable",
            "--config",
            "empty",
            "--mode-script",
            "m.rsc",
            "--remove-branding",
            "--license-key",
            "k.key",
            "--after",
            "reboot-message",
        ])
        .unwrap();
        let Command::Flash(f) = cli.command else {
            panic!("flash expected")
        };
        assert_eq!(f.config.config, Some(ConfigArg::Empty));
        assert_eq!(f.config.mode_script, Some(PathBuf::from("m.rsc")));
        assert!(f.config.remove_branding);
        assert_eq!(f.config.license_key, Some(PathBuf::from("k.key")));
        assert_eq!(f.config.after, Some(AfterArg::RebootMessage));
        for extra in [
            &["--config", "keep"][..],
            &["--remove-branding"],
            &["--after", "shutdown"],
            &["--license-key", "k.key"],
        ] {
            let mut args = vec!["flash", "--iface", "en7", "--profile", "p"];
            args.extend_from_slice(extra);
            assert_eq!(
                parse(&args).unwrap_err().kind(),
                ErrorKind::ArgumentConflict,
                "{extra:?}"
            );
        }
        assert_eq!(
            parse(&[
                "profiles",
                "save",
                "--name",
                "x",
                "--version",
                "7.25.1",
                "--config",
                "maybe"
            ])
            .unwrap_err()
            .kind(),
            ErrorKind::InvalidValue
        );
    }
```

- [ ] **Step 4: Edit `crates/ni-cli/src/commands.rs`**

1. In `crates/ni-cli/src/commands.rs`, replace:

```rust
//! Everything but `flash`: interfaces, preflight, assets, profiles.

use crate::cli::{exit, AssetsCommand, Cli, Command, NetArgs, ProfilesCommand, RecipeArgs};
use crate::output::finding;
use ni_assets::{default_cache_root, AssetEvent, Assets, HttpConfig, PkgArch, VersionSpec};
```

with:

```rust
//! Everything but `flash`: interfaces, preflight, assets, profiles.

use crate::cli::{
    exit, AfterArg, AssetsCommand, Cli, Command, ConfigArg, NetArgs, ProfilesCommand, RecipeArgs,
};
use crate::output::finding;
use ni_assets::{default_cache_root, AssetEvent, Assets, HttpConfig, PkgArch, VersionSpec};
```

2. In `crates/ni-cli/src/commands.rs`, replace:

```rust
use ni_engine::profile::spec_to_string;
use ni_engine::{
    default_profiles_root, AfterInstall, ConfigMode, EngineConfig, Failure, Profile, ProfileStore,
};
use ni_net::{list_interfaces, Interface};
```

with:

```rust
use ni_engine::profile::spec_to_string;
use ni_engine::{
    default_profiles_root, license_key_from_file, AfterInstall, ConfigMode, EngineConfig, Failure,
    Profile, ProfileStore,
};
use ni_net::{list_interfaces, Interface};
```

3. In `crates/ni-cli/src/commands.rs`, replace:

```rust
// ------------------------------------------------------------ profiles

/// A profile from command-line arguments. A script is registered in the
/// cache (as its lowercase file name) first.
pub async fn recipe_profile(
    assets: &Assets,
```

with:

```rust
// ------------------------------------------------------------ profiles

/// A profile from command-line arguments. Scripts are registered in the
/// cache (as their lowercase file names) first. Errors besides the profile
/// validation codes: `script_with_config` (`config`: `--script` with
/// `--config keep` or `empty`, which the official tool refuses too),
/// `mode_script_with_keep`, `script_failed`, `license_key_failed` (`path`,
/// `detail`: unreadable key file), `invalid_license_key`.
pub async fn recipe_profile(
    assets: &Assets,
```

4. In `crates/ni-cli/src/commands.rs`, replace:

```rust
    let mut p = Profile::new(name, recipe.version.clone());
    p.extra_packages = recipe.packages.clone();
    if let Some(script) = &recipe.script {
        let local = register_script(assets, script).await?;
        p.config = ConfigMode::Script { local_name: local };
    }
    p.validate()?;
    Ok(p)
```

with:

```rust
    let mut p = Profile::new(name, recipe.version.clone());
    p.extra_packages = recipe.packages.clone();
    let c = &recipe.config;
    p.config = match (c.config, &c.script) {
        (None | Some(ConfigArg::Default), None) => ConfigMode::Default,
        (Some(ConfigArg::Keep), None) => ConfigMode::Keep,
        (Some(ConfigArg::Empty), None) => ConfigMode::Empty,
        (None | Some(ConfigArg::Default), Some(script)) => ConfigMode::Script {
            local_name: register_script(assets, script).await?,
        },
        (Some(other), Some(_)) => {
            let config = format!("{other:?}").to_lowercase();
            return Err(Failure::new("script_with_config").with("config", config));
        }
    };
    if let Some(mode) = &c.mode_script {
        if p.config == ConfigMode::Keep {
            return Err(Failure::new("mode_script_with_keep"));
        }
        p.mode_script = Some(register_script(assets, mode).await?);
    }
    p.remove_branding = c.remove_branding;
    if let Some(path) = &c.license_key {
        let data = std::fs::read(path).map_err(|e| {
            Failure::new("license_key_failed")
                .with("path", path.display())
                .with("detail", e)
        })?;
        p.license_key = Some(license_key_from_file(&data)?);
    }
    p.after_install = match c.after {
        None | Some(AfterArg::Reboot) => AfterInstall::Reboot,
        Some(AfterArg::RebootMessage) => AfterInstall::RebootMessage,
        Some(AfterArg::Shutdown) => AfterInstall::Shutdown,
    };
    p.validate()?;
    Ok(p)
```

- [ ] **Step 5: Edit `crates/ni-cli/src/flash.rs`**

1. In `crates/ni-cli/src/flash.rs`, replace:

```rust
                version: version.clone(),
                packages: args.packages.clone(),
                script: args.script.clone(),
            };
            recipe_profile(engine.assets(), "command-line", &recipe).await?
```

with:

```rust
                version: version.clone(),
                packages: args.packages.clone(),
                config: args.config.clone(),
            };
            recipe_profile(engine.assets(), "command-line", &recipe).await?
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p ni-cli`
Expected: `test result: ok. 22 passed` (unit) and `test result: ok. 12 passed` (`tests/cli.rs`)

- [ ] **Step 7: Commit**

```bash
git add crates/ni-cli/src/cli.rs crates/ni-cli/src/commands.rs crates/ni-cli/src/flash.rs crates/ni-cli/tests/cli.rs
git commit -m "feat(cli): configuration options for profiles and flash (--config, --mode-script, --remove-branding, --license-key, --after)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: ni-bootp: requests shorter than 300 bytes are answered

**Files:**
- Modify: `crates/ni-bootp/src/responder.rs`

The official server ignores BOOTP requests under 300 bytes; ours already answers any request that parses (plus our DHCP options, a superset of its plain reply). This pins it with a test; no code change. The unit tests live in the module itself (`#[cfg(test)] mod tests`), so they are written together with the code; run them right after.

- [ ] **Step 1: Edit `crates/ni-bootp/src/responder.rs` (with its unit tests)**

1. In `crates/ni-bootp/src/responder.rs`, replace:

```rust
        assert_eq!(rep.arch, BootArch::Arm64);
        assert_eq!(&rep.bytes[240..242], &[opt::SUBNET_MASK, 4]);
    }

```

with:

```rust
        assert_eq!(rep.arch, BootArch::Arm64);
        assert_eq!(&rep.bytes[240..242], &[opt::SUBNET_MASK, 4]);
    }

    #[test]
    fn requests_shorter_than_300_bytes_are_answered() {
        // The official netinstall-cli ignores BOOTP requests under 300
        // bytes (RFC 951's size); we answer anything that parses.
        let mut r = responder();
        let mut buf = sample_request(MAC, None, Some("ARM64__boot"));
        let options = HEADER_LEN + 4;
        let end = options + buf[options..].iter().position(|&b| b == opt::END).unwrap();
        buf.truncate(end + 1);
        assert!(buf.len() < 300, "{} bytes", buf.len());
        let rep = reply(r.handle(&buf).unwrap());
        assert_eq!(rep.kind, ReplyKind::Bootp);
        assert!(rep.bytes.len() >= 300, "replies are padded to 300 bytes");
    }

```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p ni-bootp --lib short`
Expected: `test result: ok. 2 passed` (the new test and `malformed_packets_are_errors`' neighbour that matches `short`)

- [ ] **Step 3: Commit**

```bash
git add crates/ni-bootp/src/responder.rs
git commit -m "test(bootp): requests shorter than 300 bytes are answered" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: app: pick a license key file (read in Rust)

**Files:**
- Modify: `app/src-tauri/build.rs`
- Modify: `app/src-tauri/capabilities/default.json`
- Modify: `app/src-tauri/src/codes.rs`
- Modify: `app/src-tauri/src/commands.rs`
- Modify: `app/src-tauri/src/core.rs`
- Modify: `app/src-tauri/src/lib.rs`
- Modify: `app/src-tauri/src/views.rs`
- Modify: `app/src-tauri/tests/bridge.rs`
- Modify: `app/src/lib/codes.json`
- Modify: `app/src/locales/en.json`
- Modify: `app/src/locales/hu.json`

A new command `pick_license_key`: the Rust dialog API picks the file, `AppCore::read_license_key` reads it (at most 64 KiB, `key_file_failed` with `reason` `unreadable` / `too_large`) and returns only the key (`LicenseKeyView`); no path reaches the window. Registered like every command: `build.rs`, the capability, the invoke handler (the bundle test checks they agree).

- [ ] **Step 1: Update the tests in `app/src-tauri/tests/bridge.rs`**

1. In `app/src-tauri/tests/bridge.rs`, replace:

```rust
    );

    let view = rig.core.cache_inventory(true).await.unwrap();
    assert!(view.inventory.total > 0, "packages were cached");
```

with:

```rust
    );

    let key = rig.dir.join("device.key");
    std::fs::write(
        &key,
        b"-----BEGIN KEY-----\r\nAAAA\nBBBB==\n-----END KEY-----\n",
    )
    .unwrap();
    let view = rig.core.read_license_key(&key).unwrap();
    assert_eq!(view.key, "-----BEGIN KEY-----AAAABBBB==-----END KEY-----");
    let code = |p: &std::path::Path| rig.core.read_license_key(p).unwrap_err();
    let err = code(&rig.dir.join("missing.key"));
    assert_eq!(
        (err.code.as_str(), err.param("reason")),
        ("key_file_failed", Some("unreadable"))
    );
    assert!(err.param("path").is_none(), "no path for the window");
    std::fs::write(&key, vec![b'k'; 64 * 1024 + 1]).unwrap();
    assert_eq!(code(&key).param("reason"), Some("too_large"));
    std::fs::write(&key, b"\n\n").unwrap();
    assert_eq!(code(&key).code, "invalid_license_key");

    let view = rig.core.cache_inventory(true).await.unwrap();
    assert!(view.inventory.total > 0, "packages were cached");
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p mac-netinstall --test bridge`
Expected: `cargo test -p mac-netinstall --test bridge` does not compile: `no method named `read_license_key` found for struct `Arc<AppCore>``

- [ ] **Step 3: Edit `app/src-tauri/build.rs`**

1. In `app/src-tauri/build.rs`, replace:

```rust
    "available_packages",
    "pick_script",
    "get_settings",
    "set_settings",
```

with:

```rust
    "available_packages",
    "pick_script",
    "pick_license_key",
    "get_settings",
    "set_settings",
```

- [ ] **Step 4: Edit `app/src-tauri/capabilities/default.json`**

1. In `app/src-tauri/capabilities/default.json`, replace:

```json
    "allow-available-packages",
    "allow-pick-script",
    "allow-get-settings",
    "allow-set-settings",
```

with:

```json
    "allow-available-packages",
    "allow-pick-script",
    "allow-pick-license-key",
    "allow-get-settings",
    "allow-set-settings",
```

- [ ] **Step 5: Edit `app/src-tauri/src/codes.rs`**

1. In `app/src-tauri/src/codes.rs`, replace:

```rust
    "invalid_mac",
    "invalid_setting",
    "log_io",
    "no_home",
```

with:

```rust
    "invalid_mac",
    "invalid_setting",
    "key_file_failed",
    "log_io",
    "no_home",
```

- [ ] **Step 6: Edit `app/src-tauri/src/commands.rs`**

1. In `app/src-tauri/src/commands.rs`, replace:

```rust
use crate::settings::Settings;
use crate::views::{
    CacheView, InterfaceView, PackagesView, ProfilesView, ScriptView, StartRequest,
};
use ni_assets::DeleteScope;
```

with:

```rust
use crate::settings::Settings;
use crate::views::{
    CacheView, InterfaceView, LicenseKeyView, PackagesView, ProfilesView, ScriptView, StartRequest,
};
use ni_assets::DeleteScope;
```

2. In `app/src-tauri/src/commands.rs`, replace:

```rust
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_settings(core: State<'_, AppCore>) -> Settings {
```

with:

```rust
}

/// Let the user pick a MikroTik key file and read the license key from it
/// (`None`: cancelled). The path stays here; the key goes to the window,
/// which puts it in the profile.
#[tauri::command(rename_all = "snake_case")]
pub async fn pick_license_key(
    app: AppHandle,
    core: State<'_, AppCore>,
) -> R<Option<LicenseKeyView>> {
    let (tx, rx) = oneshot::channel();
    app.dialog().file().pick_file(move |file| {
        let _ = tx.send(file);
    });
    let Some(file) = rx.await.map_err(|_| Failure::new("dialog_failed"))? else {
        return Ok(None);
    };
    let path = file
        .into_path()
        .map_err(|e| Failure::new("dialog_failed").with("detail", e))?;
    core.read_license_key(&path).map(Some)
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_settings(core: State<'_, AppCore>) -> Settings {
```

- [ ] **Step 7: Edit `app/src-tauri/src/core.rs`**

1. In `app/src-tauri/src/core.rs`, replace:

```rust
use crate::settings::{Settings, SettingsStore};
use crate::views::{
    without_paths, BrokenProfile, CacheView, InterfaceView, PackagesView, ProfilesView, ScriptView,
    StartRequest,
};
use ni_assets::{
```

with:

```rust
use crate::settings::{Settings, SettingsStore};
use crate::views::{
    without_paths, BrokenProfile, CacheView, InterfaceView, LicenseKeyView, PackagesView,
    ProfilesView, ScriptView, StartRequest,
};
use ni_assets::{
```

2. In `app/src-tauri/src/core.rs`, replace:

```rust
};
use ni_engine::{
    default_profiles_root, preflight, EngineConfig, EngineHandle, Failure, PreflightReport,
    Profile, ProfileStore, Snapshot, StartInfo,
};
use ni_proto::Mac;
```

with:

```rust
};
use ni_engine::{
    default_profiles_root, license_key_from_file, preflight, EngineConfig, EngineHandle, Failure,
    PreflightReport, Profile, ProfileStore, Snapshot, StartInfo,
};
use ni_proto::Mac;
```

3. In `app/src-tauri/src/core.rs`, replace:

```rust
use std::time::Duration;
use tokio::sync::watch;

/// Where the app keeps its files.
```

with:

```rust
use std::time::Duration;
use tokio::sync::watch;

/// Largest key file read (MikroTik key files are a few hundred bytes).
pub const MAX_KEY_FILE: u64 = 64 * 1024;

/// Where the app keeps its files.
```

4. In `app/src-tauri/src/core.rs`, replace:

```rust
    }

    // ------------------------------------------------------------ cache, log

```

with:

```rust
    }

    /// The license key in the key file at `path` (the official tool's
    /// `-k`). Errors: `key_file_failed` (`reason`: `unreadable`, or
    /// `too_large` past [`MAX_KEY_FILE`] bytes), `invalid_license_key`
    /// (`max`).
    pub fn read_license_key(&self, path: &Path) -> Result<LicenseKeyView, Failure> {
        let failed = |reason: &str| Failure::new("key_file_failed").with("reason", reason);
        let len = std::fs::metadata(path)
            .map_err(|_| failed("unreadable"))?
            .len();
        if len > MAX_KEY_FILE {
            return Err(failed("too_large"));
        }
        let data = std::fs::read(path).map_err(|_| failed("unreadable"))?;
        let key = license_key_from_file(&data)?;
        Ok(LicenseKeyView { key })
    }

    // ------------------------------------------------------------ cache, log

```

- [ ] **Step 8: Edit `app/src-tauri/src/lib.rs`**

1. In `app/src-tauri/src/lib.rs`, replace:

```rust
            commands::available_packages,
            commands::pick_script,
            commands::get_settings,
            commands::set_settings,
```

with:

```rust
            commands::available_packages,
            commands::pick_script,
            commands::pick_license_key,
            commands::get_settings,
            commands::set_settings,
```

- [ ] **Step 9: Edit `app/src-tauri/src/views.rs`**

1. In `app/src-tauri/src/views.rs`, replace:

```rust
}

/// The download cache, for the settings view: what is in it, by
/// path-free ids ([`ni_assets::Inventory`]).
```

with:

```rust
}

/// A license key read from a key file (line breaks removed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseKeyView {
    pub key: String,
}

/// The download cache, for the settings view: what is in it, by
/// path-free ids ([`ni_assets::Inventory`]).
```

- [ ] **Step 10: Edit `app/src/locales/en.json`**

1. In `app/src/locales/en.json`, replace:

```json
  "code.invalid_mac": "“{mac}” is not a MAC address.",
  "code.invalid_setting": "Invalid setting: {field}.",
  "code.log_io": "Could not write the log file.",
  "code.no_home": "Your home folder could not be found.",
```

with:

```json
  "code.invalid_mac": "“{mac}” is not a MAC address.",
  "code.invalid_setting": "Invalid setting: {field}.",
  "code.key_file_failed": "The key file could not be used ({reason, select, too_large {it is too large} other {it cannot be read}}).",
  "code.log_io": "Could not write the log file.",
  "code.no_home": "Your home folder could not be found.",
```

- [ ] **Step 11: Edit `app/src/locales/hu.json`**

1. In `app/src/locales/hu.json`, replace:

```json
  "code.invalid_mac": "„{mac}” nem MAC-cím.",
  "code.invalid_setting": "Érvénytelen beállítás: {field}.",
  "code.log_io": "Nem sikerült menteni a naplót.",
  "code.no_home": "Nem található a felhasználói mappád.",
```

with:

```json
  "code.invalid_mac": "„{mac}” nem MAC-cím.",
  "code.invalid_setting": "Érvénytelen beállítás: {field}.",
  "code.key_file_failed": "A kulcsfájl nem használható ({reason, select, too_large {túl nagy} other {nem olvasható}}).",
  "code.log_io": "Nem sikerült menteni a naplót.",
  "code.no_home": "Nem található a felhasználói mappád.",
```

- [ ] **Step 12: Regenerate `app/src/lib/codes.json`**

```bash
UPDATE_CODES=1 cargo test -p mac-netinstall --test codes
```

Expected: `test result: ok. 3 passed`; `git diff --stat app/src/lib/codes.json` shows one added line in `app_failures` (`key_file_failed`).

- [ ] **Step 13: Run the tests to verify they pass**

Run: `cargo test -p mac-netinstall && (cd app && npx vitest run tests/i18n.test.ts)`
Expected: every suite `ok`; `tests/bridge.rs` `9 passed`, `tests/bundle.rs` `4 passed`

- [ ] **Step 14: Commit**

```bash
git add app/src-tauri/build.rs app/src-tauri/capabilities/default.json app/src-tauri/src/codes.rs app/src-tauri/src/commands.rs app/src-tauri/src/core.rs app/src-tauri/src/lib.rs app/src-tauri/src/views.rs app/src-tauri/tests/bridge.rs app/src/lib/codes.json app/src/locales/en.json app/src/locales/hu.json
git commit -m "feat(app): pick a license key file (read in Rust, no path to the window)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: UI: configuration, after-install and advanced options in the profile editor

**Files:**
- Modify: `app/src/components/ProfileEditor.svelte`
- Modify: `app/src/lib/api.ts`
- Modify: `app/src/lib/profile.ts`
- Modify: `app/src/lib/types.ts`
- Modify: `app/src/locales/en.json`
- Modify: `app/src/locales/hu.json`
- Modify: `app/tests/App.test.ts`
- Modify: `app/tests/ProfileEditor.test.ts`
- Modify: `app/tests/i18n.test.ts`
- Modify: `app/tests/mock.ts`

The editor's Configuration group offers the four modes with a help line each (Hungarian: "Meglévő konfiguráció megtartása", "Gyári alapkonfiguráció", "Üres konfiguráció", "Saját szkript (.rsc)"), an "After install" choice, and an Advanced group: "Mode script (advanced)" (disabled with keep; switching to keep with one set shows the error at once), "Remove branding", and the license key (picked through `pickLicenseKey`, shown as its length, removable). `profile.ts` maps the form to the new profile JSON; `tests/mock.ts` gets a `profile()` helper.

- [ ] **Step 1: Update the tests in `app/tests/App.test.ts`**

1. In `app/tests/App.test.ts`, replace:

```ts
import type { Backend } from "../src/lib/api";
import type { DeviceState, DeviceView, Profile, Snapshot } from "../src/lib/types";
import { mockBackend, SETTINGS, IDLE } from "./mock";

const A = "48:A9:8A:00:00:0A";
const B = "48:A9:8A:00:00:0B";
const LAB: Profile = { name: "lab", version: "stable", extra_packages: [], config: { mode: "default" } };
const OTHER: Profile = { ...LAB, name: "other" };
const FORMATTING: DeviceState = { state: "flashing", version: "7.25.1", phase: "formatting", sent: 0, total: 100 };
```

with:

```ts
import type { Backend } from "../src/lib/api";
import type { DeviceState, DeviceView, Profile, Snapshot } from "../src/lib/types";
import { mockBackend, profile, SETTINGS, IDLE } from "./mock";

const A = "48:A9:8A:00:00:0A";
const B = "48:A9:8A:00:00:0B";
const LAB: Profile = profile("lab");
const OTHER: Profile = { ...LAB, name: "other" };
const FORMATTING: DeviceState = { state: "flashing", version: "7.25.1", phase: "formatting", sent: 0, total: 100 };
```

2. In `app/tests/App.test.ts`, replace:

```ts
    vi.mocked(backend.listProfiles).mockResolvedValueOnce({
      profiles: [
        { name: "fresh", version: "stable", extra_packages: [], config: { mode: "default" } },
        { name: "lab", version: "stable", extra_packages: [], config: { mode: "default" } },
      ],
      broken: [],
```

with:

```ts
    vi.mocked(backend.listProfiles).mockResolvedValueOnce({
      profiles: [
        profile("fresh"),
        profile("lab"),
      ],
      broken: [],
```

- [ ] **Step 2: Update the tests in `app/tests/ProfileEditor.test.ts`**

1. In `app/tests/ProfileEditor.test.ts`, replace:

```ts
import { formErrors, fromForm, toForm, validName, validVersion } from "../src/lib/profile";
import type { Profile } from "../src/lib/types";
import { mockBackend } from "./mock";

beforeEach(() => locale.set("en"));
```

with:

```ts
import { formErrors, fromForm, toForm, validName, validVersion } from "../src/lib/profile";
import type { Profile } from "../src/lib/types";
import { mockBackend, profile } from "./mock";

beforeEach(() => locale.set("en"));
```

2. In `app/tests/ProfileEditor.test.ts`, replace:

```ts

  it("round-trips profiles through the form", () => {
    const p: Profile = {
      name: "lab",
      version: "7.25.1",
      extra_packages: ["wireless"],
      config: { mode: "custom_script", local_name: "setup.rsc" },
    };
    expect(fromForm(toForm(p))).toEqual(p);
    const f = toForm(null);
    f.useScript = true;
    expect(formErrors(f)).toEqual({ name: "profile.err_name", script: "profile.err_script" });
  });
});
```

with:

```ts

  it("round-trips profiles through the form", () => {
    const p: Profile = profile("lab", {
      version: "7.25.1",
      extra_packages: ["wireless"],
      config: { mode: "script", local_name: "setup.rsc" },
      mode_script: "mode.rsc",
      remove_branding: true,
      license_key: "KEY",
      after_install: "shutdown",
    });
    expect(fromForm(toForm(p))).toEqual(p);
    for (const mode of ["keep", "default", "empty"] as const) {
      const q = profile("x", { config: { mode } });
      expect(fromForm(toForm(q))).toEqual(q);
    }
    const f = toForm(null);
    f.config = "script";
    f.useModeScript = true;
    expect(formErrors(f)).toEqual({
      name: "profile.err_name",
      script: "profile.err_script",
      modeScript: "profile.err_mode_script",
    });
    f.config = "keep";
    expect(formErrors(f).modeScript).toBe("profile.err_mode_keep");
  });
});
```

3. In `app/tests/ProfileEditor.test.ts`, replace:

```ts
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).toHaveBeenCalledWith(
      {
        name: "hAP lab",
        version: "stable",
        extra_packages: ["wireless"],
        config: { mode: "custom_script", local_name: "setup.rsc" },
      },
      null,
    );
    expect(onclose).toHaveBeenCalledWith({ saved: "hAP lab" });
  });

```

with:

```ts
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).toHaveBeenCalledWith(
      profile("hAP lab", { extra_packages: ["wireless"], config: { mode: "script", local_name: "setup.rsc" } }),
      null,
    );
    expect(onclose).toHaveBeenCalledWith({ saved: "hAP lab" });
  });

  it("sets the configuration, what happens afterwards and the advanced options", async () => {
    const { backend } = open(null);
    await userEvent.type(screen.getByLabelText("Name"), "x86 lab");
    expect(screen.getByLabelText("RouterOS default configuration")).toBeChecked();
    await userEvent.click(screen.getByLabelText("Empty configuration"));
    await userEvent.selectOptions(screen.getByLabelText("After install"), "Shut down");
    await userEvent.click(screen.getByLabelText("Mode script (advanced)"));
    await userEvent.click(screen.getAllByRole("button", { name: "Choose…" }).at(-1)!);
    await userEvent.click(screen.getByLabelText("Remove branding"));
    await userEvent.click(screen.getByRole("button", { name: "Choose key file…" }));
    expect(await screen.findByText("key loaded (40 characters)")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).toHaveBeenCalledWith(
      profile("x86 lab", {
        config: { mode: "empty" },
        mode_script: "setup.rsc",
        remove_branding: true,
        license_key: "-----BEGIN KEY-----AAAA-----END KEY-----",
        after_install: "shutdown",
      }),
      null,
    );
  });

  it("keeps a mode script away from keeping the configuration", async () => {
    const { backend } = open(profile("lab", { config: { mode: "empty" }, mode_script: "m.rsc", license_key: "K" }));
    expect(screen.getByText("m.rsc")).toBeInTheDocument();
    await userEvent.click(screen.getByLabelText("Keep the existing configuration"));
    expect(screen.getByRole("alert")).toHaveTextContent(
      "A mode script cannot be used when keeping the existing configuration.",
    );
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).not.toHaveBeenCalled();
    await userEvent.click(screen.getByLabelText("Mode script (advanced)"));
    expect(screen.getByLabelText("Mode script (advanced)")).toBeDisabled();
    expect(screen.queryByRole("alert")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Remove" }));
    expect(screen.getByText("none")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(backend.saveProfile).toHaveBeenCalledWith(profile("lab", { config: { mode: "keep" } }), "lab");
  });

```

4. In `app/tests/ProfileEditor.test.ts`, replace:

```ts
      saveProfile: vi.fn(async () => Promise.reject({ code: "profile_exists", params: { name: "other" } })),
    });
    const { onclose } = open(
      { name: "lab", version: "7.24.6", extra_packages: ["gps"], config: { mode: "default" } },
      backend,
    );
    expect(screen.getByRole("heading")).toHaveTextContent("Edit profile");
    expect(screen.getByRole("textbox", { name: "Exact version" })).toHaveValue("7.24.6");
```

with:

```ts
      saveProfile: vi.fn(async () => Promise.reject({ code: "profile_exists", params: { name: "other" } })),
    });
    const { onclose } = open(profile("lab", { version: "7.24.6", extra_packages: ["gps"] }), backend);
    expect(screen.getByRole("heading")).toHaveTextContent("Edit profile");
    expect(screen.getByRole("textbox", { name: "Exact version" })).toHaveValue("7.24.6");
```

5. In `app/tests/ProfileEditor.test.ts`, replace:

```ts

  it("deletes after confirmation", async () => {
    const { backend, onclose } = open({ name: "lab", version: "stable", extra_packages: [], config: { mode: "default" } });
    await userEvent.click(screen.getByRole("button", { name: "Delete…" }));
    const dialogs = screen.getAllByRole("dialog");
```

with:

```ts

  it("deletes after confirmation", async () => {
    const { backend, onclose } = open(profile("lab"));
    await userEvent.click(screen.getByRole("button", { name: "Delete…" }));
    const dialogs = screen.getAllByRole("dialog");
```

6. In `app/tests/ProfileEditor.test.ts`, replace:

```ts

  it("asks before Escape discards changes", async () => {
    const { onclose } = open({ name: "lab", version: "stable", extra_packages: [], config: { mode: "default" } });
    await userEvent.type(screen.getByLabelText("Name"), "2");
    await userEvent.keyboard("{Escape}");
```

with:

```ts

  it("asks before Escape discards changes", async () => {
    const { onclose } = open(profile("lab"));
    await userEvent.type(screen.getByLabelText("Name"), "2");
    await userEvent.keyboard("{Escape}");
```

7. In `app/tests/ProfileEditor.test.ts`, replace:

```ts

  it("closes on Escape at once without changes", async () => {
    const { onclose } = open({ name: "lab", version: "stable", extra_packages: [], config: { mode: "default" } });
    await userEvent.keyboard("{Escape}");
    expect(onclose).toHaveBeenCalledWith({});
```

with:

```ts

  it("closes on Escape at once without changes", async () => {
    const { onclose } = open(profile("lab"));
    await userEvent.keyboard("{Escape}");
    expect(onclose).toHaveBeenCalledWith({});
```

- [ ] **Step 3: Update the tests in `app/tests/i18n.test.ts`**

1. In `app/tests/i18n.test.ts`, replace:

```ts
      expect(HU[key], key).toMatch(/előkészítés vagy telepítés/);
    }
  });

```

with:

```ts
      expect(HU[key], key).toMatch(/előkészítés vagy telepítés/);
    }
  });

  it("names the configuration modes in natural Hungarian", () => {
    expect(HU["profile.config_keep"]).toBe("Meglévő konfiguráció megtartása");
    expect(HU["profile.config_default"]).toBe("Gyári alapkonfiguráció");
    expect(HU["profile.config_empty"]).toBe("Üres konfiguráció");
    expect(HU["profile.config_script"]).toBe("Saját szkript (.rsc)");
    expect(HU["profile.mode_script"]).toBe("Mode script (haladó)");
  });

```

- [ ] **Step 4: Update the test helper `app/tests/mock.ts`**

1. In `app/tests/mock.ts`, replace:

```ts
import { vi } from "vitest";
import type { Backend } from "../src/lib/api";
import type { CacheView, EngineEvent, Settings, Snapshot } from "../src/lib/types";

export const SETTINGS: Settings = {
```

with:

```ts
import { vi } from "vitest";
import type { Backend } from "../src/lib/api";
import type { CacheView, EngineEvent, Profile, Settings, Snapshot } from "../src/lib/types";

export const SETTINGS: Settings = {
```

2. In `app/tests/mock.ts`, replace:

```ts
};

export interface MockBackend extends Backend {
  emit(e: EngineEvent): void;
```

with:

```ts
};

/** A stored profile with the default options. */
export function profile(name: string, more: Partial<Profile> = {}): Profile {
  return {
    name,
    version: "stable",
    extra_packages: [],
    config: { mode: "default" },
    mode_script: null,
    remove_branding: false,
    license_key: null,
    after_install: "reboot",
    ...more,
  };
}

export interface MockBackend extends Backend {
  emit(e: EngineEvent): void;
```

3. In `app/tests/mock.ts`, replace:

```ts
    snapshot: vi.fn(async () => IDLE),
    listProfiles: vi.fn(async () => ({
      profiles: [{ name: "lab", version: "stable", extra_packages: [], config: { mode: "default" as const } }],
      broken: [],
    })),
```

with:

```ts
    snapshot: vi.fn(async () => IDLE),
    listProfiles: vi.fn(async () => ({
      profiles: [profile("lab")],
      broken: [],
    })),
```

4. In `app/tests/mock.ts`, replace:

```ts
    })),
    pickScript: vi.fn(async () => ({ name: "setup.rsc", size: 30 })),
    getSettings: vi.fn(async () => ({ ...SETTINGS })),
    setSettings: vi.fn(async (s: Settings) => s),
```

with:

```ts
    })),
    pickScript: vi.fn(async () => ({ name: "setup.rsc", size: 30 })),
    pickLicenseKey: vi.fn(async () => ({ key: "-----BEGIN KEY-----AAAA-----END KEY-----" })),
    getSettings: vi.fn(async () => ({ ...SETTINGS })),
    setSettings: vi.fn(async (s: Settings) => s),
```

- [ ] **Step 5: Run it to verify it fails**

Run: `cd app && npx vitest run`
Expected: 5 failed tests: `names the configuration modes in natural Hungarian`, `round-trips profiles through the form`, `creates a profile with packages and a script`, `sets the configuration, …`, `keeps a mode script away from keeping the configuration`

- [ ] **Step 6: Edit `app/src/components/ProfileEditor.svelte`**

1. In `app/src/components/ProfileEditor.svelte`, replace:

```svelte
<script lang="ts">
  // Create or edit a profile: version (channel or exact), extra packages
  // from the published list, configuration (default or a picked script).
  import { untrack } from "svelte";
  import { toFailure } from "../lib/api";
```

with:

```svelte
<script lang="ts">
  // Create or edit a profile: version (channel or exact), extra packages
  // from the published list, configuration (keep, default, empty or a
  // picked script), what to do afterwards, and the advanced options of
  // the official tool (mode script, remove branding, license key).
  import { untrack } from "svelte";
  import { toFailure } from "../lib/api";
```

2. In `app/src/components/ProfileEditor.svelte`, replace:

```svelte
  import { locale, t } from "../lib/i18n";
  import { describe, hint } from "../lib/messages";
  import { ARCHES, formErrors, fromForm, toForm } from "../lib/profile";
  import { CHANNELS, type Failure, type Profile } from "../lib/types";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import Modal from "./Modal.svelte";
```

with:

```svelte
  import { locale, t } from "../lib/i18n";
  import { describe, hint } from "../lib/messages";
  import { ARCHES, CONFIG_CHOICES, formErrors, fromForm, toForm } from "../lib/profile";
  import { AFTER_INSTALL, CHANNELS, type Failure, type Profile } from "../lib/types";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import Modal from "./Modal.svelte";
```

3. In `app/src/components/ProfileEditor.svelte`, replace:

```svelte
  }

  async function pick() {
    try {
      const s = await backend.pickScript();
      if (s) form.script = s.name;
    } catch (e) {
      saveError = toFailure(e);
```

with:

```svelte
  }

  async function pick(field: "script" | "modeScript") {
    try {
      const s = await backend.pickScript();
      if (s) form[field] = s.name;
    } catch (e) {
      saveError = toFailure(e);
    }
  }

  async function pickKey() {
    try {
      const k = await backend.pickLicenseKey();
      if (k) form.licenseKey = k.key;
    } catch (e) {
      saveError = toFailure(e);
```

4. In `app/src/components/ProfileEditor.svelte`, replace:

```svelte
</script>

<Modal title={initial ? $t("profile.edit_title") : $t("profile.new_title")} onclose={requestClose} width={520}>
  <form class="grid" onsubmit={(e) => { e.preventDefault(); save(); }}>
    <label for="p-name">{$t("profile.name")}</label>
```

with:

```svelte
</script>

<Modal title={initial ? $t("profile.edit_title") : $t("profile.new_title")} onclose={requestClose} width={560}>
  <form class="grid" onsubmit={(e) => { e.preventDefault(); save(); }}>
    <label for="p-name">{$t("profile.name")}</label>
```

5. In `app/src/components/ProfileEditor.svelte`, replace:

```svelte
    <span class="label">{$t("profile.config")}</span>
    <div class="stack">
      <label class="row">
        <input type="radio" name="cmode" checked={!form.useScript} onchange={() => (form.useScript = false)} />
        {$t("profile.config_default")}
      </label>
      <label class="row">
        <input type="radio" name="cmode" checked={form.useScript} onchange={() => (form.useScript = true)} />
        {$t("profile.config_script")}
      </label>
      {#if form.useScript}
        <div class="row indent">
          <button type="button" class="small" onclick={pick}>{$t("profile.choose")}</button>
          <span class={form.script ? "mono" : "secondary"}>{form.script ?? $t("profile.no_script")}</span>
        </div>
        {#if tried && errors.script}<div class="field-error" role="alert">{$t(errors.script)}</div>{/if}
      {/if}
    </div>
  </form>
```

with:

```svelte
    <span class="label">{$t("profile.config")}</span>
    <div class="stack">
      {#each CONFIG_CHOICES as c (c)}
        <label class="row">
          <input type="radio" name="cmode" checked={form.config === c} onchange={() => (form.config = c)} />
          {$t(`profile.config_${c}`)}
        </label>
        <div class="help indent">{$t(`profile.config_${c}_help`)}</div>
        {#if c === "script" && form.config === "script"}
          <div class="row indent">
            <button type="button" class="small" onclick={() => pick("script")}>{$t("profile.choose")}</button>
            <span class={form.script ? "mono" : "secondary"}>{form.script ?? $t("profile.no_script")}</span>
          </div>
          {#if tried && errors.script}<div class="field-error" role="alert">{$t(errors.script)}</div>{/if}
        {/if}
      {/each}
    </div>

    <label for="p-after">{$t("profile.after_install")}</label>
    <div>
      <select id="p-after" bind:value={form.afterInstall}>
        {#each AFTER_INSTALL as a (a)}<option value={a}>{$t(`profile.after_${a}`)}</option>{/each}
      </select>
    </div>

    <span class="label">{$t("profile.advanced")}</span>
    <div class="stack">
      <label class="row">
        <input type="checkbox" bind:checked={form.useModeScript} disabled={form.config === "keep" && !form.useModeScript} />
        {$t("profile.mode_script")}
      </label>
      <div class="help indent">{$t("profile.mode_script_help")}</div>
      {#if form.useModeScript}
        <div class="row indent">
          <button type="button" class="small" onclick={() => pick("modeScript")}>{$t("profile.choose")}</button>
          <span class={form.modeScript ? "mono" : "secondary"}>{form.modeScript ?? $t("profile.no_script")}</span>
        </div>
      {/if}
      {#if errors.modeScript && (tried || errors.modeScript === "profile.err_mode_keep")}
        <div class="field-error" role="alert">{$t(errors.modeScript)}</div>
      {/if}
      <label class="row">
        <input type="checkbox" bind:checked={form.removeBranding} />
        {$t("profile.remove_branding")}
      </label>
      <div class="help indent">{$t("profile.remove_branding_help")}</div>
      <div class="row">
        {$t("profile.license_key")}
        <button type="button" class="small" onclick={pickKey}>{$t("profile.license_key_choose")}</button>
        {#if form.licenseKey}
          <span>{$t("profile.license_key_set", { count: form.licenseKey.length })}</span>
          <button type="button" class="small" onclick={() => (form.licenseKey = null)}>{$t("profile.license_key_remove")}</button>
        {:else}
          <span class="secondary">{$t("profile.license_key_none")}</span>
        {/if}
      </div>
      <div class="help">{$t("profile.license_key_help")}</div>
    </div>
  </form>
```

6. In `app/src/components/ProfileEditor.svelte`, replace:

```svelte
    padding-left: 22px;
  }
  .packages {
    display: grid;
```

with:

```svelte
    padding-left: 22px;
  }
  .help {
    margin-top: -3px;
    font-size: 11px;
    color: var(--secondary);
    white-space: normal;
  }
  .packages {
    display: grid;
```

- [ ] **Step 7: Edit `app/src/lib/api.ts`**

1. In `app/src/lib/api.ts`, replace:

```ts
  Failure,
  InterfaceView,
  PackagesView,
  PreflightReport,
```

with:

```ts
  Failure,
  InterfaceView,
  LicenseKeyView,
  PackagesView,
  PreflightReport,
```

2. In `app/src/lib/api.ts`, replace:

```ts
  /** Opens a file dialog (in Rust); `null` if cancelled. */
  pickScript(): Promise<ScriptView | null>;
  getSettings(): Promise<Settings>;
  setSettings(settings: Settings): Promise<Settings>;
```

with:

```ts
  /** Opens a file dialog (in Rust); `null` if cancelled. */
  pickScript(): Promise<ScriptView | null>;
  /** Opens a file dialog and reads the key file (in Rust); `null` if cancelled. */
  pickLicenseKey(): Promise<LicenseKeyView | null>;
  getSettings(): Promise<Settings>;
  setSettings(settings: Settings): Promise<Settings>;
```

3. In `app/src/lib/api.ts`, replace:

```ts
    availablePackages: (version, arch) => invoke("available_packages", { version, arch }),
    pickScript: () => invoke("pick_script"),
    getSettings: () => invoke("get_settings"),
    setSettings: (settings) => invoke("set_settings", { settings }),
```

with:

```ts
    availablePackages: (version, arch) => invoke("available_packages", { version, arch }),
    pickScript: () => invoke("pick_script"),
    pickLicenseKey: () => invoke("pick_license_key"),
    getSettings: () => invoke("get_settings"),
    setSettings: (settings) => invoke("set_settings", { settings }),
```

- [ ] **Step 8: Edit `app/src/lib/profile.ts`**

1. In `app/src/lib/profile.ts`, replace:

```ts
// Profile form rules, mirroring ni-engine's validation so most mistakes
// are shown before saving (the backend checks again).
import { CHANNELS, type Profile } from "./types";

/** Architectures RouterOS packages are published for. */
export const ARCHES = ["arm64", "arm", "mipsbe", "mmips", "smips", "ppc", "tile"] as const;

const NAME = /^[A-Za-z0-9 ._+-]{1,64}$/;
```

with:

```ts
// Profile form rules, mirroring ni-engine's validation so most mistakes
// are shown before saving (the backend checks again).
import { CHANNELS, type AfterInstall, type Profile } from "./types";

/** Architectures RouterOS packages are published for. */
export const ARCHES = ["arm64", "arm", "mipsbe", "mmips", "smips", "ppc", "tile"] as const;

/** The configuration choices, in the order the editor shows them. */
export const CONFIG_CHOICES = ["keep", "default", "empty", "script"] as const;
export type ConfigChoice = (typeof CONFIG_CHOICES)[number];

const NAME = /^[A-Za-z0-9 ._+-]{1,64}$/;
```

2. In `app/src/lib/profile.ts`, replace:

```ts
  exact: string;
  packages: string[];
  useScript: boolean;
  script: string | null;
}

export function toForm(p: Profile | null): ProfileForm {
  if (!p) {
    return { name: "", useChannel: true, channel: "stable", exact: "", packages: [], useScript: false, script: null };
  }
  const channel = isChannel(p.version);
```

with:

```ts
  exact: string;
  packages: string[];
  config: ConfigChoice;
  /** The script for `config === "script"`. */
  script: string | null;
  useModeScript: boolean;
  modeScript: string | null;
  removeBranding: boolean;
  licenseKey: string | null;
  afterInstall: AfterInstall;
}

export function toForm(p: Profile | null): ProfileForm {
  if (!p) {
    return {
      name: "",
      useChannel: true,
      channel: "stable",
      exact: "",
      packages: [],
      config: "default",
      script: null,
      useModeScript: false,
      modeScript: null,
      removeBranding: false,
      licenseKey: null,
      afterInstall: "reboot",
    };
  }
  const channel = isChannel(p.version);
```

3. In `app/src/lib/profile.ts`, replace:

```ts
    exact: channel ? "" : p.version,
    packages: [...p.extra_packages],
    useScript: p.config.mode === "custom_script",
    script: p.config.mode === "custom_script" ? p.config.local_name : null,
  };
}

/** Message keys of the form's problems, by field. */
export function formErrors(f: ProfileForm): Partial<Record<"name" | "version" | "script", string>> {
  const out: Partial<Record<"name" | "version" | "script", string>> = {};
  if (!validName(f.name)) out.name = "profile.err_name";
  if (!f.useChannel && !validVersion(f.exact.trim())) out.version = "profile.err_version";
  if (f.useScript && !f.script) out.script = "profile.err_script";
  return out;
}

export function fromForm(f: ProfileForm): Profile {
  return {
    name: f.name,
    version: f.useChannel ? f.channel : f.exact.trim(),
    extra_packages: [...f.packages],
    config: f.useScript && f.script ? { mode: "custom_script", local_name: f.script } : { mode: "default" },
  };
}
```

with:

```ts
    exact: channel ? "" : p.version,
    packages: [...p.extra_packages],
    config: p.config.mode,
    script: p.config.mode === "script" ? p.config.local_name : null,
    useModeScript: p.mode_script !== null,
    modeScript: p.mode_script,
    removeBranding: p.remove_branding,
    licenseKey: p.license_key,
    afterInstall: p.after_install,
  };
}

type Field = "name" | "version" | "script" | "modeScript";

/** Message keys of the form's problems, by field. */
export function formErrors(f: ProfileForm): Partial<Record<Field, string>> {
  const out: Partial<Record<Field, string>> = {};
  if (!validName(f.name)) out.name = "profile.err_name";
  if (!f.useChannel && !validVersion(f.exact.trim())) out.version = "profile.err_version";
  if (f.config === "script" && !f.script) out.script = "profile.err_script";
  if (f.useModeScript && f.config === "keep") out.modeScript = "profile.err_mode_keep";
  else if (f.useModeScript && !f.modeScript) out.modeScript = "profile.err_mode_script";
  return out;
}

export function fromForm(f: ProfileForm): Profile {
  const config: Profile["config"] =
    f.config === "script" ? { mode: "script", local_name: f.script ?? "" } : { mode: f.config };
  return {
    name: f.name,
    version: f.useChannel ? f.channel : f.exact.trim(),
    extra_packages: [...f.packages],
    config,
    mode_script: f.useModeScript ? f.modeScript : null,
    remove_branding: f.removeBranding,
    license_key: f.licenseKey,
    after_install: f.afterInstall,
  };
}
```

- [ ] **Step 9: Edit `app/src/lib/types.ts`**

1. In `app/src/lib/types.ts`, replace:

```ts
}

export type ConfigMode = { mode: "default" } | { mode: "custom_script"; local_name: string };

export interface Profile {
```

with:

```ts
}

/** The device's configuration after the install (`ni_engine::ConfigMode`). */
export type ConfigMode =
  | { mode: "keep" }
  | { mode: "default" }
  | { mode: "empty" }
  | { mode: "script"; local_name: string };

export const AFTER_INSTALL = ["reboot", "reboot_message", "shutdown"] as const;
export type AfterInstall = (typeof AFTER_INSTALL)[number];

export interface Profile {
```

2. In `app/src/lib/types.ts`, replace:

```ts
  extra_packages: string[];
  config: ConfigMode;
}

```

with:

```ts
  extra_packages: string[];
  config: ConfigMode;
  /** Registered `.rsc` sent as `mode.scr` (not with `keep`). */
  mode_script: string | null;
  remove_branding: boolean;
  /** Key file contents without line breaks. */
  license_key: string | null;
  after_install: AfterInstall;
}

```

3. In `app/src/lib/types.ts`, replace:

```ts
  name: string;
  size: number;
}

```

with:

```ts
  name: string;
  size: number;
}

export interface LicenseKeyView {
  key: string;
}

```

- [ ] **Step 10: Edit `app/src/locales/en.json`**

1. In `app/src/locales/en.json`, replace:

```json
  "profile.packages_none": "No extra packages are published for this version.",
  "profile.config": "Configuration",
  "profile.config_default": "RouterOS default configuration",
  "profile.config_script": "Run my script (.rsc)",
  "profile.choose": "Choose…",
  "profile.no_script": "No script chosen",
  "profile.save": "Save",
  "profile.cancel": "Cancel",
```

with:

```json
  "profile.packages_none": "No extra packages are published for this version.",
  "profile.config": "Configuration",
  "profile.config_keep": "Keep the existing configuration",
  "profile.config_keep_help": "The device keeps its current settings (what the official tool does by default).",
  "profile.config_default": "RouterOS default configuration",
  "profile.config_default_help": "RouterOS applies its factory default configuration on first boot.",
  "profile.config_empty": "Empty configuration",
  "profile.config_empty_help": "No configuration at all: reach the device through its MAC address (WinBox) or a console.",
  "profile.config_script": "Run my script (.rsc)",
  "profile.config_script_help": "Your RouterOS script runs on first boot instead of the default configuration.",
  "profile.choose": "Choose…",
  "profile.no_script": "No script chosen",
  "profile.after_install": "After install",
  "profile.after_reboot": "Reboot",
  "profile.after_reboot_message": "Reboot with the message “Installation successful”",
  "profile.after_shutdown": "Shut down",
  "profile.advanced": "Advanced",
  "profile.mode_script": "Mode script (advanced)",
  "profile.mode_script_help": "An extra RouterOS script sent as mode.scr, like the official tool's -sm. Not together with keeping the existing configuration.",
  "profile.remove_branding": "Remove branding",
  "profile.remove_branding_help": "Like the official tool's -b: asks the device to drop a branding package's customisations.",
  "profile.license_key": "License key:",
  "profile.license_key_choose": "Choose key file…",
  "profile.license_key_set": "key loaded ({count} characters)",
  "profile.license_key_none": "none",
  "profile.license_key_remove": "Remove",
  "profile.license_key_help": "Only for devices that need a software key (e.g. x86), like the official tool's -k.",
  "profile.save": "Save",
  "profile.cancel": "Cancel",
```

2. In `app/src/locales/en.json`, replace:

```json
  "profile.err_version": "Enter a version such as 7.25.1 or 7.26rc1.",
  "profile.err_script": "Choose a script file.",

  "channel.stable": "Stable",
```

with:

```json
  "profile.err_version": "Enter a version such as 7.25.1 or 7.26rc1.",
  "profile.err_script": "Choose a script file.",
  "profile.err_mode_script": "Choose a mode script file.",
  "profile.err_mode_keep": "A mode script cannot be used when keeping the existing configuration.",

  "channel.stable": "Stable",
```

- [ ] **Step 11: Edit `app/src/locales/hu.json`**

1. In `app/src/locales/hu.json`, replace:

```json
  "profile.packages_none": "Ehhez a verzióhoz nincs további csomag.",
  "profile.config": "Konfiguráció",
  "profile.config_default": "A RouterOS alapértelmezett konfigurációja",
  "profile.config_script": "Saját szkript futtatása (.rsc)",
  "profile.choose": "Kiválasztás…",
  "profile.no_script": "Nincs kiválasztott szkript",
  "profile.save": "Mentés",
  "profile.cancel": "Mégse",
```

with:

```json
  "profile.packages_none": "Ehhez a verzióhoz nincs további csomag.",
  "profile.config": "Konfiguráció",
  "profile.config_keep": "Meglévő konfiguráció megtartása",
  "profile.config_keep_help": "Az eszköz megtartja a jelenlegi beállításait (a hivatalos eszköz alapértelmezése).",
  "profile.config_default": "Gyári alapkonfiguráció",
  "profile.config_default_help": "Az első indításkor a RouterOS gyári alapkonfigurációja lép életbe.",
  "profile.config_empty": "Üres konfiguráció",
  "profile.config_empty_help": "Semmilyen konfiguráció nem lesz: az eszköz MAC-címen (WinBox) vagy konzolon érhető el.",
  "profile.config_script": "Saját szkript (.rsc)",
  "profile.config_script_help": "Az első indításkor a saját RouterOS-szkripted fut le a gyári alapkonfiguráció helyett.",
  "profile.choose": "Kiválasztás…",
  "profile.no_script": "Nincs kiválasztott szkript",
  "profile.after_install": "Telepítés után",
  "profile.after_reboot": "Újraindítás",
  "profile.after_reboot_message": "Újraindítás „Installation successful” üzenettel",
  "profile.after_shutdown": "Leállítás",
  "profile.advanced": "Haladó",
  "profile.mode_script": "Mode script (haladó)",
  "profile.mode_script_help": "A hivatalos eszköz -sm kapcsolójának megfelelő, mode.scr néven küldött kiegészítő RouterOS-szkript. A meglévő konfiguráció megtartásával nem használható.",
  "profile.remove_branding": "Márkajelzés (branding) eltávolítása",
  "profile.remove_branding_help": "A hivatalos eszköz -b kapcsolójának megfelelően kéri az eszközt, hogy hagyja el a branding csomag testreszabásait.",
  "profile.license_key": "Licenckulcs:",
  "profile.license_key_choose": "Kulcsfájl kiválasztása…",
  "profile.license_key_set": "kulcs betöltve ({count} karakter)",
  "profile.license_key_none": "nincs",
  "profile.license_key_remove": "Eltávolítás",
  "profile.license_key_help": "Csak szoftverkulcsot igénylő eszközökhöz (pl. x86), a hivatalos eszköz -k kapcsolójának megfelelően.",
  "profile.save": "Mentés",
  "profile.cancel": "Mégse",
```

2. In `app/src/locales/hu.json`, replace:

```json
  "profile.err_version": "Adj meg egy verziót, például 7.25.1 vagy 7.26rc1.",
  "profile.err_script": "Válassz egy szkriptfájlt.",

  "channel.stable": "Stabil",
```

with:

```json
  "profile.err_version": "Adj meg egy verziót, például 7.25.1 vagy 7.26rc1.",
  "profile.err_script": "Válassz egy szkriptfájlt.",
  "profile.err_mode_script": "Válassz egy mode script fájlt.",
  "profile.err_mode_keep": "Mode script nem használható a meglévő konfiguráció megtartásával.",

  "channel.stable": "Stabil",
```

- [ ] **Step 12: Run the tests to verify they pass**

Run: `cd app && npm run check && npx vitest run`
Expected: svelte-check `0 ERRORS 0 WARNINGS`; Vitest `Tests  126 passed (126)`

- [ ] **Step 13: Commit**

```bash
git add app/src/components/ProfileEditor.svelte app/src/lib/api.ts app/src/lib/profile.ts app/src/lib/types.ts app/src/locales/en.json app/src/locales/hu.json app/tests/App.test.ts app/tests/ProfileEditor.test.ts app/tests/i18n.test.ts app/tests/mock.ts
git commit -m "feat(ui): configuration modes, after-install action and advanced options in the profile editor" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: Show each device's configuration mode

**Files:**
- Modify: `app/src/components/DeviceTable.svelte`
- Modify: `app/src/lib/state.svelte.ts`
- Modify: `app/src/lib/types.ts`
- Modify: `app/src/locales/en.json`
- Modify: `app/src/locales/hu.json`
- Modify: `app/tests/DeviceTable.test.ts`
- Modify: `app/tests/state.test.ts`
- Modify: `crates/ni-cli/src/output.rs`
- Modify: `crates/ni-engine/src/codes.rs`
- Modify: `crates/ni-engine/src/engine/core.rs`
- Modify: `crates/ni-engine/src/event.rs`
- Modify: `crates/ni-engine/tests/assets.rs`
- Modify: `crates/ni-engine/tests/flash.rs`

The engine's `preparing` state now carries `config` (the profile's mode), so the window learns it from the event stream as well as from snapshots; the device table shows it under the version ("empty configuration" / "üres konfiguráció"). `ni-cli`'s patterns ignore the new field.

- [ ] **Step 1: Update the tests in `app/tests/DeviceTable.test.ts`**

1. In `app/tests/DeviceTable.test.ts`, replace:

```ts
    info: { model: "RB5009UG+S+", arch: "arm64", license_id: "X", min_os: "7" },
    version: "7.25.1",
    attempt: 1,
    updated_ms: Date.now(),
```

with:

```ts
    info: { model: "RB5009UG+S+", arch: "arm64", license_id: "X", min_os: "7" },
    version: "7.25.1",
    config: null,
    attempt: 1,
    updated_ms: Date.now(),
```

2. In `app/tests/DeviceTable.test.ts`, replace:

```ts
    expect(within(row()).getByText(text)).toBeInTheDocument();
    expect(within(row()).getByText("RB5009UG+S+")).toBeInTheDocument();
  });

```

with:

```ts
    expect(within(row()).getByText(text)).toBeInTheDocument();
    expect(within(row()).getByText("RB5009UG+S+")).toBeInTheDocument();
  });

  it("shows the configuration mode under the version", async () => {
    show([dev({ state: "done" }, { config: "empty" })]);
    expect(within(row()).getByText("empty configuration")).toBeInTheDocument();
    locale.set("hu");
    expect(await within(row()).findByText("üres konfiguráció")).toBeInTheDocument();
  });

```

- [ ] **Step 2: Update the tests in `app/tests/state.test.ts`**

1. In `app/tests/state.test.ts`, replace:

```ts
    s.apply({ event: "device", mac: A, state: "bootp_seen", boot_arch: "arm64", ip: "192.168.88.2" }, 10);
    s.apply({ event: "discovered", mac: A, model: "RB5009", arch: "arm64", license_id: "X", min_os: "7" });
    s.apply({ event: "device", mac: A, state: "preparing", version: "7.25.1" });
    s.apply({ event: "device", mac: A, state: "flashing", version: "7.25.1", phase: "uploading", sent: 50, total: 100 }, 20);
    const d = s.devices[A]!;
```

with:

```ts
    s.apply({ event: "device", mac: A, state: "bootp_seen", boot_arch: "arm64", ip: "192.168.88.2" }, 10);
    s.apply({ event: "discovered", mac: A, model: "RB5009", arch: "arm64", license_id: "X", min_os: "7" });
    s.apply({ event: "device", mac: A, state: "preparing", version: "7.25.1", config: "script" });
    s.apply({ event: "device", mac: A, state: "flashing", version: "7.25.1", phase: "uploading", sent: 50, total: 100 }, 20);
    const d = s.devices[A]!;
```

2. In `app/tests/state.test.ts`, replace:

```ts
    expect(d.info?.model).toBe("RB5009");
    expect(d.version).toBe("7.25.1");
    expect(d.attempt).toBe(1);
    expect(d.updated_ms).toBe(20);
```

with:

```ts
    expect(d.info?.model).toBe("RB5009");
    expect(d.version).toBe("7.25.1");
    expect(d.config).toBe("script");
    expect(d.attempt).toBe(1);
    expect(d.updated_ms).toBe(20);
```

- [ ] **Step 3: Update the tests in `crates/ni-engine/tests/assets.rs`**

1. In `crates/ni-engine/tests/assets.rs`, replace:

```rust
            EngineEvent::Device {
                mac,
                state: DeviceState::Preparing { version },
            } if *mac == B => Some(version.clone()),
            _ => None,
```

with:

```rust
            EngineEvent::Device {
                mac,
                state: DeviceState::Preparing { version, .. },
            } if *mac == B => Some(version.clone()),
            _ => None,
```

- [ ] **Step 4: Update the tests in `crates/ni-engine/tests/flash.rs`**

1. In `crates/ni-engine/tests/flash.rs`, replace:

```rust
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
    let report = rig.lan.wait_terminated(A, WAIT).await.unwrap();
```

with:

```rust
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    let e = state(&mut rig.events, A, "preparing").await;
    let EngineEvent::Device {
        state: DeviceState::Preparing { config, .. },
        ..
    } = e
    else {
        unreachable!()
    };
    assert_eq!(config.as_deref(), Some("keep"));
    state(&mut rig.events, A, "done").await;
    let report = rig.lan.wait_terminated(A, WAIT).await.unwrap();
```

- [ ] **Step 5: Run it to verify it fails**

Run: `cargo test -p ni-engine --test flash`
Expected: `cargo test -p ni-engine --test flash` does not compile: `variant `ni_engine::DeviceState::Preparing` does not have a field named `config``

- [ ] **Step 6: Edit `app/src/components/DeviceTable.svelte`**

1. In `app/src/components/DeviceTable.svelte`, replace:

```svelte
            {/if}
          </td>
          <td>{d.version ?? ""}</td>
          <td class="actions-col">
            {#if d.state.state === "waiting" && d.state.reason === "no_profile"}
```

with:

```svelte
            {/if}
          </td>
          <td>
            {d.version ?? ""}
            {#if d.config}<div class="config secondary">{$t(`devices.config_${d.config}`)}</div>{/if}
          </td>
          <td class="actions-col">
            {#if d.state.state === "waiting" && d.state.reason === "no_profile"}
```

2. In `app/src/components/DeviceTable.svelte`, replace:

```svelte
    font-size: 12px;
  }
  .state-failed {
    color: var(--danger);
```

with:

```svelte
    font-size: 12px;
  }
  .config {
    font-size: 11px;
  }
  .state-failed {
    color: var(--danger);
```

- [ ] **Step 7: Edit `app/src/lib/state.svelte.ts`**

1. In `app/src/lib/state.svelte.ts`, replace:

```ts
  info: DeviceInfoView | null;
  version: string | null;
  attempt: number;
  updated_ms: number;
```

with:

```ts
  info: DeviceInfoView | null;
  version: string | null;
  /** The configuration mode of its profile (keep, default, empty, script). */
  config: string | null;
  attempt: number;
  updated_ms: number;
```

2. In `app/src/lib/state.svelte.ts`, replace:

```ts
    info: d.info,
    version: d.version,
    attempt: d.attempt,
    updated_ms: d.updated_ms,
```

with:

```ts
    info: d.info,
    version: d.version,
    config: d.config,
    attempt: d.attempt,
    updated_ms: d.updated_ms,
```

3. In `app/src/lib/state.svelte.ts`, replace:

```ts
        info: null,
        version: null,
        attempt: 0,
        updated_ms: Date.now(),
```

with:

```ts
        info: null,
        version: null,
        config: null,
        attempt: 0,
        updated_ms: Date.now(),
```

4. In `app/src/lib/state.svelte.ts`, replace:

```ts
          d.version = s.version ?? d.version;
        }
        d.state = s;
        d.updated_ms = now;
```

with:

```ts
          d.version = s.version ?? d.version;
        }
        if (s.state === "preparing" && s.config) d.config = s.config;
        d.state = s;
        d.updated_ms = now;
```

- [ ] **Step 8: Edit `app/src/lib/types.ts`**

1. In `app/src/lib/types.ts`, replace:

```ts
  | { state: "discovered" }
  | { state: "waiting"; reason: WaitReason }
  | { state: "preparing"; version?: string }
  | { state: "flashing"; version: string; phase: string; sent: number; total: number }
  | { state: "done" }
```

with:

```ts
  | { state: "discovered" }
  | { state: "waiting"; reason: WaitReason }
  | { state: "preparing"; version?: string; config?: string }
  | { state: "flashing"; version: string; phase: string; sent: number; total: number }
  | { state: "done" }
```

- [ ] **Step 9: Edit `app/src/locales/en.json`**

1. In `app/src/locales/en.json`, replace:

```json
  "devices.stale": "Last known state; the server is stopped.",
  "devices.forget_cooldown": "Just flashed and restarting: it can be forgotten once its cooldown is over.",

  "action.flash": "Flash",
```

with:

```json
  "devices.stale": "Last known state; the server is stopped.",
  "devices.forget_cooldown": "Just flashed and restarting: it can be forgotten once its cooldown is over.",
  "devices.config_keep": "existing configuration kept",
  "devices.config_default": "default configuration",
  "devices.config_empty": "empty configuration",
  "devices.config_script": "own script",

  "action.flash": "Flash",
```

- [ ] **Step 10: Edit `app/src/locales/hu.json`**

1. In `app/src/locales/hu.json`, replace:

```json
  "devices.stale": "Utolsó ismert állapot; a kiszolgálás leállt.",
  "devices.forget_cooldown": "Épp most települt és újraindul: a várakozási idő letelte után lehet elfelejteni.",

  "action.flash": "Telepítés indítása",
```

with:

```json
  "devices.stale": "Utolsó ismert állapot; a kiszolgálás leállt.",
  "devices.forget_cooldown": "Épp most települt és újraindul: a várakozási idő letelte után lehet elfelejteni.",
  "devices.config_keep": "meglévő konfiguráció megtartva",
  "devices.config_default": "gyári alapkonfiguráció",
  "devices.config_empty": "üres konfiguráció",
  "devices.config_script": "saját szkript",

  "action.flash": "Telepítés indítása",
```

- [ ] **Step 11: Edit `crates/ni-cli/src/output.rs`**

1. In `crates/ni-cli/src/output.rs`, replace:

```rust
                reason: WaitReason::Concurrency,
            } => "waiting for a free slot".into(),
            DeviceState::Preparing { version: None } => "preparing packages".into(),
            DeviceState::Preparing { version: Some(v) } => format!("preparing RouterOS {v}"),
            DeviceState::Flashing {
                phase, sent, total, ..
```

with:

```rust
                reason: WaitReason::Concurrency,
            } => "waiting for a free slot".into(),
            DeviceState::Preparing { version: None, .. } => "preparing packages".into(),
            DeviceState::Preparing {
                version: Some(v), ..
            } => format!("preparing RouterOS {v}"),
            DeviceState::Flashing {
                phase, sent, total, ..
```

- [ ] **Step 12: Edit `crates/ni-engine/src/codes.rs` (with its unit tests)**

1. In `crates/ni-engine/src/codes.rs`, replace:

```rust
                reason: WaitReason::NoProfile,
            },
            DeviceState::Preparing { version: None },
            DeviceState::Flashing {
                version: String::new(),
```

with:

```rust
                reason: WaitReason::NoProfile,
            },
            DeviceState::Preparing {
                version: None,
                config: None,
            },
            DeviceState::Flashing {
                version: String::new(),
```

- [ ] **Step 13: Edit `crates/ni-engine/src/engine/core.rs` (with its unit tests)**

1. In `crates/ni-engine/src/engine/core.rs`, replace:

```rust
        dev.version = version.clone();
        let shown = version.as_ref().map(|v| v.to_string());
        run.tasks.retain(|t| !t.is_finished());
        run.tasks.push(tokio::spawn(async move {
```

with:

```rust
        dev.version = version.clone();
        let shown = version.as_ref().map(|v| v.to_string());
        let config = Some(profile.config.name().to_string());
        run.tasks.retain(|t| !t.is_finished());
        run.tasks.push(tokio::spawn(async move {
```

2. In `crates/ni-engine/src/engine/core.rs`, replace:

```rust
                .await;
        }));
        self.set_state(mac, DeviceState::Preparing { version: shown });
    }

```

with:

```rust
                .await;
        }));
        self.set_state(
            mac,
            DeviceState::Preparing {
                version: shown,
                config,
            },
        );
    }

```

- [ ] **Step 14: Edit `crates/ni-engine/src/event.rs` (with its unit tests)**

1. In `crates/ni-engine/src/event.rs`, replace:

```rust
    /// when the profile's version is the one the boot kernels were
    /// resolved to; otherwise it is resolved during preparation).
    Preparing {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    /// The install session runs; `phase` is [`ni_proto::Phase::name`],
```

with:

```rust
    /// when the profile's version is the one the boot kernels were
    /// resolved to; otherwise it is resolved during preparation).
    /// `config` is the profile's configuration mode (`keep`, `default`,
    /// `empty`, `script`).
    Preparing {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<String>,
    },
    /// The install session runs; `phase` is [`ni_proto::Phase::name`],
```

2. In `crates/ni-engine/src/event.rs`, replace:

```rust
            state: DeviceState::Preparing {
                version: version.map(String::from),
            },
        };
```

with:

```rust
            state: DeviceState::Preparing {
                version: version.map(String::from),
                config: None,
            },
        };
```

3. In `crates/ni-engine/src/event.rs`, replace:

```rust
        );
        assert_eq!(roundtrip(&preparing(Some("7.25.1")))["version"], "7.25.1");
        let failed = EngineEvent::Device {
            mac: MAC,
```

with:

```rust
        );
        assert_eq!(roundtrip(&preparing(Some("7.25.1")))["version"], "7.25.1");
        let with_config = EngineEvent::Device {
            mac: MAC,
            state: DeviceState::Preparing {
                version: None,
                config: Some("empty".into()),
            },
        };
        assert_eq!(
            roundtrip(&with_config),
            json!({"event": "device", "mac": "48:A9:8A:11:22:33", "state": "preparing",
                   "config": "empty"})
        );
        let failed = EngineEvent::Device {
            mac: MAC,
```

4. In `crates/ni-engine/src/event.rs`, replace:

```rust
                reason: WaitReason::Concurrency,
            },
            DeviceState::Preparing { version: None },
            DeviceState::Flashing {
                version: "7.25.1".into(),
```

with:

```rust
                reason: WaitReason::Concurrency,
            },
            DeviceState::Preparing {
                version: None,
                config: None,
            },
            DeviceState::Flashing {
                version: "7.25.1".into(),
```

- [ ] **Step 15: Run the tests to verify they pass**

Run: `cargo test -p ni-engine && cargo test -p ni-cli && (cd app && npm run check && npx vitest run)`
Expected: every suite `ok`; Vitest `Tests  127 passed (127)`

- [ ] **Step 16: Commit**

```bash
git add app/src/components/DeviceTable.svelte app/src/lib/state.svelte.ts app/src/lib/types.ts app/src/locales/en.json app/src/locales/hu.json app/tests/DeviceTable.test.ts app/tests/state.test.ts crates/ni-cli/src/output.rs crates/ni-engine/src/codes.rs crates/ni-engine/src/engine/core.rs crates/ni-engine/src/event.rs crates/ni-engine/tests/assets.rs crates/ni-engine/tests/flash.rs
git commit -m "feat: show each device's configuration mode" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Cache: never list or keep empty version directories

**Files:**
- Modify: `app/src/components/CachePanel.svelte`
- Modify: `app/tests/SettingsDialog.test.ts`
- Modify: `crates/ni-assets/src/assets/inventory.rs`

The cache view showed "RouterOS x.y.z – 0 B" for a version directory without files (left e.g. after deleting its last item). The inventory skips such directories, listings and deletions remove them (`delete_version(…, only_if_empty)`, under the same locks; a directory a download is starting in stays, `busy`), and "Delete all" includes them. The frontend also ignores a version without items. The inventory test's hash counter becomes per cache root: as one global counter it was flaky when tests ran in parallel. The unit tests live in the module itself (`#[cfg(test)] mod tests`), so they are written together with the code; run them right after.

- [ ] **Step 1: Update the tests in `app/tests/SettingsDialog.test.ts`**

1. In `app/tests/SettingsDialog.test.ts`, replace:

```ts
  });

  it("has nothing to delete when nothing is downloaded", async () => {
    const backend = mockBackend({
```

with:

```ts
  });

  it("never shows a version without files", async () => {
    const view = structuredClone(CACHE);
    view.inventory.versions.unshift({ version: "7.24.6", size: 0, items: [] });
    open(mockBackend({ cacheInventory: vi.fn(async () => view) }));
    expect(await screen.findByText(/RouterOS 7\.25\.1/)).toBeInTheDocument();
    expect(screen.queryByText(/RouterOS 7\.24\.6/)).toBeNull();
  });

  it("has nothing to delete when nothing is downloaded", async () => {
    const backend = mockBackend({
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd app && npx vitest run tests/SettingsDialog.test.ts`
Expected: `× never shows a version without files`, `Tests  1 failed | 18 passed (19)`

- [ ] **Step 3: Edit `app/src/components/CachePanel.svelte`**

1. In `app/src/components/CachePanel.svelte`, replace:

```svelte

  const inv = $derived(view?.inventory ?? null);
  const empty = $derived(!inv || (inv.versions.length === 0 && inv.partial.length === 0 && inv.local.length === 0));
  const allItems = $derived(inv ? [...inv.versions.flatMap((v) => v.items), ...inv.partial, ...inv.local] : []);
  const damagedItems = $derived(allItems.filter(damaged));

```

with:

```svelte

  const inv = $derived(view?.inventory ?? null);
  // A version without files is never shown (the backend no longer lists
  // or keeps one; this guards against an older backend).
  const versions = $derived(inv ? inv.versions.filter((v) => v.items.length > 0) : []);
  const empty = $derived(!inv || (versions.length === 0 && inv.partial.length === 0 && inv.local.length === 0));
  const allItems = $derived(inv ? [...versions.flatMap((v) => v.items), ...inv.partial, ...inv.local] : []);
  const damagedItems = $derived(allItems.filter(damaged));

```

2. In `app/src/components/CachePanel.svelte`, replace:

```svelte
    <table>
      <tbody>
        {#each inv.versions as v (v.version)}
          <tr class="group">
            <th colspan="2">{$t("settings.cache_version", { version: v.version })}</th>
```

with:

```svelte
    <table>
      <tbody>
        {#each versions as v (v.version)}
          <tr class="group">
            <th colspan="2">{$t("settings.cache_version", { version: v.version })}</th>
```

3. In `app/src/components/CachePanel.svelte`, replace:

```svelte
      type="button"
      class="small"
      disabled={working || empty || (inv?.versions.length === 0 && inv?.partial.length === 0)}
      onclick={() => ask({ scope: "all" }, $t("confirm.delete_cached_all"), inv?.total ?? 0)}
      >{$t("settings.delete_all")}</button
```

with:

```svelte
      type="button"
      class="small"
      disabled={working || empty || (versions.length === 0 && inv?.partial.length === 0)}
      onclick={() => ask({ scope: "all" }, $t("confirm.delete_cached_all"), inv?.total ?? 0)}
      >{$t("settings.delete_all")}</button
```

- [ ] **Step 4: Edit `crates/ni-assets/src/assets/inventory.rs` (with its unit tests)**

1. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
//! lock, removes the target's part files, and never follows a symlink out
//! of the cache root (a symlink is only unlinked).

use super::{blocking, part_target, Assets, LOCAL_DIR};
```

with:

```rust
//! lock, removes the target's part files, and never follows a symlink out
//! of the cache root (a symlink is only unlinked).
//!
//! A version directory with nothing in it (no index entry, no part file;
//! e.g. after its last item was deleted) is never listed, and listings and
//! deletions remove it, unless a download is starting in it.

use super::{blocking, part_target, Assets, LOCAL_DIR};
```

2. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
```

with:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
```

3. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
pub(crate) type Checks = Arc<Mutex<HashMap<PathBuf, Check>>>;

/// Files hashed by inventories (tests count them).
static HASHES: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn hashes() -> u64 {
    HASHES.load(Ordering::Relaxed)
}

```

with:

```rust
pub(crate) type Checks = Arc<Mutex<HashMap<PathBuf, Check>>>;

/// Files hashed by inventories, per cache root (tests count them; per root
/// so tests running in parallel do not see each other's).
#[cfg(test)]
static HASHES: Mutex<std::collections::BTreeMap<PathBuf, u64>> =
    Mutex::new(std::collections::BTreeMap::new());

/// Count one hashed `file` (`<root>/<dir>/<file>`) for the tests.
fn count_hash(_file: &Path) {
    #[cfg(test)]
    if let Some(root) = _file.parent().and_then(Path::parent) {
        let mut counts = HASHES.lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry(root.to_path_buf()).or_default() += 1;
    }
}

#[cfg(test)]
fn hashes(root: &Path) -> u64 {
    let counts = HASHES.lock().unwrap_or_else(|e| e.into_inner());
    counts.get(root).copied().unwrap_or(0)
}

```

4. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
        };
    }
    HASHES.fetch_add(1, Ordering::Relaxed);
    let valid = matches!(dir.lookup(index, name), Lookup::Valid(_));
    memo.insert(
```

with:

```rust
        };
    }
    count_hash(&path);
    let valid = matches!(dir.lookup(index, name), Lookup::Valid(_));
    memo.insert(
```

5. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
}

fn inventory(root: &Path, checks: &Checks, verify: bool) -> Inventory {
    let mut inv = Inventory::default();
    let mut versions: Vec<Version> = subdirs(root)
        .iter()
```

with:

```rust
}

/// The version directories under `root`, oldest first.
fn version_dirs(root: &Path) -> Vec<Version> {
    let mut versions: Vec<Version> = subdirs(root)
        .iter()
```

6. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
        .collect();
    versions.sort();
    for v in &versions {
        inv.versions
            .push(version_inventory(root, v, checks, verify));
    }
    for (dir_name, path) in subdirs(root) {
```

with:

```rust
        .collect();
    versions.sort();
    versions
}

/// Whether `dir` holds part files.
fn has_parts(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(PART_SUFFIX) && part_target(n).is_some())
        })
}

/// Remove `version`'s directory if nothing is in it; a directory being
/// downloaded into (`busy`) is left alone. Returns whether it was removed.
fn remove_if_empty(root: &Path, version: &Version) -> Result<bool, AssetError> {
    match delete_version(root, version, true) {
        Ok(None) => Ok(false),
        Ok(Some(_)) => Ok(true),
        Err(AssetError::Busy { .. }) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Everything in the cache. With `prune`, empty version directories are
/// removed on the way (errors ignored: they are not listed either way).
fn inventory(root: &Path, checks: &Checks, verify: bool, prune: bool) -> Inventory {
    let mut inv = Inventory::default();
    for v in &version_dirs(root) {
        let listed = version_inventory(root, v, checks, verify);
        if !listed.items.is_empty() {
            inv.versions.push(listed);
        } else if prune {
            let _ = remove_if_empty(root, v);
        }
    }
    for (dir_name, path) in subdirs(root) {
```

7. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust

/// Delete a whole version directory, holding every lock its writers use.
fn delete_version(root: &Path, version: &Version) -> Result<u64, AssetError> {
    let id = version.as_str();
    let path = root.join(id);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(AssetError::io("remove", &path)(e)),
    };
    if !meta.is_dir() {
        return remove(&path); // a symlink (or stray file): unlink only
    }
    let dir = CacheDir::new(path.clone());
```

with:

```rust

/// Delete a whole version directory, holding every lock its writers use.
/// With `only_if_empty`, nothing is deleted (`None`) unless the directory
/// has no index entry and no part file (checked under the locks).
fn delete_version(
    root: &Path,
    version: &Version,
    only_if_empty: bool,
) -> Result<Option<u64>, AssetError> {
    let id = version.as_str();
    let path = root.join(id);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(0)),
        Err(e) => return Err(AssetError::io("remove", &path)(e)),
    };
    if !meta.is_dir() {
        if only_if_empty {
            return Ok(None);
        }
        return remove(&path).map(Some); // a symlink (or stray file): unlink only
    }
    let dir = CacheDir::new(path.clone());
```

8. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
    }
    let _index_lock = try_hold(&dir, INDEX_FILE, id)?;
    let size = version_inventory(root, version, &Checks::default(), false).size;
    std::fs::remove_dir_all(&path).map_err(AssetError::io("remove", &path))?;
    drop(held);
    Ok(size)
}

```

with:

```rust
    }
    let _index_lock = try_hold(&dir, INDEX_FILE, id)?;
    let listed = version_inventory(root, version, &Checks::default(), false);
    if only_if_empty && (!listed.items.is_empty() || has_parts(&path)) {
        return Ok(None);
    }
    std::fs::remove_dir_all(&path).map_err(AssetError::io("remove", &path))?;
    drop(held);
    Ok(Some(listed.size))
}

```

9. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
    pub async fn cached_inventory(&self) -> Result<Inventory, AssetError> {
        let (root, checks) = (self.root.clone(), self.checks.clone());
        blocking(move || inventory(&root, &checks, false)).await
    }

```

with:

```rust
    pub async fn cached_inventory(&self) -> Result<Inventory, AssetError> {
        let (root, checks) = (self.root.clone(), self.checks.clone());
        blocking(move || inventory(&root, &checks, false, true)).await
    }

```

10. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
    pub async fn verify_cache(&self) -> Result<Inventory, AssetError> {
        let (root, checks) = (self.root.clone(), self.checks.clone());
        blocking(move || inventory(&root, &checks, true)).await
    }

```

with:

```rust
    pub async fn verify_cache(&self) -> Result<Inventory, AssetError> {
        let (root, checks) = (self.root.clone(), self.checks.clone());
        blocking(move || inventory(&root, &checks, true, true)).await
    }

```

11. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
        blocking(move || -> Result<u64, AssetError> {
            match scope {
                DeleteScope::Item { id } => delete_item(&root, &id),
                DeleteScope::Version { version } => {
                    let v = Version::parse(&version).map_err(|_| invalid(&version))?;
                    delete_version(&root, &v)
                }
                DeleteScope::All => {
                    let inv = inventory(&root, &checks, false);
                    let mut freed = 0;
                    for v in &inv.versions {
                        let version =
                            Version::parse(&v.version).map_err(|_| invalid(&v.version))?;
                        freed += delete_version(&root, &version)?;
                    }
                    for item in &inv.partial {
```

with:

```rust
        blocking(move || -> Result<u64, AssetError> {
            match scope {
                DeleteScope::Item { id } => {
                    let freed = delete_item(&root, &id)?;
                    if let Some(v) = item_version(&id) {
                        remove_if_empty(&root, &v)?;
                    }
                    Ok(freed)
                }
                DeleteScope::Version { version } => {
                    let v = Version::parse(&version).map_err(|_| invalid(&version))?;
                    Ok(delete_version(&root, &v, false)?.unwrap_or(0))
                }
                DeleteScope::All => {
                    let inv = inventory(&root, &checks, false, false);
                    let mut freed = 0;
                    for version in version_dirs(&root) {
                        freed += delete_version(&root, &version, false)?.unwrap_or(0);
                    }
                    for item in &inv.partial {
```

12. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
                }
                DeleteScope::Damaged => {
                    let inv = inventory(&root, &checks, false);
                    let mut freed = 0;
                    for item in inv.items().filter(|i| i.integrity.damaged()) {
                        freed += delete_item(&root, &item.id)?;
                    }
                    Ok(freed)
```

with:

```rust
                }
                DeleteScope::Damaged => {
                    let inv = inventory(&root, &checks, false, false);
                    let mut freed = 0;
                    for item in inv.items().filter(|i| i.integrity.damaged()) {
                        freed += delete_item(&root, &item.id)?;
                    }
                    for version in version_dirs(&root) {
                        remove_if_empty(&root, &version)?;
                    }
                    Ok(freed)
```

13. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
    async fn listings_reuse_verification_results_of_unchanged_files() {
        let (_srv, root, a) = filled("inv-memo").await;
        let before = hashes();
        let verified = a.verify_cache().await.unwrap();
        assert!(hashes() > before, "verify reads the files");
        assert!(verified
            .items()
            .all(|i| i.integrity != Integrity::Unchecked));
        let after_verify = hashes();
        let quick = a.cached_inventory().await.unwrap();
        assert_eq!(
            hashes(),
            after_verify,
            "unchanged files are not hashed again"
```

with:

```rust
    async fn listings_reuse_verification_results_of_unchanged_files() {
        let (_srv, root, a) = filled("inv-memo").await;
        let before = hashes(&root);
        let verified = a.verify_cache().await.unwrap();
        assert!(hashes(&root) > before, "verify reads the files");
        assert!(verified
            .items()
            .all(|i| i.integrity != Integrity::Unchecked));
        let after_verify = hashes(&root);
        let quick = a.cached_inventory().await.unwrap();
        assert_eq!(
            hashes(&root),
            after_verify,
            "unchanged files are not hashed again"
```

14. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust
        // Deleting the damaged ones does not hash everything again either.
        a.delete_cached(DeleteScope::Damaged).await.unwrap();
        assert_eq!(hashes(), after_verify);
        // A fresh manager has nothing remembered: unchecked, not hashed.
        let fresh = assets(&_srv, &root);
        let listed = fresh.cached_inventory().await.unwrap();
        assert_eq!(hashes(), after_verify);
        assert_eq!(
            item(&listed, "7.25.1/wireless-7.25.1-arm64.npk").integrity,
```

with:

```rust
        // Deleting the damaged ones does not hash everything again either.
        a.delete_cached(DeleteScope::Damaged).await.unwrap();
        assert_eq!(hashes(&root), after_verify);
        // A fresh manager has nothing remembered: unchecked, not hashed.
        let fresh = assets(&_srv, &root);
        let listed = fresh.cached_inventory().await.unwrap();
        assert_eq!(hashes(&root), after_verify);
        assert_eq!(
            item(&listed, "7.25.1/wireless-7.25.1-arm64.npk").integrity,
```

15. In `crates/ni-assets/src/assets/inventory.rs`, replace:

```rust

    #[tokio::test]
    async fn a_target_being_written_is_busy() {
        let (_srv, root, a) = filled("inv-busy").await;
```

with:

```rust

    #[tokio::test]
    async fn empty_version_directories_are_neither_listed_nor_kept() {
        let (_srv, root, a) = filled("inv-empty").await;
        // A directory left empty by an earlier version of the app.
        let stale = root.join("7.24.6");
        std::fs::create_dir_all(&stale).unwrap();
        CacheDir::new(stale.clone()).update(|_| {}).unwrap();
        let inv = a.cached_inventory().await.unwrap();
        let listed: Vec<&str> = inv.versions.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(listed, ["7.25.1"]);
        assert!(!stale.exists(), "removed by the listing");
        // Deleting the last items of a version removes its directory.
        for id in [
            "7.25.1/kernels",
            "7.25.1/routeros-7.25.1-arm64.npk",
            "7.25.1/wireless-7.25.1-arm64.npk",
        ] {
            assert!(root.join("7.25.1").exists(), "{id}");
            a.delete_cached(DeleteScope::Item { id: id.into() })
                .await
                .unwrap();
        }
        assert!(!root.join("7.25.1").exists());
        assert!(a.cached_inventory().await.unwrap().versions.is_empty());
        // One being downloaded into is left alone.
        std::fs::create_dir_all(&stale).unwrap();
        let dir = CacheDir::new(stale.clone());
        let held = lock::try_lock_file(&dir.lock_path(DIR_LOCK))
            .unwrap()
            .unwrap();
        assert!(a.cached_inventory().await.unwrap().versions.is_empty());
        assert!(stale.exists(), "busy: kept, but not listed");
        drop(held);
        a.delete_cached(DeleteScope::All).await.unwrap();
        assert!(!stale.exists(), "everything includes empty directories");
    }

    #[tokio::test]
    async fn a_target_being_written_is_busy() {
        let (_srv, root, a) = filled("inv-busy").await;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ni-assets --lib inventory && cargo test -p mac-netinstall --test bridge && (cd app && npx vitest run)`
Expected: `test result: ok. 9 passed` (inventory), `9 passed` (bridge); Vitest `Tests  128 passed (128)`

- [ ] **Step 6: Commit**

```bash
git add app/src/components/CachePanel.svelte app/tests/SettingsDialog.test.ts crates/ni-assets/src/assets/inventory.rs
git commit -m "fix(assets): never list or keep empty version directories" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Final checks, release build and the roadmap

**Files:**
- Modify: `docs/superpowers/plans/2026-11-06-00-roadmap.md`

Everything together, the app bundle, and the roadmap: the Plan 5 facts, the VM run of our simulator against the official tool, and the new hardware checks.

- [ ] **Step 1: Edit `docs/superpowers/plans/2026-11-06-00-roadmap.md`**

1. In `docs/superpowers/plans/2026-11-06-00-roadmap.md`, replace:

```markdown
| 3 | Assets (`ni-assets`) | Boot kernel extraction, NPK download/verify/cache, version resolution | no (network only) |
| 4 | Engine + CLI (`ni-engine`, `ni-cli`) | Multi-device orchestration, profiles, auto-repeat, usable CLI | no (first hardware test possible) |
| 5 | Reference capture + config modes | Linux VM with official `netinstall-cli` vs `ni-sim`; keep / no-default / empty config modes | Linux x86 VM |
| 6 | Tauri app + i18n (`app/`) | GUI (Svelte), EN + HU | no |
| 7 | CI + packaging | GitHub Actions (macos-14), `.dmg`, release notes | no |
```

with:

```markdown
| 3 | Assets (`ni-assets`) | Boot kernel extraction, NPK download/verify/cache, version resolution | no (network only) |
| 4 | Engine + CLI (`ni-engine`, `ni-cli`) | Multi-device orchestration, profiles, auto-repeat, usable CLI | no (first hardware test possible) |
| 5 | Protocol alignment + config modes | Official `netinstall-cli` wire behaviour (VM capture, golden fixtures, VM run of `ni-sim`); keep / default / empty / script + mode script, branding, license key, after-install, end to end | Linux x86 VM (done) |
| 6 | Tauri app + i18n (`app/`) | GUI (Svelte), EN + HU | no |
| 7 | CI + packaging | GitHub Actions (macos-14), `.dmg`, release notes | no |
```

2. In `docs/superpowers/plans/2026-11-06-00-roadmap.md`, replace:

```markdown
  pyNetinstall works on real hardware with plain UDP broadcast, so Plan 2 starts with plain UDP.
- Devices send Netinstall frames with **source IP 0.0.0.0**; the server receives its own
  broadcasts, so filter by source MAC.
- pyNetinstall cannot run on macOS unmodified (Linux-only `SIOCGIFHWADDR` ioctl), so the
  "validate ni-sim against pyNetinstall" idea from the spec is replaced by: ni-sim is written from
```

with:

```markdown
  pyNetinstall works on real hardware with plain UDP broadcast, so Plan 2 starts with plain UDP.
- Devices send Netinstall frames with **source IP 0.0.0.0**; the server receives its own
  broadcasts, so filter by source MAC. *(Superseded by Plan 5: our frames no longer carry our
  MAC; our own broadcasts are dropped by source address — see "Facts established during Plan 5".)*
- pyNetinstall cannot run on macOS unmodified (Linux-only `SIOCGIFHWADDR` ioctl), so the
  "validate ni-sim against pyNetinstall" idea from the spec is replaced by: ni-sim is written from
```

3. In `docs/superpowers/plans/2026-11-06-00-roadmap.md`, replace:

```markdown
  (`ni-sim` `SimConfig::dedup = false` test shows the corruption mode).
- **Replies after the last chunk:** we require two state-matched replies, the second `RETR`
  (mirrors pyNetinstall). Capture what a real board actually sends.
- **Seq wraparound:** plans over 65535 frames (~64 MiB) are rejected; verify on hardware before lifting.

```

with:

```markdown
  (`ni-sim` `SimConfig::dedup = false` test shows the corruption mode).
- **Replies after the last chunk:** we require two state-matched replies, the second `RETR`
  (mirrors pyNetinstall). Capture what a real board actually sends. *(Answered by Plan 5 for the
  official server: one reply per chunk, then an empty poll; see "Open hardware checks (from
  Plan 5)".)*
- **Seq wraparound:** plans over 65535 frames (~64 MiB) are rejected; verify on hardware before lifting.

```

4. In `docs/superpowers/plans/2026-11-06-00-roadmap.md`, replace:

```markdown
  (`no_mac`) instead of sending frames from the placeholder (it uses the interface MAC as the
  source MAC of its Netinstall frames and to drop its own broadcasts; two Macs would collide).
  `ni-net/tests/mac_vs_ifconfig.rs` checks the result against `ifconfig`/`ioreg`.
- End-to-end tests run the real engine on `lo0` with ephemeral ports (`Ports`) against
```

with:

```markdown
  (`no_mac`) instead of sending frames from the placeholder (it uses the interface MAC as the
  source MAC of its Netinstall frames and to drop its own broadcasts; two Macs would collide).
  *(Superseded by Plan 5: frames mirror the device's MAC fields like the official tool, so no MAC
  is needed; `no_mac` and `--server-mac` are gone. The interface MAC is still listed for display.)*
  `ni-net/tests/mac_vs_ifconfig.rs` checks the result against `ifconfig`/`ioreg`.
- End-to-end tests run the real engine on `lo0` with ephemeral ports (`Ports`) against
```

5. In `docs/superpowers/plans/2026-11-06-00-roadmap.md`, replace:

```markdown
- **Robustness:** `applicationShouldTerminate:` runs under `catch_unwind` (panic → quit); a failed
  guard install is logged, the app still starts. Stop asks about the engine's current busy list.
```

with:

```markdown
- **Robustness:** `applicationShouldTerminate:` runs under `catch_unwind` (panic → quit); a failed
  guard install is logged, the app still starts. Stop asks about the engine's current busy list.

## Facts established during Plan 5 (2026-11-06)

Ground truth: `docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md` (official
`netinstall-cli` 7.25.1 captured against a simulated device in a VM).

- **Session = the official server's.** `OFFR\n<license key>\n<keep|>\n<removeBranding|>\n\0`
  → `YACK`; empty (format, `STRT`); empty (`RETR`); per file `FILE\n<name>\n<size>\n\0` → one frame
  per 1024 bytes (none for an empty file), one `RETR` each (no second reply after the last
  chunk) → an empty poll; after the last file, empty polls until `WTRM`; `TERM\n<message>\0`. Text
  payloads end with a NUL counted in the length. ack = seq − 2 (0 for the first two). The
  system package is always first (`InstallPlan::system_package_first`).
- **Header MACs:** our `[0:6]` is `[6:12]` of the device frame answered (`ff:ff:ff:ff:ff:ff` for
  the offer), `[6:12]` the device: our own MAC is never on the wire. So the engine needs no MAC
  (`no_mac`, `EngineConfig::server_mac`, `NetPlan::server_mac` and the CLI's `--server-mac` are
  gone) and drops its own looped-back broadcasts by source address (server IP + our port); a
  looped-back frame could never pass a session anyway (`ack` is `seq − 2`, a reply has
  `seq == ack`). The self-echo hazard of the research doc §5 does not apply to us.
- **Deviations from the official server (bounded where it waits forever):** a `NACK` to the
  offer re-offers the same frame after `SessionConfig::offer_retry` (1 s) up to
  `max_retransmits` times, then `flash_rejected` (official: forever); polls after the last file
  are paced (`poll_interval` 250 ms, at most `finish_polls` 20) and then fall back to
  pyNetinstall's `FILE\n` (a device that never says `WTRM` still finishes); `WTRM` before the
  last file fails with `flash_ended_early` (`remaining`) — the official tool silently skips the
  rest and reports success (seen in the VM run "early" below); unanswered frames are re-sent
  with the same seq (official: never).
- **Configuration modes** (profile JSON ↔ `ni-cli` ↔ official flag): `keep` ↔ `--config keep` ↔
  no flag; `default` ↔ `--config default` (our default, for continuity with older profiles) ↔
  `-r`; `empty` ↔ `--config empty` ↔ `-e` (an empty `autorun.scr`); `script` ↔ `--script F` ↔
  `-s F`; `mode_script` ↔ `--mode-script F` ↔ `-sm F` (with default, empty **or script**: the
  captures show `-r -s -sm` is valid, so only keep refuses it, `mode_script_with_keep`);
  `remove_branding` ↔ `--remove-branding` ↔ `-b`; `license_key` (key file with line breaks
  removed, printable ASCII, ≤ 4096 bytes) ↔ `--license-key F` ↔ `-k F`; `after_install`
  `reboot` / `reboot_message` / `shutdown` ↔ `--after …` ↔ none / `--reboot` / `--shutdown`.
  Older profiles load unchanged (`custom_script` reads as `script`).
- **Hello:** field 0 must be `DSCV`; ≥ 6 fields accepted (the official server wants 8), extra
  fields kept in `DeviceInfo::extra`. A `min_os` above the installed version is only a
  `min_os_newer` warning (the official tool does not check it either).
- **Conformance tests:** `crates/ni-proto/tests/fixtures/official-7.25.1/` holds 16 compact
  captures (e01 keep, e02 -r, e03 -e, e04 -b, e05 -s, e06 -r -s, e09 -r -sm, e10 -e -sm,
  e11 -r -s -sm, e12 -k, e13 --reboot, e14 --shutdown, e15 -r -b, e16 -e -b, x09 two packages
  + -s, x18 container first; no package bytes). `tests/conformance.rs` checks that our session
  sends every captured server frame (MACs, seq/ack, control bytes, data sizes);
  `ni-sim/tests/official_replay.rs` feeds the official frames to `DeviceSim`, which must answer
  exactly like the capture.
- **ni-sim** now is the device the official server expects (8-field hello, zeros in `[6:12]`,
  `RETR` to the first poll after a file and `WTRM` to the next; `SimConfig::after_files` also
  models pyNetinstall-era, never-`WTRM` and early-`WTRM` devices), validates the offer and records
  the offer, the files and the `TERM`.
- **BOOTP:** requests shorter than 300 bytes are answered (the official server ignores them); our
  DHCP options stay a superset of its plain reply.
- **GUI:** the profile editor offers the four configurations (with a help line each), "After
  install" (reboot / reboot with message / shut down) and an Advanced group (mode script, remove
  branding, license key). The key file is picked and read in Rust (`pick_license_key`, at most
  64 KiB, `key_file_failed`); only the key reaches the window. Device rows show the configuration
  mode under the version (`config` of the `preparing` state and of snapshot devices).
- **Cache:** a version directory without files is never listed ("RouterOS x.y.z – 0 B" was), and
  listings and deletions remove it unless a download is starting in it; "Delete all" includes
  such directories. The inventory test's hash counter is per cache root now (it was flaky when
  tests ran in parallel).
- **Not done (decisions):** TFTP "official-compat" mode (DATA from port 69, 1452-byte blocks
  without options) — the research shows the official choice, not that RouterBOOT needs it; ours
  is RFC 1350 (512-byte blocks from a per-transfer port), and replying from port 69 would need
  `SO_REUSEPORT` on the listener, which Plan 2 deliberately avoids (it hides a second server).
  Revisit only if a hardware check fails. PING/PONG detection of another Netinstall server
  (official: `PING\n` at start, `PONG\n` from a foreign address → refuse unless `-c`): follow-up.

## VM validation (Plan 5, 2026-11-06)

Our Rust `DeviceSim` against the official `netinstall-cli` 7.25.1 on the x86_64 Ubuntu VM:

- Built on the Mac: `rustup target add x86_64-unknown-linux-musl`, then
  `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld cargo build -p ni-sim --example udp_device --release --target x86_64-unknown-linux-musl`
  (static-pie, no extra linker); copied to the VM.
- Namespaces `ns5-srv` (veth `02:00:00:00:00:01`, 192.168.88.1/24) and `ns5-dev`
  (192.168.88.2/24, default route on the veth); `netinstall-cli -i veth5-srv <flags> <packages>`
  in one, `udp_device --out DIR` (the simulator on a plain UDP socket `0.0.0.0:5000` →
  `255.255.255.255:5000`, no BOOTP) in the other; namespaces removed afterwards.
- **All 13 runs passed** — keep, `-r`, `-e`, `-s`, `-r -sm`, `-e -sm`, `-r -s -sm`, `-b`, `-k`,
  `--reboot`, `--shutdown`, two packages (container first on the command line; routeros was sent
  first) and `--after-files early`: the official tool printed "Successfully finished installing",
  the received `routeros`/`container` packages are byte-identical to the originals (`cmp`), the
  scripts identical, `-e` delivered a 0-byte `autorun.scr`, and the offers / `TERM` were the
  captured ones. The official server accepted a device sending from 192.168.88.2 (not 0.0.0.0)
  with zeros in `[6:12]`. In "early" (the device answers `WTRM` to the first poll) the official
  tool skipped `autorun.scr` and still reported success.

## Open hardware checks (from Plan 5)

- **Post-file polls:** that a real device answers the first empty poll after a file `RETR` and the
  next one `WTRM` (inferred, never observed). If it keeps answering `RETR` for more than
  `finish_polls` × `poll_interval` (≈ 5 s) we send `FILE\n`: check what it does then.
- **`[6:12]` of device replies:** what real devices put there (we mirror it; the simulator uses
  zeros, the only value the official server handled cleanly).
- **`removeBranding`:** what a device does with the field (sent when `-b` is *absent*, as
  captured) — check on a branded device before relying on "Remove branding".
- **De-duplication of a re-sent frame** (same seq): still open (Plan 1); the official server never
  re-sends, so a device that does not de-duplicate would corrupt a file only with our retransmits
  (`--retransmits 0` turns them off).
- **NACK to the offer:** when devices send it, and whether our bounded re-offer is enough.
- **TFTP compat need:** that RouterBOOT boots from our 512-byte blocks sent from a per-transfer
  port; if not, add the official-compat mode (port 69, 1452 bytes).
- **Configuration modes on a device:** keep really keeps the configuration, empty leaves none,
  `mode.scr` runs (and when), a license key is accepted on a device that needs one; hello fields
  7/8 on real devices.

```

- [ ] **Step 2: Check the whole workspace and the frontend**

Run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo test --workspace
cd app && npm run check && npx vitest run && cd ..
```

Expected: no formatting diff, no warnings, docs build, every Rust suite `ok` — 563 tests in
total (533 before this plan); svelte-check `0 ERRORS 0 WARNINGS`; Vitest `Test Files  12 passed
(12)`, `Tests  128 passed (128)` (123 before). Run the Rust tests and Vitest a second time: the
same.

- [ ] **Step 3: Build the app and the DMG**

```bash
cd app
CI=true npx tauri build --target aarch64-apple-darwin --bundles app,dmg
codesign -dv "../target/aarch64-apple-darwin/release/bundle/macos/Mac Netinstall.app" 2>&1 | grep -E "^Identifier|flags"
cd ..
```

Expected: `Finished 2 bundles` (`Mac Netinstall.app` ~20 MiB, `Mac Netinstall_0.1.0_aarch64.dmg`
~6 MiB); `Identifier=io.github.sepsig.mac-netinstall`, `flags=0x10002(adhoc,runtime)`.
Optionally open the app: a profile's editor shows Configuration (four choices), After install
and Advanced.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/plans/2026-11-06-00-roadmap.md
git commit -m "docs(roadmap): Plan 5 facts, VM validation and hardware checks" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Done

Plan 5 is complete when Task 13's checks pass. Open questions for real hardware are in the
roadmap ("Open hardware checks (from Plan 5)"); the TFTP compat mode and PING/PONG detection are
documented follow-ups.
