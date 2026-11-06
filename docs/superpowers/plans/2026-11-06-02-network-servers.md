# Plan 2 — Network Servers (ni-net, ni-bootp, ni-tftp) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Real-socket building blocks for network boot: interface listing and interface-bound UDP sockets (`ni-net`), a BOOTP/DHCP server that hands RouterBOOT an address and a boot kernel name (`ni-bootp`), and a read-only TFTP server that serves the kernel (`ni-tftp`) — all proven by unit tests and loopback integration tests, no root.

**Architecture:** Each server crate is a pure, sans-IO core (codec + decision logic, unit-tested with byte fixtures; time passed in as `Instant`) plus a thin tokio runner that takes an already-bound `tokio::net::UdpSocket` and reports what it did on an unbounded `mpsc` channel. `ni-net` creates sockets with `socket2` (`SO_REUSEADDR`/`SO_REUSEPORT`, `SO_BROADCAST`, `IP_BOUND_IF`) and lists interfaces with `getifaddrs`. The engine (Plan 4) will own sockets and wire the runners together.

**Tech Stack:** Rust stable (rustup, edition 2021), `tokio` 1.54.1, `socket2` 0.6.5, `nix` 0.31.3 (feature `net` only), `bytes` 1, `thiserror` 2.

**Spec:** `docs/superpowers/specs/2026-11-06-mac-netinstall-design.md`
**Roadmap:** `docs/superpowers/plans/2026-11-06-00-roadmap.md`
**Builds on:** Plan 1 (`ni-proto::Mac` is reused for hardware addresses).

## Background (read first)

**Boot flow of a RouterBOARD in Etherboot mode:**

1. RouterBOOT broadcasts a BOOTP/DHCP request from `0.0.0.0:68` to `255.255.255.255:67`. Option 60 (vendor class) names the boot loader architecture:

   | Vendor class | `BootArch` | Notes |
   |---|---|---|
   | `Mips_boot` | `Mips` | mipsbe |
   | `MMipsBoot` | `MMips` | MT7621 |
   | `Powerboot` | `PowerPc` | e300 / mpc83xx |
   | `e500_boot`, `e500sboot` | `E500` | both map to one kernel |
   | `440__boot` | `Ppc440` | |
   | `tile_boot` | `Tile` | |
   | `ARM__boot` | `Arm` | |
   | `ARM64__boot` | `Arm64` | |
   | `PXEClient…`, `X86_64__boot`, `ARM64UEFI__boot` | — | `Unsupported` in MVP |

2. We reply (broadcast to `255.255.255.255:68`, the client has no address yet) with `yiaddr` from a MAC-keyed pool, `siaddr` = our IP (TFTP next-server) and `file` = the kernel name for that architecture. DHCP clients get OFFER then ACK; plain BOOTP clients (no option 53) get one BOOTP reply.
3. RouterBOOT fetches `file` from `siaddr` over TFTP (RFC 1350, port 69). Its TFTP client is known to misbehave with `blksize` negotiation (pyNetinstall's dnsmasq example uses `tftp-no-blocksize`), so `blksize` is **ignored by default**.
4. The kernel boots and speaks the Netinstall protocol (Plan 1).

**Verified on this machine (macOS, arm64) while prototyping this plan:**

- An unprivileged process can bind UDP `0.0.0.0:67`, `:69` and `:5000`.
- `IP_BOUND_IF` (`IPPROTO_IP`, option 25) is `socket2::Socket::bind_device_by_index_v4(Option<NonZeroU32>)` in socket2 0.6.5 — only with feature `all`. `SockRef::device_index_v4()` reads it back.
- A wildcard socket (`0.0.0.0`) with `IP_BOUND_IF = lo0` still receives unicast datagrams to `127.0.0.1`.
- `lo0` cannot broadcast: `sendto(255.255.255.255)` on an `lo0`-bound socket fails with `ENETUNREACH`. Hence `ni_bootp::serve` takes the reply destination as a parameter, and loopback tests pass the test client's address.
- `nix` 0.31: `getifaddrs` and `if_nametoindex` need only feature `net`. `LinkAddr::addr()` does **not** check the address length, so we check `alen() == 6` first. `lo0` has no MAC.
- On BSD, a second bind to a port fails with `EADDRINUSE` unless *every* socket on it set `SO_REUSEPORT`; that is how the "another DHCP/TFTP server is running" preflight (Plan 4) will detect `bootpd`.

**Toolchain:** rustup's cargo must come first in `PATH` (`which cargo` → `~/.cargo/bin/cargo`, `cargo --version` ≥ 1.80). If not, `export PATH="$HOME/.cargo/bin:$PATH"` for the session (see Plan 1 Task 0).

## Design notes and deliberate choices

- **No `Transport` trait yet.** The runners take a `tokio::net::UdpSocket`; nothing here needs to fake one (loopback sockets are cheap and real). The roadmap mentioned a trait — it is deferred until a BPF transport is actually needed.
- **`NetError`** distinguishes `AddrInUse { port }` (port conflict, e.g. `bootpd` from Internet Sharing), `UnknownInterface(name)` and `Io { op, source }`. It lives in its own module so interface lookup and socket binding share it.
- **`BindOptions::reuse`** (default `true`) toggles `SO_REUSEADDR` + `SO_REUSEPORT`. Turned off for per-transfer TFTP sockets and for the deliberate conflict in tests.
- **BOOTP outcome is typed.** `BootpResponder::handle` returns `Result<Outcome, PacketError>`; `Outcome::Ignored { mac, reason }` lets the engine log e.g. "unsupported device (X86_64__boot)" or "no boot kernel for arm".
- **TFTP extras beyond the minimum:** `timeout` option honored by default (`honor_timeout`), negotiated `blksize` capped by `max_blksize` (1468, fits a 1500 MTU) when enabled, a repeated RRQ from a peer whose transfer is running is ignored, and foreign packets on a transfer port get error 5 (unknown transfer ID) per RFC 1350.
- **Events** use `tokio::sync::mpsc::UnboundedSender` (low volume: a few per device), dropped silently if the receiver is gone. Stop a server by aborting its task.
- **Every code block below was compiled and tested** in a prototype before this plan was written; the final tree must match it byte for byte.

## File structure

```
Cargo.toml                            # + members ni-net, ni-bootp, ni-tftp; + nix, socket2, tokio
crates/ni-net/Cargo.toml
crates/ni-net/src/lib.rs              # module wiring + re-exports
crates/ni-net/src/error.rs            # NetError
crates/ni-net/src/iface.rs            # Interface, list_interfaces, interface_index
crates/ni-net/src/udp.rs              # BindOptions, bind_udp (socket2 + IP_BOUND_IF)
crates/ni-bootp/Cargo.toml
crates/ni-bootp/src/lib.rs
crates/ni-bootp/src/arch.rs           # BootArch, VendorClass
crates/ni-bootp/src/packet.rs         # Request parser, build_reply
crates/ni-bootp/src/pool.rs           # LeasePool
crates/ni-bootp/src/responder.rs      # BootpResponder (sans-IO), Outcome
crates/ni-bootp/src/server.rs         # serve(), BootpEvent
crates/ni-bootp/tests/loopback.rs
crates/ni-tftp/Cargo.toml
crates/ni-tftp/src/lib.rs
crates/ni-tftp/src/packet.rs          # Packet codec, ErrorCode
crates/ni-tftp/src/transfer.rs        # Transfer state machine (sans-IO)
crates/ni-tftp/src/accept.rs          # accept(): validation, file lookup, options
crates/ni-tftp/src/server.rs          # serve(), TftpConfig, TftpEvent
crates/ni-tftp/tests/loopback.rs      # minimal TFTP client + scenarios
docs/superpowers/plans/2026-11-06-00-roadmap.md   # Task 12: Plan 2 findings
```

Dependency direction: `ni-net → ni-proto`; `ni-bootp → ni-proto` (dev: `ni-net`); `ni-tftp → ni-net`.

---

### Task 1: ni-net crate: error type and interface listing

**Files:**
- Modify: `Cargo.toml` (workspace members + dependencies)
- Create: `crates/ni-net/Cargo.toml`, `crates/ni-net/src/lib.rs`, `crates/ni-net/src/error.rs`, `crates/ni-net/src/iface.rs`

`getifaddrs(3)` returns one entry per address. On macOS the `AF_LINK` entry carries the interface index and the MAC; IPv4 entries carry address + netmask. We fold them per name.

- [ ] **Step 1: Replace the workspace `Cargo.toml` (adds `ni-net` and the new pinned dependencies)**

```toml
[workspace]
resolver = "2"
members = [
    "crates/ni-proto",
    "crates/ni-sim",
    "crates/ni-net",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "GPL-3.0-only"
rust-version = "1.80"

[workspace.dependencies]
bytes = "1"
thiserror = "2"
proptest = "1"
nix = { version = "0.31.3", default-features = false }
socket2 = "0.6.5"
tokio = "1.54.1"
ni-proto = { path = "crates/ni-proto" }
ni-net = { path = "crates/ni-net" }
```

- [ ] **Step 2: Create `crates/ni-net/Cargo.toml`**

```toml
[package]
name = "ni-net"
description = "macOS networking helpers: interface list, interface-bound UDP sockets"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
ni-proto.workspace = true
nix = { workspace = true, features = ["net"] }
socket2 = { workspace = true, features = ["all"] }
thiserror.workspace = true
tokio = { workspace = true, features = ["net"] }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt", "time"] }
```

- [ ] **Step 3: Create `crates/ni-net/src/lib.rs` (interfaces only for now)**

```rust
//! macOS networking helpers for mac-netinstall.
//!
//! - [`list_interfaces`]: network interfaces with MAC, IPv4 addresses and flags.
//!
//! No root and no BPF: everything here uses ordinary sockets.

pub mod error;
pub mod iface;

pub use error::NetError;
pub use iface::{interface_index, list_interfaces, Interface};
```

- [ ] **Step 4: Create `crates/ni-net/src/error.rs`**

```rust
//! Error type shared by [`crate::iface`] and [`crate::udp`].

use std::io;
use thiserror::Error;

/// Errors from interface lookup and [`crate::bind_udp`].
#[derive(Debug, Error)]
pub enum NetError {
    /// Another socket holds the port (another DHCP/TFTP server running?).
    #[error("UDP port {port} is already in use")]
    AddrInUse { port: u16 },
    #[error("unknown network interface {0:?}")]
    UnknownInterface(String),
    #[error("{op} failed: {source}")]
    Io {
        op: &'static str,
        #[source]
        source: io::Error,
    },
}
```

- [ ] **Step 5: Write the failing tests — create `crates/ni-net/src/iface.rs` containing only the test module (interface listing)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lo0_is_listed_as_loopback_with_127_0_0_1() {
        let all = list_interfaces().unwrap();
        let lo0 = all.iter().find(|i| i.name == "lo0").expect("lo0 listed");
        assert!(lo0.is_loopback);
        assert!(lo0.is_up);
        assert!(lo0.index > 0);
        assert_eq!(lo0.mac, None);
        assert!(lo0
            .ipv4
            .contains(&(Ipv4Addr::LOCALHOST, Ipv4Addr::new(255, 0, 0, 0))));
    }

    #[test]
    fn some_interface_has_a_mac() {
        // Every Mac has at least one Ethernet-like interface (en0).
        let all = list_interfaces().unwrap();
        assert!(all.iter().any(|i| i.mac.is_some() && !i.is_loopback));
    }

    #[test]
    fn interface_index_matches_listing() {
        let all = list_interfaces().unwrap();
        let lo0 = all.iter().find(|i| i.name == "lo0").unwrap();
        assert_eq!(interface_index("lo0").unwrap().get(), lo0.index);
    }

    #[test]
    fn unknown_interface_is_an_error() {
        let err = interface_index("nosuch0").unwrap_err();
        assert!(matches!(err, NetError::UnknownInterface(name) if name == "nosuch0"));
    }
}
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test -p ni-net`
Expected: FAIL — compile errors such as `error[E0432]: unresolved import`.

- [ ] **Step 7: Implement — insert this above the test module in `crates/ni-net/src/iface.rs` (the file is then this code followed by the tests from the previous step)**

```rust
//! Network interface enumeration via `getifaddrs(3)`.
//!
//! On macOS `getifaddrs` returns one entry per address: an `AF_LINK` entry
//! carrying the interface index and MAC address, plus one entry per IPv4/IPv6
//! address. [`list_interfaces`] folds them into one [`Interface`] per name.

use crate::error::NetError;
use ni_proto::Mac;
use nix::ifaddrs::getifaddrs;
use nix::net::if_::{if_nametoindex, InterfaceFlags};
use std::collections::BTreeMap;
use std::io;
use std::net::Ipv4Addr;
use std::num::NonZeroU32;

/// One network interface (e.g. `en7`, `lo0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// BSD name, e.g. `en0`.
    pub name: String,
    /// Kernel interface index (what `IP_BOUND_IF` expects).
    pub index: u32,
    /// Hardware address; `None` for interfaces without one (e.g. `lo0`).
    pub mac: Option<Mac>,
    /// IPv4 addresses with their netmasks.
    pub ipv4: Vec<(Ipv4Addr, Ipv4Addr)>,
    /// `IFF_UP`: administratively up.
    pub is_up: bool,
    /// `IFF_RUNNING`: resources allocated (on macOS: link is active).
    pub is_running: bool,
    /// `IFF_LOOPBACK`.
    pub is_loopback: bool,
}

/// List all interfaces, sorted by name.
pub fn list_interfaces() -> io::Result<Vec<Interface>> {
    let mut by_name: BTreeMap<String, Interface> = BTreeMap::new();
    for ifa in getifaddrs()? {
        let entry = by_name
            .entry(ifa.interface_name.clone())
            .or_insert_with(|| Interface {
                name: ifa.interface_name.clone(),
                index: 0,
                mac: None,
                ipv4: Vec::new(),
                is_up: false,
                is_running: false,
                is_loopback: false,
            });
        entry.is_up |= ifa.flags.contains(InterfaceFlags::IFF_UP);
        entry.is_running |= ifa.flags.contains(InterfaceFlags::IFF_RUNNING);
        entry.is_loopback |= ifa.flags.contains(InterfaceFlags::IFF_LOOPBACK);
        let Some(addr) = ifa.address else { continue };
        if let Some(link) = addr.as_link_addr() {
            entry.index = link.ifindex() as u32;
            // `LinkAddr::addr` does not check the address length itself.
            if link.alen() == 6 {
                entry.mac = link.addr().map(Mac);
            }
        } else if let Some(sin) = addr.as_sockaddr_in() {
            let mask = ifa
                .netmask
                .as_ref()
                .and_then(|m| m.as_sockaddr_in())
                .map(|m| m.ip())
                .unwrap_or(Ipv4Addr::UNSPECIFIED);
            entry.ipv4.push((sin.ip(), mask));
        }
    }
    let mut out: Vec<Interface> = by_name.into_values().collect();
    for iface in &mut out {
        if iface.index == 0 {
            if let Ok(index) = if_nametoindex(iface.name.as_str()) {
                iface.index = index;
            }
        }
    }
    Ok(out)
}

/// Interface index for `name`, as needed by `IP_BOUND_IF`.
pub fn interface_index(name: &str) -> Result<NonZeroU32, NetError> {
    if_nametoindex(name)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or_else(|| NetError::UnknownInterface(name.to_string()))
}
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test -p ni-net`
Expected: PASS — `test result: ok. 4 passed`.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock crates/ni-net
git commit -m "feat(ni-net): list network interfaces via getifaddrs" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 2: ni-net: interface-bound UDP sockets

**Files:**
- Create: `crates/ni-net/src/udp.rs`
- Modify: `crates/ni-net/src/lib.rs`

`bind_udp` builds the socket with `socket2` (so options are set *before* `bind`), then hands it to tokio. `IP_BOUND_IF` = `bind_device_by_index_v4` (needs socket2 feature `all`). The `AddrInUse` test creates the conflict deliberately: a holder socket without `SO_REUSEPORT`, then a second bind to its port.

- [ ] **Step 1: Replace `crates/ni-net/src/lib.rs` with the final version**

```rust
//! macOS networking helpers for mac-netinstall.
//!
//! - [`list_interfaces`]: network interfaces with MAC, IPv4 addresses and flags.
//! - [`bind_udp`]: UDP sockets bound to one interface with `IP_BOUND_IF`,
//!   so broadcasts go out (and are received) on the selected Ethernet port only.
//!
//! No root and no BPF: everything here uses ordinary sockets.

pub mod error;
pub mod iface;
pub mod udp;

pub use error::NetError;
pub use iface::{interface_index, list_interfaces, Interface};
pub use udp::{bind_udp, BindOptions};
```

- [ ] **Step 2: Write the failing tests — create `crates/ni-net/src/udp.rs` containing only the test module (binding on lo0)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn lo0(port: u16) -> BindOptions {
        BindOptions::new(port)
            .addr(Ipv4Addr::LOCALHOST)
            .interface("lo0")
    }

    #[tokio::test]
    async fn binds_ephemeral_port_on_lo0() {
        let sock = bind_udp(&lo0(0)).unwrap();
        let local = sock.local_addr().unwrap();
        assert_eq!(local.ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(local.port(), 0);
        let bound = socket2::SockRef::from(&sock).device_index_v4().unwrap();
        assert_eq!(bound, Some(interface_index("lo0").unwrap()));
    }

    #[tokio::test]
    async fn two_lo0_sockets_exchange_a_datagram() {
        let a = bind_udp(&lo0(0)).unwrap();
        let b = bind_udp(&lo0(0)).unwrap();
        a.send_to(b"ping", b.local_addr().unwrap()).await.unwrap();
        let mut buf = [0u8; 16];
        let (n, from) = tokio::time::timeout(Duration::from_secs(2), b.recv_from(&mut buf))
            .await
            .expect("datagram arrives")
            .unwrap();
        assert_eq!(&buf[..n], b"ping");
        assert_eq!(from, a.local_addr().unwrap());
    }

    #[tokio::test]
    async fn port_held_without_reuse_reports_addr_in_use() {
        let holder = bind_udp(&lo0(0).reuse(false)).unwrap();
        let port = holder.local_addr().unwrap().port();
        let err = bind_udp(&lo0(port)).unwrap_err();
        assert!(
            matches!(err, NetError::AddrInUse { port: p } if p == port),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn two_reuse_sockets_can_share_a_port() {
        let first = bind_udp(&lo0(0)).unwrap();
        let port = first.local_addr().unwrap().port();
        bind_udp(&lo0(port)).unwrap();
    }

    #[tokio::test]
    async fn unknown_interface_fails_before_bind() {
        let err = bind_udp(&BindOptions::new(0).interface("nosuch0")).unwrap_err();
        assert!(matches!(err, NetError::UnknownInterface(_)));
    }

    #[tokio::test]
    async fn broadcast_flag_is_applied() {
        let sock = bind_udp(&lo0(0).broadcast(true)).unwrap();
        assert!(sock.broadcast().unwrap());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ni-net`
Expected: FAIL — compile errors such as `error[E0432]: unresolved import`.

- [ ] **Step 4: Implement — insert this above the test module in `crates/ni-net/src/udp.rs` (the file is then this code followed by the tests from the previous step)**

```rust
//! Interface-bound UDP sockets.
//!
//! macOS has no `SO_BINDTODEVICE`; the equivalent is the `IP_BOUND_IF`
//! socket option (`IPPROTO_IP`, option 25), exposed by `socket2` as
//! [`socket2::Socket::bind_device_by_index_v4`]. A socket bound to an
//! interface sends (including to `255.255.255.255`) only through it.

use crate::error::NetError;
use crate::iface::interface_index;
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::net::UdpSocket;

/// How to create a UDP socket with [`bind_udp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindOptions {
    /// Local address; `0.0.0.0` is needed to receive broadcasts.
    pub addr: Ipv4Addr,
    /// Local port; 0 picks an ephemeral port.
    pub port: u16,
    /// Interface name for `IP_BOUND_IF` (e.g. `en7`); `None` = not bound.
    pub interface: Option<String>,
    /// Set `SO_BROADCAST` (needed to send to `255.255.255.255`).
    pub broadcast: bool,
    /// Set `SO_REUSEADDR` and `SO_REUSEPORT`. A port held by a process
    /// that did not set them (e.g. `bootpd`) still fails with
    /// [`NetError::AddrInUse`].
    pub reuse: bool,
}

impl BindOptions {
    /// `0.0.0.0:port`, no interface, no broadcast, reuse on.
    pub fn new(port: u16) -> Self {
        BindOptions {
            addr: Ipv4Addr::UNSPECIFIED,
            port,
            interface: None,
            broadcast: false,
            reuse: true,
        }
    }

    pub fn addr(mut self, addr: Ipv4Addr) -> Self {
        self.addr = addr;
        self
    }

    pub fn interface(mut self, name: impl Into<String>) -> Self {
        self.interface = Some(name.into());
        self
    }

    pub fn broadcast(mut self, on: bool) -> Self {
        self.broadcast = on;
        self
    }

    pub fn reuse(mut self, on: bool) -> Self {
        self.reuse = on;
        self
    }
}

fn io_err(op: &'static str) -> impl FnOnce(io::Error) -> NetError {
    move |source| NetError::Io { op, source }
}

/// Create a nonblocking UDP socket as described by `opts` and register it
/// with tokio. Must be called from within a tokio runtime.
pub fn bind_udp(opts: &BindOptions) -> Result<UdpSocket, NetError> {
    let socket =
        Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(io_err("socket"))?;
    if opts.reuse {
        socket
            .set_reuse_address(true)
            .map_err(io_err("SO_REUSEADDR"))?;
        socket
            .set_reuse_port(true)
            .map_err(io_err("SO_REUSEPORT"))?;
    }
    if opts.broadcast {
        socket.set_broadcast(true).map_err(io_err("SO_BROADCAST"))?;
    }
    if let Some(name) = &opts.interface {
        let index = interface_index(name)?;
        socket
            .bind_device_by_index_v4(Some(index))
            .map_err(io_err("IP_BOUND_IF"))?;
    }
    socket
        .set_nonblocking(true)
        .map_err(io_err("set_nonblocking"))?;
    let addr = SocketAddr::V4(SocketAddrV4::new(opts.addr, opts.port));
    socket.bind(&addr.into()).map_err(|e| {
        if e.kind() == io::ErrorKind::AddrInUse {
            NetError::AddrInUse { port: opts.port }
        } else {
            NetError::Io {
                op: "bind",
                source: e,
            }
        }
    })?;
    UdpSocket::from_std(socket.into()).map_err(io_err("tokio register"))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ni-net`
Expected: PASS — `test result: ok. 10 passed`.

- [ ] **Step 6: Lint**

Run: `cargo clippy -p ni-net --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/ni-net
git commit -m "feat(ni-net): interface-bound UDP sockets with IP_BOUND_IF" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 3: ni-bootp crate: BootArch and vendor classes

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/ni-bootp/Cargo.toml`, `crates/ni-bootp/src/lib.rs`, `crates/ni-bootp/src/arch.rs`

`BootArch` is the shared architecture identity (Plan 3's `ni-assets` will depend on `ni-bootp` for it). Matching is exact and case-sensitive, as RouterBOOT sends fixed strings.

- [ ] **Step 1: Add `crates/ni-bootp` to the workspace members**

```toml
[workspace]
resolver = "2"
members = [
    "crates/ni-proto",
    "crates/ni-sim",
    "crates/ni-net",
    "crates/ni-bootp",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "GPL-3.0-only"
rust-version = "1.80"

[workspace.dependencies]
bytes = "1"
thiserror = "2"
proptest = "1"
nix = { version = "0.31.3", default-features = false }
socket2 = "0.6.5"
tokio = "1.54.1"
ni-proto = { path = "crates/ni-proto" }
ni-net = { path = "crates/ni-net" }
```

- [ ] **Step 2: Create `crates/ni-bootp/Cargo.toml`**

```toml
[package]
name = "ni-bootp"
description = "Minimal BOOTP/DHCP server for RouterBOOT network boot"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
ni-proto.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["net", "sync"] }

[dev-dependencies]
ni-net.workspace = true
tokio = { workspace = true, features = ["macros", "rt", "time"] }
```

- [ ] **Step 3: Create `crates/ni-bootp/src/lib.rs`**

```rust
//! Minimal BOOTP/DHCP server for RouterBOOT network boot.

pub mod arch;

pub use arch::{BootArch, VendorClass};
```

- [ ] **Step 4: Write the failing tests — create `crates/ni-bootp/src/arch.rs` containing only the test module (vendor class mapping)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_routerboot_vendor_class() {
        let cases = [
            ("Mips_boot", BootArch::Mips),
            ("MMipsBoot", BootArch::MMips),
            ("Powerboot", BootArch::PowerPc),
            ("e500_boot", BootArch::E500),
            ("e500sboot", BootArch::E500),
            ("440__boot", BootArch::Ppc440),
            ("tile_boot", BootArch::Tile),
            ("ARM__boot", BootArch::Arm),
            ("ARM64__boot", BootArch::Arm64),
        ];
        for (class, arch) in cases {
            assert_eq!(
                BootArch::from_vendor_class(class),
                VendorClass::Supported(arch),
                "{class}"
            );
        }
    }

    #[test]
    fn x86_and_uefi_are_unsupported() {
        for class in [
            "X86_64__boot",
            "ARM64UEFI__boot",
            "PXEClient:Arch:00000:UNDI:002001",
        ] {
            assert_eq!(
                BootArch::from_vendor_class(class),
                VendorClass::Unsupported(class.to_string())
            );
        }
    }

    #[test]
    fn other_classes_are_unknown_and_matching_is_case_sensitive() {
        for class in ["MSFT 5.0", "android-dhcp-14", "mips_boot", ""] {
            assert_eq!(
                BootArch::from_vendor_class(class),
                VendorClass::Unknown(class.to_string())
            );
        }
    }

    #[test]
    fn identifiers_are_unique() {
        let mut names: Vec<&str> = BootArch::ALL.iter().map(|a| a.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), BootArch::ALL.len());
        assert_eq!(BootArch::Arm64.to_string(), "arm64");
    }
}
```

- [ ] **Step 5: Run tests to verify they fail**

Run: `cargo test -p ni-bootp`
Expected: FAIL — compile errors such as `error[E0432]: unresolved import`.

- [ ] **Step 6: Implement — insert this above the test module in `crates/ni-bootp/src/arch.rs` (the file is then this code followed by the tests from the previous step)**

```rust
//! RouterBOOT architectures, identified by the DHCP vendor class (option 60).
//!
//! [`BootArch`] is the shared architecture identity for later crates
//! (`ni-assets` picks the boot kernel by it).

use std::fmt;

/// Architectures whose RouterBOOT can network-boot a Netinstall kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BootArch {
    /// `Mips_boot`: MIPS big-endian (mipsbe).
    Mips,
    /// `MMipsBoot`: MIPS little-endian, MT7621 (mmips).
    MMips,
    /// `Powerboot`: generic PowerPC (e300 / mpc83xx).
    PowerPc,
    /// `e500_boot`, `e500sboot`: PowerPC e500.
    E500,
    /// `440__boot`: PowerPC 440.
    Ppc440,
    /// `tile_boot`: Tilera.
    Tile,
    /// `ARM__boot`: 32-bit ARM.
    Arm,
    /// `ARM64__boot`: AArch64.
    Arm64,
}

impl BootArch {
    pub const ALL: [BootArch; 8] = [
        BootArch::Mips,
        BootArch::MMips,
        BootArch::PowerPc,
        BootArch::E500,
        BootArch::Ppc440,
        BootArch::Tile,
        BootArch::Arm,
        BootArch::Arm64,
    ];

    /// Stable lowercase identifier for logs, events and file names.
    pub fn as_str(self) -> &'static str {
        match self {
            BootArch::Mips => "mips",
            BootArch::MMips => "mmips",
            BootArch::PowerPc => "powerpc",
            BootArch::E500 => "e500",
            BootArch::Ppc440 => "ppc440",
            BootArch::Tile => "tile",
            BootArch::Arm => "arm",
            BootArch::Arm64 => "arm64",
        }
    }

    /// Classify a DHCP vendor class string (exact, case-sensitive match).
    pub fn from_vendor_class(class: &str) -> VendorClass {
        let arch = match class {
            "Mips_boot" => BootArch::Mips,
            "MMipsBoot" => BootArch::MMips,
            "Powerboot" => BootArch::PowerPc,
            "e500_boot" | "e500sboot" => BootArch::E500,
            "440__boot" => BootArch::Ppc440,
            "tile_boot" => BootArch::Tile,
            "ARM__boot" => BootArch::Arm,
            "ARM64__boot" => BootArch::Arm64,
            "X86_64__boot" | "ARM64UEFI__boot" => {
                return VendorClass::Unsupported(class.to_string())
            }
            c if c.starts_with("PXEClient") => return VendorClass::Unsupported(class.to_string()),
            _ => return VendorClass::Unknown(class.to_string()),
        };
        VendorClass::Supported(arch)
    }
}

impl fmt::Display for BootArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of classifying a vendor class string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorClass {
    /// A RouterBOOT architecture we can boot.
    Supported(BootArch),
    /// A MikroTik boot loader outside the MVP (x86 PXE, ARM64 UEFI).
    Unsupported(String),
    /// Not a known MikroTik vendor class (another DHCP client on the wire).
    Unknown(String),
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p ni-bootp`
Expected: PASS — `test result: ok. 4 passed`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock crates/ni-bootp
git commit -m "feat(ni-bootp): BootArch and vendor class mapping" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 4: ni-bootp: BOOTP/DHCP packet codec

**Files:**
- Create: `crates/ni-bootp/src/packet.rs`
- Modify: `crates/ni-bootp/src/lib.rs`

Plain BOOTP vs DHCP: a request is DHCP only if it has the magic cookie **and** option 53. Replies always carry the cookie (RFC 1048 vendor extensions) so the netmask can be sent. `sample_request` (test-only, in the implementation part) builds RouterBOOT-like requests and is reused by the responder tests in Task 6.

- [ ] **Step 1: Register the module in `crates/ni-bootp/src/lib.rs`**

```rust
//! Minimal BOOTP/DHCP server for RouterBOOT network boot.

pub mod arch;
pub mod packet;

pub use arch::{BootArch, VendorClass};
pub use packet::{MessageType, PacketError, ReplyKind, Request, CLIENT_PORT, SERVER_PORT};
```

- [ ] **Step 2: Write the failing tests — create `crates/ni-bootp/src/packet.rs` containing only the test module (parser and reply layout fixtures)**

```rust
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ni-bootp`
Expected: FAIL — compile errors such as `error[E0432]: unresolved import`.

- [ ] **Step 4: Implement — insert this above the test module in `crates/ni-bootp/src/packet.rs` (the file is then this code followed by the tests from the previous step)**

```rust
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
pub fn build_reply(req: &Request, p: &ReplyParams<'_>) -> Vec<u8> {
    let mut out = vec![0u8; HEADER_LEN];
    out[0] = OP_REPLY;
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&req.xid.to_be_bytes());
    out[10..12].copy_from_slice(&req.flags.to_be_bytes());
    out[12..16].copy_from_slice(&req.ciaddr.octets());
    out[16..20].copy_from_slice(&p.your_ip.octets());
    out[20..24].copy_from_slice(&p.server_ip.octets());
    out[24..28].copy_from_slice(&req.giaddr.octets());
    out[28..34].copy_from_slice(&req.chaddr.0);
    let file = p.file.as_bytes();
    let n = file.len().min(MAX_FILE_LEN);
    out[108..108 + n].copy_from_slice(&file[..n]);
    out.extend_from_slice(&MAGIC_COOKIE);
    let msg_type = match p.kind {
        ReplyKind::Bootp => None,
        ReplyKind::Offer => Some(MessageType::Offer),
        ReplyKind::Ack => Some(MessageType::Ack),
    };
    if let Some(t) = msg_type {
        out.extend_from_slice(&[opt::MESSAGE_TYPE, 1, t.to_u8()]);
        out.extend_from_slice(&[opt::SERVER_ID, 4]);
        out.extend_from_slice(&p.server_ip.octets());
        out.extend_from_slice(&[opt::LEASE_TIME, 4]);
        out.extend_from_slice(&p.lease_secs.to_be_bytes());
    }
    out.extend_from_slice(&[opt::SUBNET_MASK, 4]);
    out.extend_from_slice(&p.netmask.octets());
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
    b.push(opt::END);
    b.resize(MIN_LEN, 0);
    b
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ni-bootp`
Expected: PASS — `test result: ok. 20 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/ni-bootp
git commit -m "feat(ni-bootp): BOOTP/DHCP request parser and reply builder" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 5: ni-bootp: MAC-keyed lease pool

**Files:**
- Create: `crates/ni-bootp/src/pool.rs`
- Modify: `crates/ni-bootp/src/lib.rs`

- [ ] **Step 1: Register the module in `crates/ni-bootp/src/lib.rs`**

```rust
//! Minimal BOOTP/DHCP server for RouterBOOT network boot.

pub mod arch;
pub mod packet;
pub mod pool;

pub use arch::{BootArch, VendorClass};
pub use packet::{MessageType, PacketError, ReplyKind, Request, CLIENT_PORT, SERVER_PORT};
pub use pool::{LeasePool, PoolError};
```

- [ ] **Step 2: Write the failing tests — create `crates/ni-bootp/src/pool.rs` containing only the test module (lease allocation)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn mac(n: u8) -> Mac {
        Mac([2, 0, 0, 0, 0, n])
    }

    fn ip(last: u8) -> Ipv4Addr {
        Ipv4Addr::new(192, 168, 88, last)
    }

    #[test]
    fn same_mac_gets_same_ip() {
        let mut pool = LeasePool::new(ip(10), ip(20), ip(1)).unwrap();
        let a = pool.lease(mac(1)).unwrap();
        assert_eq!(a, ip(10));
        assert_eq!(pool.lease(mac(1)), Some(a));
        assert_eq!(pool.get(&mac(1)), Some(a));
        assert_eq!(pool.lease(mac(2)), Some(ip(11)));
    }

    #[test]
    fn skips_server_ip() {
        let mut pool = LeasePool::new(ip(1), ip(3), ip(2)).unwrap();
        assert_eq!(pool.lease(mac(1)), Some(ip(1)));
        assert_eq!(pool.lease(mac(2)), Some(ip(3)));
    }

    #[test]
    fn exhaustion_returns_none_but_known_macs_keep_their_lease() {
        let mut pool = LeasePool::new(ip(10), ip(11), ip(1)).unwrap();
        assert!(pool.lease(mac(1)).is_some());
        assert!(pool.lease(mac(2)).is_some());
        assert_eq!(pool.lease(mac(3)), None);
        assert_eq!(pool.get(&mac(3)), None);
        assert_eq!(pool.lease(mac(1)), Some(ip(10)));
    }

    #[test]
    fn single_address_pool_equal_to_server_is_always_empty() {
        let mut pool = LeasePool::new(ip(1), ip(1), ip(1)).unwrap();
        assert_eq!(pool.lease(mac(1)), None);
    }

    #[test]
    fn inverted_range_is_rejected() {
        assert_eq!(
            LeasePool::new(ip(20), ip(10), ip(1)).unwrap_err(),
            PoolError::Inverted {
                start: ip(20),
                end: ip(10)
            }
        );
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ni-bootp`
Expected: FAIL — compile errors such as `error[E0432]: unresolved import`.

- [ ] **Step 4: Implement — insert this above the test module in `crates/ni-bootp/src/pool.rs` (the file is then this code followed by the tests from the previous step)**

```rust
//! MAC-keyed IPv4 address pool.

use ni_proto::Mac;
use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PoolError {
    #[error("pool start {start} is after end {end}")]
    Inverted { start: Ipv4Addr, end: Ipv4Addr },
}

/// Hands out addresses from `start..=end`, never `exclude` (the server's
/// own IP). A MAC keeps its address for the pool's lifetime; leases never
/// expire (a Netinstall run is short).
#[derive(Debug, Clone)]
pub struct LeasePool {
    start: u32,
    end: u32,
    exclude: Ipv4Addr,
    leases: HashMap<Mac, Ipv4Addr>,
    used: HashSet<Ipv4Addr>,
}

impl LeasePool {
    pub fn new(start: Ipv4Addr, end: Ipv4Addr, exclude: Ipv4Addr) -> Result<Self, PoolError> {
        if u32::from(start) > u32::from(end) {
            return Err(PoolError::Inverted { start, end });
        }
        Ok(LeasePool {
            start: start.into(),
            end: end.into(),
            exclude,
            leases: HashMap::new(),
            used: HashSet::new(),
        })
    }

    /// Address for `mac`: its existing lease, else the lowest free address.
    /// `None` when the pool is exhausted.
    pub fn lease(&mut self, mac: Mac) -> Option<Ipv4Addr> {
        if let Some(&ip) = self.leases.get(&mac) {
            return Some(ip);
        }
        let ip = (self.start..=self.end)
            .map(Ipv4Addr::from)
            .find(|ip| *ip != self.exclude && !self.used.contains(ip))?;
        self.used.insert(ip);
        self.leases.insert(mac, ip);
        Some(ip)
    }

    /// Existing lease for `mac`, without allocating.
    pub fn get(&self, mac: &Mac) -> Option<Ipv4Addr> {
        self.leases.get(mac).copied()
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ni-bootp`
Expected: PASS — `test result: ok. 25 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/ni-bootp
git commit -m "feat(ni-bootp): MAC-keyed lease pool" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 6: ni-bootp: sans-IO responder

**Files:**
- Create: `crates/ni-bootp/src/responder.rs`
- Modify: `crates/ni-bootp/src/lib.rs`

Decision order: parse → message type (DISCOVER→OFFER, REQUEST→ACK unless option 54 names another server, other DHCP types ignored; no option 53 → plain BOOTP) → vendor class → boot file → lease. Nothing is leased for ignored requests. A DHCPREQUEST is ACKed with the MAC's lease even if it asks for a different address (option 50 is not checked; RouterBOOT takes what it is given).

- [ ] **Step 1: Register the module in `crates/ni-bootp/src/lib.rs`**

```rust
//! Minimal BOOTP/DHCP server for RouterBOOT network boot.
//!
//! RouterBOOT sends a BOOTP or DHCP request with its architecture in the
//! vendor class (option 60). The reply assigns an address from a MAC-keyed
//! pool and names the boot kernel to fetch over TFTP from `siaddr`.

pub mod arch;
pub mod packet;
pub mod pool;
pub mod responder;

pub use arch::{BootArch, VendorClass};
pub use packet::{MessageType, PacketError, ReplyKind, Request, CLIENT_PORT, SERVER_PORT};
pub use pool::{LeasePool, PoolError};
pub use responder::{BootpConfig, BootpResponder, ConfigError, IgnoreReason, Outcome, Reply};
```

- [ ] **Step 2: Write the failing tests — create `crates/ni-bootp/src/responder.rs` containing only the test module (responder decisions)**

```rust
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ni-bootp`
Expected: FAIL — compile errors such as `error[E0432]: unresolved import`.

- [ ] **Step 4: Implement — insert this above the test module in `crates/ni-bootp/src/responder.rs` (the file is then this code followed by the tests from the previous step)**

```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ni-bootp`
Expected: PASS — `test result: ok. 36 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/ni-bootp
git commit -m "feat(ni-bootp): sans-IO BOOTP/DHCP responder" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 7: ni-bootp: tokio runner and loopback tests

**Files:**
- Create: `crates/ni-bootp/src/server.rs`, `crates/ni-bootp/tests/loopback.rs`
- Modify: `crates/ni-bootp/src/lib.rs`

Replies go to an injected `reply_to` address: `255.255.255.255:68` in production, the test client's address in tests (`lo0` cannot broadcast: `sendto 255.255.255.255` fails with `ENETUNREACH`). The last test uses the production socket shape from `ni-net` (`0.0.0.0`, `SO_BROADCAST`, `IP_BOUND_IF` on `lo0`).

- [ ] **Step 1: Write the failing integration test `crates/ni-bootp/tests/loopback.rs`**

```rust
//! BOOTP server over real UDP sockets on 127.0.0.1.

use ni_bootp::{serve, BootArch, BootpConfig, BootpEvent, BootpResponder, IgnoreReason, ReplyKind};
use ni_proto::Mac;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 0x11, 0x22, 0x33]);

/// A RouterBOOT-like request: DHCP if `msg_type` is set, else plain BOOTP.
fn request(msg_type: Option<u8>, vendor_class: &str) -> Vec<u8> {
    let mut b = vec![0u8; 236];
    b[0] = 1;
    b[1] = 1;
    b[2] = 6;
    b[4..8].copy_from_slice(&0x1234_5678_u32.to_be_bytes());
    b[28..34].copy_from_slice(&MAC.0);
    b.extend_from_slice(&[99, 130, 83, 99]);
    if let Some(t) = msg_type {
        b.extend_from_slice(&[53, 1, t]);
    }
    b.push(60);
    b.push(vendor_class.len() as u8);
    b.extend_from_slice(vendor_class.as_bytes());
    b.push(255);
    b.resize(300, 0);
    b
}

struct Fixture {
    client: UdpSocket,
    server_addr: SocketAddr,
    events: mpsc::UnboundedReceiver<BootpEvent>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

async fn start() -> Fixture {
    start_with(UdpSocket::bind("127.0.0.1:0").await.unwrap()).await
}

async fn start_with(server: UdpSocket) -> Fixture {
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    let server_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let responder = BootpResponder::new(BootpConfig {
        server_ip: Ipv4Addr::new(192, 168, 88, 1),
        netmask: Ipv4Addr::new(255, 255, 255, 0),
        pool_start: Ipv4Addr::new(192, 168, 88, 100),
        pool_end: Ipv4Addr::new(192, 168, 88, 199),
        boot_files: HashMap::from([(BootArch::Arm64, "arm64-boot".to_string())]),
        lease_secs: 300,
    })
    .unwrap();
    let (tx, events) = mpsc::unbounded_channel();
    let reply_to = client.local_addr().unwrap();
    let task = tokio::spawn(serve(server, responder, reply_to, tx));
    Fixture {
        client,
        server_addr,
        events,
        task,
    }
}

async fn recv(sock: &UdpSocket) -> Vec<u8> {
    let mut buf = vec![0u8; 1500];
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
        .await
        .expect("reply within 2 s")
        .unwrap();
    buf.truncate(n);
    buf
}

async fn next_event(events: &mut mpsc::UnboundedReceiver<BootpEvent>) -> BootpEvent {
    tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("event within 2 s")
        .expect("server alive")
}

#[tokio::test]
async fn discover_then_request_over_udp() {
    let mut f = start().await;

    f.client
        .send_to(&request(Some(1), "ARM64__boot"), f.server_addr)
        .await
        .unwrap();
    let offer = recv(&f.client).await;
    assert_eq!(offer.len(), 300);
    assert_eq!(offer[0], 2);
    assert_eq!(&offer[4..8], &0x1234_5678_u32.to_be_bytes());
    assert_eq!(&offer[16..20], &[192, 168, 88, 100]);
    assert_eq!(&offer[20..24], &[192, 168, 88, 1]);
    assert_eq!(&offer[108..119], b"arm64-boot\0");
    assert_eq!(&offer[240..243], &[53, 1, 2]);
    assert_eq!(
        next_event(&mut f.events).await,
        BootpEvent::Replied {
            mac: MAC,
            arch: BootArch::Arm64,
            ip: Ipv4Addr::new(192, 168, 88, 100),
            kind: ReplyKind::Offer,
        }
    );

    f.client
        .send_to(&request(Some(3), "ARM64__boot"), f.server_addr)
        .await
        .unwrap();
    let ack = recv(&f.client).await;
    assert_eq!(&ack[16..20], &[192, 168, 88, 100]);
    assert_eq!(&ack[240..243], &[53, 1, 5]);
    f.task.abort();
}

#[tokio::test]
async fn plain_bootp_over_udp() {
    let mut f = start().await;
    f.client
        .send_to(&request(None, "ARM64__boot"), f.server_addr)
        .await
        .unwrap();
    let reply = recv(&f.client).await;
    assert_eq!(
        &reply[236..247],
        &[99, 130, 83, 99, 1, 4, 255, 255, 255, 0, 255]
    );
    assert!(matches!(
        next_event(&mut f.events).await,
        BootpEvent::Replied {
            kind: ReplyKind::Bootp,
            ..
        }
    ));
    f.task.abort();
}

#[tokio::test]
async fn ignored_and_malformed_requests_produce_events_only() {
    let mut f = start().await;
    f.client
        .send_to(&request(Some(1), "X86_64__boot"), f.server_addr)
        .await
        .unwrap();
    assert_eq!(
        next_event(&mut f.events).await,
        BootpEvent::Ignored {
            mac: MAC,
            reason: IgnoreReason::UnsupportedDevice("X86_64__boot".into()),
        }
    );
    f.client.send_to(b"junk", f.server_addr).await.unwrap();
    assert!(matches!(
        next_event(&mut f.events).await,
        BootpEvent::Malformed { .. }
    ));
    let mut buf = [0u8; 1500];
    let nothing =
        tokio::time::timeout(Duration::from_millis(200), f.client.recv_from(&mut buf)).await;
    assert!(nothing.is_err(), "no reply expected");
    f.task.abort();
}

/// Production socket shape: `0.0.0.0`, `SO_BROADCAST`, `IP_BOUND_IF` (here lo0).
#[tokio::test]
async fn works_on_interface_bound_wildcard_socket() {
    let opts = ni_net::BindOptions::new(0).interface("lo0").broadcast(true);
    let server = ni_net::bind_udp(&opts).unwrap();
    let mut f = start_with(server).await;
    f.client
        .send_to(&request(Some(1), "ARM64__boot"), f.server_addr)
        .await
        .unwrap();
    let offer = recv(&f.client).await;
    assert_eq!(&offer[240..243], &[53, 1, 2]);
    assert!(matches!(
        next_event(&mut f.events).await,
        BootpEvent::Replied { .. }
    ));
    f.task.abort();
}
```

- [ ] **Step 2: Replace `crates/ni-bootp/src/lib.rs` with the final version**

```rust
//! Minimal BOOTP/DHCP server for RouterBOOT network boot.
//!
//! RouterBOOT sends a BOOTP or DHCP request with its architecture in the
//! vendor class (option 60). The reply assigns an address from a MAC-keyed
//! pool and names the boot kernel to fetch over TFTP from `siaddr`.
//!
//! [`BootpResponder`] is sans-IO; [`serve`] runs it on a tokio socket.

pub mod arch;
pub mod packet;
pub mod pool;
pub mod responder;
pub mod server;

pub use arch::{BootArch, VendorClass};
pub use packet::{MessageType, PacketError, ReplyKind, Request, CLIENT_PORT, SERVER_PORT};
pub use pool::{LeasePool, PoolError};
pub use responder::{BootpConfig, BootpResponder, ConfigError, IgnoreReason, Outcome, Reply};
pub use server::{serve, BootpEvent};
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p ni-bootp --test loopback`
Expected: FAIL — compile errors such as `` file not found for module `server` ``.

- [ ] **Step 4: Implement `crates/ni-bootp/src/server.rs`**

```rust
//! tokio runner for [`BootpResponder`].

use crate::arch::BootArch;
use crate::packet::{PacketError, ReplyKind};
use crate::responder::{BootpResponder, IgnoreReason, Outcome};
use ni_proto::Mac;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;

/// What the server did with one datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootpEvent {
    Replied {
        mac: Mac,
        arch: BootArch,
        ip: Ipv4Addr,
        kind: ReplyKind,
    },
    Ignored {
        mac: Mac,
        reason: IgnoreReason,
    },
    Malformed {
        from: SocketAddr,
        error: PacketError,
    },
    /// Sending a reply failed (e.g. the interface went down).
    SendFailed {
        mac: Mac,
        error: String,
    },
}

/// Answer requests arriving on `socket` until receiving fails.
///
/// Replies go to `reply_to`: `255.255.255.255:68` in production (the
/// client has no address yet, and unicasting to `yiaddr` would need an ARP
/// entry we cannot add without root; `socket` then needs `SO_BROADCAST`
/// and `IP_BOUND_IF`). Tests pass a loopback address. Events are dropped
/// silently if the receiver is gone; abort the task to stop the server.
pub async fn serve(
    socket: UdpSocket,
    mut responder: BootpResponder,
    reply_to: SocketAddr,
    events: UnboundedSender<BootpEvent>,
) -> io::Result<()> {
    let mut buf = vec![0u8; 1500];
    loop {
        let (n, from) = socket.recv_from(&mut buf).await?;
        let event = match responder.handle(&buf[..n]) {
            Ok(Outcome::Reply(reply)) => match socket.send_to(&reply.bytes, reply_to).await {
                Ok(_) => BootpEvent::Replied {
                    mac: reply.client_mac,
                    arch: reply.arch,
                    ip: reply.assigned_ip,
                    kind: reply.kind,
                },
                Err(e) => BootpEvent::SendFailed {
                    mac: reply.client_mac,
                    error: e.to_string(),
                },
            },
            Ok(Outcome::Ignored { mac, reason }) => BootpEvent::Ignored { mac, reason },
            Err(error) => BootpEvent::Malformed { from, error },
        };
        let _ = events.send(event);
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ni-bootp`
Expected: PASS — `test result: ok. 36 passed` and `test result: ok. 4 passed`.

- [ ] **Step 6: Lint**

Run: `cargo clippy -p ni-bootp --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/ni-bootp
git commit -m "feat(ni-bootp): tokio runner with loopback tests" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 8: ni-tftp crate: packet codec

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/ni-tftp/Cargo.toml`, `crates/ni-tftp/src/lib.rs`, `crates/ni-tftp/src/packet.rs`

Option names and the mode are lowercased while parsing (RFC 2347: case-insensitive). ERROR packets without a terminating NUL are accepted (lenient), everything else must be terminated.

- [ ] **Step 1: Add `crates/ni-tftp` to the workspace members (final workspace file)**

```toml
[workspace]
resolver = "2"
members = [
    "crates/ni-proto",
    "crates/ni-sim",
    "crates/ni-net",
    "crates/ni-bootp",
    "crates/ni-tftp",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "GPL-3.0-only"
rust-version = "1.80"

[workspace.dependencies]
bytes = "1"
thiserror = "2"
proptest = "1"
nix = { version = "0.31.3", default-features = false }
socket2 = "0.6.5"
tokio = "1.54.1"
ni-proto = { path = "crates/ni-proto" }
ni-net = { path = "crates/ni-net" }
```

- [ ] **Step 2: Create `crates/ni-tftp/Cargo.toml`**

```toml
[package]
name = "ni-tftp"
description = "Minimal read-only TFTP server for RouterBOOT boot kernels"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
bytes.workspace = true
ni-net.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["net", "rt", "sync", "time", "macros"] }
```

- [ ] **Step 3: Create `crates/ni-tftp/src/lib.rs`**

```rust
//! Minimal read-only TFTP server (RFC 1350) for RouterBOOT boot kernels.

pub mod packet;

pub use packet::{ErrorCode, Packet, PacketError, Request, DEFAULT_BLKSIZE, PORT};
```

- [ ] **Step 4: Write the failing tests — create `crates/ni-tftp/src/packet.rs` containing only the test module (codec fixtures)**

```rust
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
```

- [ ] **Step 5: Run tests to verify they fail**

Run: `cargo test -p ni-tftp`
Expected: FAIL — compile errors such as `error[E0432]: unresolved import`.

- [ ] **Step 6: Implement — insert this above the test module in `crates/ni-tftp/src/packet.rs` (the file is then this code followed by the tests from the previous step)**

```rust
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
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p ni-tftp`
Expected: PASS — `test result: ok. 8 passed`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock crates/ni-tftp
git commit -m "feat(ni-tftp): TFTP packet codec" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 9: ni-tftp: sans-IO transfer state machine

**Files:**
- Create: `crates/ni-tftp/src/transfer.rs`
- Modify: `crates/ni-tftp/src/lib.rs`

Same shape as `ni_proto::ServerSession`: `poll_transmit` / `handle_packet` / `handle_timeout` / `poll_timeout`, time passed in. Blocks are counted as `u64` internally and wrapped to `u16` on the wire (65535 → 0). The wraparound test uses `blksize = 8` so 65 538 blocks stay fast.

- [ ] **Step 1: Register the module in `crates/ni-tftp/src/lib.rs`**

```rust
//! Minimal read-only TFTP server (RFC 1350) for RouterBOOT boot kernels.

pub mod packet;
pub mod transfer;

pub use packet::{ErrorCode, Packet, PacketError, Request, DEFAULT_BLKSIZE, PORT};
pub use transfer::{Status, Transfer, TransferError};
```

- [ ] **Step 2: Write the failing tests — create `crates/ni-tftp/src/transfer.rs` containing only the test module (lock-step transfer, wraparound, retransmit)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(1);

    fn data(len: usize) -> Bytes {
        Bytes::from((0..len).map(|i| (i % 251) as u8).collect::<Vec<u8>>())
    }

    fn ack(n: u16) -> Vec<u8> {
        Packet::Ack(n).encode()
    }

    /// Drive a transfer to completion with a perfect client; returns the
    /// DATA packets (block, payload length) and the reassembled file.
    fn run(mut t: Transfer, now: Instant) -> (Vec<(u16, usize)>, Vec<u8>) {
        let mut seen = Vec::new();
        let mut file = Vec::new();
        while let Some(pkt) = t.poll_transmit(now) {
            match Packet::parse(&pkt).unwrap() {
                Packet::Data { block, data } => {
                    seen.push((block, data.len()));
                    file.extend_from_slice(&data);
                    t.handle_packet(&ack(block), now);
                }
                Packet::Oack(_) => t.handle_packet(&ack(0), now),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(t.status(), &Status::Done);
        assert_eq!(t.poll_timeout(), None);
        assert_eq!(t.acked_bytes(), t.size());
        (seen, file)
    }

    #[test]
    fn three_and_a_bit_blocks() {
        let now = Instant::now();
        let t = Transfer::new("f".into(), data(1600), 512, T, 3, None, now);
        let (seen, file) = run(t, now);
        assert_eq!(seen, vec![(1, 512), (2, 512), (3, 512), (4, 64)]);
        assert_eq!(file, data(1600).to_vec());
    }

    #[test]
    fn exact_multiple_ends_with_empty_block() {
        let now = Instant::now();
        let t = Transfer::new("f".into(), data(1024), 512, T, 3, None, now);
        let (seen, _) = run(t, now);
        assert_eq!(seen, vec![(1, 512), (2, 512), (3, 0)]);
    }

    #[test]
    fn empty_file_is_one_empty_block() {
        let now = Instant::now();
        let t = Transfer::new("f".into(), Bytes::new(), 512, T, 3, None, now);
        let (seen, _) = run(t, now);
        assert_eq!(seen, vec![(1, 0)]);
    }

    #[test]
    fn oack_first_then_data_after_ack_0() {
        let now = Instant::now();
        let opts = vec![("tsize".to_string(), "10".to_string())];
        let mut t = Transfer::new("f".into(), data(10), 512, T, 3, Some(opts), now);
        let first = t.poll_transmit(now).unwrap();
        assert_eq!(first, b"\x00\x06tsize\x0010\x00");
        assert_eq!(t.poll_transmit(now), None, "waits for ACK 0");
        t.handle_packet(&ack(1), now);
        assert_eq!(t.poll_transmit(now), None, "wrong ACK ignored");
        t.handle_packet(&ack(0), now);
        let second = t.poll_transmit(now).unwrap();
        assert_eq!(&second[..4], b"\x00\x03\x00\x01");
        assert_eq!(second.len(), 4 + 10);
    }

    #[test]
    fn block_numbers_wrap_after_65535() {
        let now = Instant::now();
        let len = 8 * 65_537 + 3;
        let t = Transfer::new("f".into(), data(len), 8, T, 3, None, now);
        let (seen, file) = run(t, now);
        assert_eq!(seen.len(), 65_538);
        assert_eq!(seen[65_534], (65_535, 8));
        assert_eq!(seen[65_535], (0, 8));
        assert_eq!(seen[65_536], (1, 8));
        assert_eq!(seen[65_537], (2, 3));
        assert_eq!(file.len(), len);
    }

    #[test]
    fn retransmits_on_timeout_then_fails() {
        let t0 = Instant::now();
        let mut t = Transfer::new("f".into(), data(100), 512, T, 2, None, t0);
        let first = t.poll_transmit(t0).unwrap();
        assert_eq!(t.poll_timeout(), Some(t0 + T));
        t.handle_timeout(t0 + T / 2);
        assert_eq!(t.poll_transmit(t0 + T / 2), None, "not yet");
        let mut now = t0;
        for _ in 0..2 {
            now += T;
            t.handle_timeout(now);
            assert_eq!(t.poll_transmit(now).as_deref(), Some(first.as_slice()));
        }
        now += T;
        t.handle_timeout(now);
        assert_eq!(
            t.status(),
            &Status::Failed(TransferError::Timeout { retransmits: 2 })
        );
        assert_eq!(t.poll_transmit(now), None);
        assert_eq!(t.poll_timeout(), None);
    }

    #[test]
    fn retry_budget_resets_per_block() {
        let t0 = Instant::now();
        let mut t = Transfer::new("f".into(), data(600), 512, T, 1, None, t0);
        t.poll_transmit(t0).unwrap();
        t.handle_timeout(t0 + T);
        t.poll_transmit(t0 + T).unwrap();
        t.handle_packet(&ack(1), t0 + T);
        t.poll_transmit(t0 + T).unwrap();
        t.handle_timeout(t0 + 2 * T);
        assert!(
            t.poll_transmit(t0 + 2 * T).is_some(),
            "block 2 may retry once"
        );
        assert_eq!(t.status(), &Status::Running);
    }

    #[test]
    fn duplicate_ack_does_not_trigger_resend() {
        let now = Instant::now();
        let mut t = Transfer::new("f".into(), data(1600), 512, T, 3, None, now);
        t.poll_transmit(now).unwrap();
        t.handle_packet(&ack(1), now);
        t.poll_transmit(now).unwrap();
        t.handle_packet(&ack(1), now);
        assert_eq!(t.poll_transmit(now), None);
        assert_eq!(t.acked_bytes(), 512);
    }

    #[test]
    fn client_error_fails_transfer() {
        let now = Instant::now();
        let mut t = Transfer::new("f".into(), data(10), 512, T, 3, None, now);
        t.poll_transmit(now).unwrap();
        t.handle_packet(b"\x00\x05\x00\x08bad option\x00", now);
        assert_eq!(
            t.status(),
            &Status::Failed(TransferError::ClientError {
                code: 8,
                message: "bad option".into()
            })
        );
    }

    #[test]
    fn garbage_is_ignored_and_abort_fails() {
        let now = Instant::now();
        let mut t = Transfer::new("f".into(), data(10), 512, T, 3, None, now);
        t.poll_transmit(now).unwrap();
        t.handle_packet(b"\xff", now);
        assert_eq!(t.status(), &Status::Running);
        t.abort(TransferError::Io("gone".into()));
        assert_eq!(
            t.status(),
            &Status::Failed(TransferError::Io("gone".into()))
        );
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ni-tftp`
Expected: FAIL — compile errors such as `error[E0432]: unresolved import`.

- [ ] **Step 4: Implement — insert this above the test module in `crates/ni-tftp/src/transfer.rs` (the file is then this code followed by the tests from the previous step)**

```rust
//! Sans-IO state machine for one read transfer (server side).
//!
//! Lock-step per RFC 1350: send block `n`, wait for ACK `n`, send `n + 1`.
//! The last block is shorter than `blksize`; if the file size is a
//! multiple of `blksize` (including 0), a final empty block is sent.
//! Block numbers wrap from 65535 to 0. Duplicate ACKs are ignored (no
//! "Sorcerer's Apprentice" resends); lost packets are recovered by
//! retransmitting on timeout.

use crate::packet::{encode_data, Packet};
use bytes::Bytes;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Why a transfer failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransferError {
    #[error("no ACK after {retransmits} retransmits")]
    Timeout { retransmits: u32 },
    #[error("client sent error {code}: {message}")]
    ClientError { code: u16, message: String },
    #[error("socket error: {0}")]
    Io(String),
}

/// Current state of a [`Transfer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Running,
    Done,
    Failed(TransferError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// Send the current packet at `at`.
    Send {
        at: Instant,
    },
    /// Current packet sent; waiting for its ACK until `deadline`.
    Ack {
        deadline: Instant,
    },
    Finished,
}

/// One file being sent to one client.
#[derive(Debug)]
pub struct Transfer {
    file: String,
    data: Bytes,
    blksize: usize,
    timeout: Duration,
    max_retransmits: u32,
    /// OACK to send (and have acknowledged with ACK 0) before block 1.
    oack: Option<Vec<(String, String)>>,
    /// Block being sent: 0 = OACK, 1.. = data blocks (not wrapped).
    block: u64,
    last_block: u64,
    retries_left: u32,
    wait: Wait,
    status: Status,
}

impl Transfer {
    /// Start sending `data`. With `oack`, the OACK is sent first.
    pub fn new(
        file: String,
        data: Bytes,
        blksize: usize,
        timeout: Duration,
        max_retransmits: u32,
        oack: Option<Vec<(String, String)>>,
        now: Instant,
    ) -> Self {
        assert!(blksize > 0, "blksize must be positive");
        let last_block = (data.len() / blksize) as u64 + 1;
        let block = if oack.is_some() { 0 } else { 1 };
        Transfer {
            file,
            data,
            blksize,
            timeout,
            max_retransmits,
            oack,
            block,
            last_block,
            retries_left: max_retransmits,
            wait: Wait::Send { at: now },
            status: Status::Running,
        }
    }

    pub fn file(&self) -> &str {
        &self.file
    }

    pub fn size(&self) -> u64 {
        self.data.len() as u64
    }

    pub fn blksize(&self) -> usize {
        self.blksize
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn is_finished(&self) -> bool {
        self.status != Status::Running
    }

    /// Bytes the client has acknowledged so far.
    pub fn acked_bytes(&self) -> u64 {
        let acked_blocks = if self.status == Status::Done {
            self.last_block
        } else {
            self.block.saturating_sub(1)
        };
        (acked_blocks * self.blksize as u64).min(self.size())
    }

    /// Next datagram to send, if one is due at `now`.
    pub fn poll_transmit(&mut self, now: Instant) -> Option<Vec<u8>> {
        let Wait::Send { at } = self.wait else {
            return None;
        };
        if now < at {
            return None;
        }
        self.wait = Wait::Ack {
            deadline: now + self.timeout,
        };
        Some(self.current_packet())
    }

    /// Feed a datagram received from the client's address.
    pub fn handle_packet(&mut self, datagram: &[u8], now: Instant) {
        if self.is_finished() {
            return;
        }
        match Packet::parse(datagram) {
            Ok(Packet::Ack(n)) if n == wire_block(self.block) => {
                if !matches!(self.wait, Wait::Ack { .. }) {
                    return;
                }
                if self.block == self.last_block {
                    self.finish(Status::Done);
                } else {
                    self.block += 1;
                    self.retries_left = self.max_retransmits;
                    self.wait = Wait::Send { at: now };
                }
            }
            Ok(Packet::Error { code, message }) => {
                self.finish(Status::Failed(TransferError::ClientError {
                    code: code.to_u16(),
                    message,
                }));
            }
            _ => {}
        }
    }

    /// Call when `now >= poll_timeout()`.
    pub fn handle_timeout(&mut self, now: Instant) {
        let Wait::Ack { deadline } = self.wait else {
            return;
        };
        if now < deadline {
            return;
        }
        if self.retries_left > 0 {
            self.retries_left -= 1;
            self.wait = Wait::Send { at: now };
        } else {
            self.finish(Status::Failed(TransferError::Timeout {
                retransmits: self.max_retransmits,
            }));
        }
    }

    /// When the transfer next needs attention; `None` once finished.
    pub fn poll_timeout(&self) -> Option<Instant> {
        match self.wait {
            Wait::Send { at } => Some(at),
            Wait::Ack { deadline } => Some(deadline),
            Wait::Finished => None,
        }
    }

    /// Fail the transfer locally (e.g. the socket broke).
    pub fn abort(&mut self, error: TransferError) {
        if !self.is_finished() {
            self.finish(Status::Failed(error));
        }
    }

    fn finish(&mut self, status: Status) {
        self.status = status;
        self.wait = Wait::Finished;
    }

    fn current_packet(&self) -> Vec<u8> {
        if self.block == 0 {
            let opts = self.oack.clone().unwrap_or_default();
            return Packet::Oack(opts).encode();
        }
        let start = ((self.block - 1) as usize).saturating_mul(self.blksize);
        let start = start.min(self.data.len());
        let end = (start + self.blksize).min(self.data.len());
        encode_data(wire_block(self.block), &self.data[start..end])
    }
}

/// Block number as sent on the wire (wraps 65535 -> 0).
fn wire_block(block: u64) -> u16 {
    (block & 0xffff) as u16
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ni-tftp`
Expected: PASS — `test result: ok. 18 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/ni-tftp
git commit -m "feat(ni-tftp): sans-IO read transfer state machine" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 10: ni-tftp: request acceptance and option negotiation

**Files:**
- Create: `crates/ni-tftp/src/accept.rs`
- Modify: `crates/ni-tftp/src/lib.rs`

`blksize` is ignored unless `honor_blksize` is set (RouterBOOT quirk; pyNetinstall's dnsmasq config uses `tftp-no-blocksize`). `tsize` is always answered with the real size, `timeout` is honored by default. An OACK is sent only if at least one option was accepted; otherwise DATA 1 goes out directly (RFC 2347 fallback). Path check: strip one leading `/`, then any `/` or `..` is an access violation (error 2).

- [ ] **Step 1: Register the module in `crates/ni-tftp/src/lib.rs`**

```rust
//! Minimal read-only TFTP server (RFC 1350) for RouterBOOT boot kernels.

pub mod accept;
pub mod packet;
pub mod transfer;

pub use accept::{accept, Accept, FileMap, Rejection, TransferConfig};
pub use packet::{ErrorCode, Packet, PacketError, Request, DEFAULT_BLKSIZE, PORT};
pub use transfer::{Status, Transfer, TransferError};
```

- [ ] **Step 2: Write the failing tests — create `crates/ni-tftp/src/accept.rs` containing only the test module (request handling and option negotiation)**

```rust
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ni-tftp`
Expected: FAIL — compile errors such as `error[E0432]: unresolved import`.

- [ ] **Step 4: Implement — insert this above the test module in `crates/ni-tftp/src/accept.rs` (the file is then this code followed by the tests from the previous step)**

```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ni-tftp`
Expected: PASS — `test result: ok. 30 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/ni-tftp
git commit -m "feat(ni-tftp): accept read requests and negotiate options" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 11: ni-tftp: tokio runner and loopback tests

**Files:**
- Create: `crates/ni-tftp/src/server.rs`, `crates/ni-tftp/tests/loopback.rs`
- Modify: `crates/ni-tftp/src/lib.rs`

The listener answers rejections itself; each accepted RRQ gets a task with its own socket created from `TftpConfig::transfer_socket` (port 0, same interface as the listener), so DATA comes from a new transfer ID. Transfers live in a `JoinSet`, so aborting `serve` aborts them. A repeated RRQ from a peer with a running transfer is ignored; packets from strangers on a transfer port get error 5. The test file contains a minimal TFTP client.

- [ ] **Step 1: Write the failing integration test `crates/ni-tftp/tests/loopback.rs`**

```rust
//! TFTP server over real UDP sockets on lo0, with a minimal client.

use bytes::Bytes;
use ni_net::BindOptions;
use ni_tftp::{
    serve, ErrorCode, FileMap, Packet, Request, TftpConfig, TftpEvent, TransferConfig,
    TransferError,
};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

fn content(len: usize) -> Bytes {
    Bytes::from((0..len).map(|i| (i * 7 % 256) as u8).collect::<Vec<u8>>())
}

fn lo0() -> BindOptions {
    BindOptions::new(0)
        .addr(Ipv4Addr::LOCALHOST)
        .interface("lo0")
        .reuse(false)
}

struct Server {
    addr: SocketAddr,
    events: mpsc::UnboundedReceiver<TftpEvent>,
    task: JoinHandle<std::io::Result<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start(transfer: TransferConfig) -> Server {
    let files = FileMap::from([
        ("kernel".to_string(), content(1600)),
        ("exact".to_string(), content(1024)),
        ("empty".to_string(), Bytes::new()),
    ]);
    let listener = ni_net::bind_udp(&lo0()).unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, events) = mpsc::unbounded_channel();
    let cfg = TftpConfig {
        transfer,
        transfer_socket: lo0(),
    };
    let task = tokio::spawn(serve(listener, Arc::new(files), cfg, tx));
    Server { addr, events, task }
}

fn fast() -> TransferConfig {
    TransferConfig {
        timeout: Duration::from_millis(100),
        max_retransmits: 2,
        ..Default::default()
    }
}

fn rrq(name: &str, options: &[(&str, &str)]) -> Vec<u8> {
    Packet::Rrq(Request {
        filename: name.into(),
        mode: "octet".into(),
        options: options
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    })
    .encode()
}

async fn recv(sock: &UdpSocket) -> (Packet, SocketAddr) {
    let mut buf = vec![0u8; 70_000];
    let (n, from) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
        .await
        .expect("packet within 2 s")
        .unwrap();
    (Packet::parse(&buf[..n]).unwrap(), from)
}

/// What a download saw.
#[derive(Debug)]
struct Download {
    data: Vec<u8>,
    blocks: Vec<(u16, usize)>,
    oack: Option<Vec<(String, String)>>,
    tid: SocketAddr,
}

/// Minimal TFTP client: RRQ, optional OACK -> ACK 0, then ACK each block.
async fn download(
    server: SocketAddr,
    name: &str,
    options: &[(&str, &str)],
) -> Result<Download, (ErrorCode, String)> {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&rrq(name, options), server).await.unwrap();
    let mut dl = Download {
        data: Vec::new(),
        blocks: Vec::new(),
        oack: None,
        tid: server,
    };
    loop {
        let (pkt, from) = recv(&sock).await;
        dl.tid = from;
        match pkt {
            Packet::Oack(opts) => {
                dl.oack = Some(opts);
                sock.send_to(&Packet::Ack(0).encode(), from).await.unwrap();
            }
            Packet::Data { block, data } => {
                let last = data.len() < 512;
                dl.blocks.push((block, data.len()));
                dl.data.extend_from_slice(&data);
                sock.send_to(&Packet::Ack(block).encode(), from)
                    .await
                    .unwrap();
                if last {
                    return Ok(dl);
                }
            }
            Packet::Error { code, message } => return Err((code, message)),
            other => panic!("unexpected {other:?}"),
        }
    }
}

async fn next_event(s: &mut Server) -> TftpEvent {
    tokio::time::timeout(Duration::from_secs(2), s.events.recv())
        .await
        .expect("event within 2 s")
        .unwrap()
}

#[tokio::test]
async fn downloads_multi_block_file_from_new_tid() {
    let mut s = start(fast()).await;
    let dl = download(s.addr, "kernel", &[]).await.unwrap();
    assert_eq!(dl.data, content(1600).to_vec());
    assert_eq!(dl.blocks, vec![(1, 512), (2, 512), (3, 512), (4, 64)]);
    assert_eq!(dl.oack, None);
    assert_ne!(dl.tid, s.addr, "data comes from a per-transfer port");
    let peer = match next_event(&mut s).await {
        TftpEvent::Started { peer, file, size } => {
            assert_eq!((file.as_str(), size), ("kernel", 1600));
            peer
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(
        next_event(&mut s).await,
        TftpEvent::Completed {
            peer,
            file: "kernel".into(),
            size: 1600
        }
    );
}

#[tokio::test]
async fn exact_multiple_of_512_ends_with_empty_block() {
    let s = start(fast()).await;
    let dl = download(s.addr, "exact", &[]).await.unwrap();
    assert_eq!(dl.blocks, vec![(1, 512), (2, 512), (3, 0)]);
    assert_eq!(dl.data, content(1024).to_vec());
}

#[tokio::test]
async fn empty_file_is_one_empty_block() {
    let s = start(fast()).await;
    let dl = download(s.addr, "/empty", &[]).await.unwrap();
    assert_eq!(dl.blocks, vec![(1, 0)]);
}

#[tokio::test]
async fn missing_file_is_error_1() {
    let mut s = start(fast()).await;
    let err = download(s.addr, "nope", &[]).await.unwrap_err();
    assert_eq!(err, (ErrorCode::FileNotFound, "file not found".into()));
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Rejected {
            code: ErrorCode::FileNotFound,
            ..
        }
    ));
}

#[tokio::test]
async fn traversal_is_error_2() {
    let s = start(fast()).await;
    let err = download(s.addr, "../kernel", &[]).await.unwrap_err();
    assert_eq!(err.0, ErrorCode::AccessViolation);
}

#[tokio::test]
async fn write_request_is_error_4() {
    let s = start(fast()).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let wrq = Packet::Wrq(Request {
        filename: "kernel".into(),
        mode: "octet".into(),
        options: vec![],
    });
    sock.send_to(&wrq.encode(), s.addr).await.unwrap();
    let (pkt, _) = recv(&sock).await;
    assert_eq!(
        pkt,
        Packet::Error {
            code: ErrorCode::IllegalOperation,
            message: "illegal operation".into()
        }
    );
}

#[tokio::test]
async fn tsize_gets_oack_and_blksize_is_ignored() {
    let s = start(fast()).await;
    let dl = download(s.addr, "kernel", &[("tsize", "0"), ("blksize", "1468")])
        .await
        .unwrap();
    assert_eq!(dl.oack, Some(vec![("tsize".into(), "1600".into())]));
    assert_eq!(dl.blocks.len(), 4, "still 512-byte blocks");
    assert_eq!(dl.data, content(1600).to_vec());
}

#[tokio::test]
async fn lost_ack_is_recovered_by_retransmit() {
    let s = start(fast()).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    let (first, tid) = recv(&sock).await;
    // Drop our ACK for block 1: the server must resend block 1.
    let (again, _) = recv(&sock).await;
    assert_eq!(first, again);
    let mut data = Vec::new();
    let mut pkt = again;
    loop {
        let Packet::Data { block, data: chunk } = pkt else {
            panic!("{pkt:?}")
        };
        data.extend_from_slice(&chunk);
        sock.send_to(&Packet::Ack(block).encode(), tid)
            .await
            .unwrap();
        if chunk.len() < 512 {
            break;
        }
        pkt = recv(&sock).await.0;
    }
    assert_eq!(data, content(1600).to_vec());
}

#[tokio::test]
async fn silent_client_fails_with_timeout() {
    let mut s = start(fast()).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Started { .. }
    ));
    match next_event(&mut s).await {
        TftpEvent::Failed { error, .. } => {
            assert_eq!(error, TransferError::Timeout { retransmits: 2 })
        }
        other => panic!("{other:?}"),
    }
    // 1 original + 2 retransmits reached the client.
    for _ in 0..3 {
        assert!(matches!(recv(&sock).await.0, Packet::Data { block: 1, .. }));
    }
}

#[tokio::test]
async fn repeated_rrq_does_not_start_a_second_transfer() {
    let mut s = start(fast()).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    let (_, tid) = recv(&sock).await;
    sock.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    // Complete the one transfer.
    let mut block = 1u16;
    loop {
        sock.send_to(&Packet::Ack(block).encode(), tid)
            .await
            .unwrap();
        let (pkt, from) = recv(&sock).await;
        assert_eq!(from, tid, "everything comes from the first transfer");
        let Packet::Data { block: b, data } = pkt else {
            panic!()
        };
        if b == block {
            continue; // retransmit raced with our ACK
        }
        block = b;
        if data.len() < 512 {
            sock.send_to(&Packet::Ack(block).encode(), tid)
                .await
                .unwrap();
            break;
        }
    }
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Started { .. }
    ));
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Completed { .. }
    ));
}

#[tokio::test]
async fn foreign_packet_on_transfer_port_gets_error_5() {
    let s = start(fast()).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    let (_, tid) = recv(&client).await;
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    stranger
        .send_to(&Packet::Ack(1).encode(), tid)
        .await
        .unwrap();
    let (pkt, _) = recv(&stranger).await;
    assert!(matches!(
        pkt,
        Packet::Error {
            code: ErrorCode::UnknownTransferId,
            ..
        }
    ));
}

#[tokio::test]
async fn concurrent_downloads() {
    let s = start(fast()).await;
    let (a, b, c) = tokio::join!(
        download(s.addr, "kernel", &[]),
        download(s.addr, "exact", &[]),
        download(s.addr, "kernel", &[("tsize", "0")]),
    );
    assert_eq!(a.unwrap().data, content(1600).to_vec());
    assert_eq!(b.unwrap().data, content(1024).to_vec());
    assert_eq!(c.unwrap().data, content(1600).to_vec());
}
```

- [ ] **Step 2: Replace `crates/ni-tftp/src/lib.rs` with the final version**

```rust
//! Minimal read-only TFTP server (RFC 1350) for RouterBOOT boot kernels.
//!
//! Supports RRQ in `octet` and `netascii` (served as octet) modes and the
//! `tsize`, `timeout` and (opt-in) `blksize` options (RFC 2347-2349).
//! [`accept()`] and [`Transfer`] are sans-IO; [`serve`] runs them on tokio.

pub mod accept;
pub mod packet;
pub mod server;
pub mod transfer;

pub use accept::{accept, Accept, FileMap, Rejection, TransferConfig};
pub use packet::{ErrorCode, Packet, PacketError, Request, DEFAULT_BLKSIZE, PORT};
pub use server::{serve, TftpConfig, TftpEvent};
pub use transfer::{Status, Transfer, TransferError};
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p ni-tftp --test loopback`
Expected: FAIL — compile errors such as `` file not found for module `server` ``.

- [ ] **Step 4: Implement `crates/ni-tftp/src/server.rs`**

```rust
//! tokio runner: a listener plus one task (and ephemeral socket) per transfer.

use crate::accept::{accept, Accept, FileMap, TransferConfig};
use crate::packet::{ErrorCode, Packet};
use crate::transfer::{Status, Transfer, TransferError};
use ni_net::{bind_udp, BindOptions};
use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

/// Runner settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TftpConfig {
    pub transfer: TransferConfig,
    /// Template for each transfer's socket. Use port 0 and the listener's
    /// interface, e.g. `BindOptions::new(0).interface("en7").reuse(false)`.
    pub transfer_socket: BindOptions,
}

/// Lifecycle of transfers, for logging and progress tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TftpEvent {
    Started {
        peer: SocketAddr,
        file: String,
        size: u64,
    },
    Completed {
        peer: SocketAddr,
        file: String,
        size: u64,
    },
    Failed {
        peer: SocketAddr,
        file: String,
        error: TransferError,
    },
    /// Request answered with an ERROR packet.
    Rejected {
        peer: SocketAddr,
        file: Option<String>,
        code: ErrorCode,
    },
}

/// Serve read requests arriving on `listener` until receiving fails.
///
/// Each accepted RRQ runs as its own task with its own socket (a new
/// transfer ID, as RFC 1350 requires). A repeated RRQ from a peer whose
/// transfer is still running is ignored. Aborting this task aborts all
/// running transfers.
pub async fn serve(
    listener: UdpSocket,
    files: Arc<FileMap>,
    cfg: TftpConfig,
    events: UnboundedSender<TftpEvent>,
) -> io::Result<()> {
    let mut tasks: JoinSet<SocketAddr> = JoinSet::new();
    let mut active: HashSet<SocketAddr> = HashSet::new();
    let mut buf = vec![0u8; 2048];
    loop {
        tokio::select! {
            received = listener.recv_from(&mut buf) => {
                let (n, peer) = received?;
                if active.contains(&peer) {
                    continue;
                }
                match accept(&buf[..n], &files, &cfg.transfer, Instant::now()) {
                    Accept::Start(t) => {
                        let _ = events.send(TftpEvent::Started {
                            peer,
                            file: t.file().to_string(),
                            size: t.size(),
                        });
                        active.insert(peer);
                        tasks.spawn(run_transfer(
                            t,
                            peer,
                            cfg.transfer_socket.clone(),
                            events.clone(),
                        ));
                    }
                    Accept::Reject(r) => {
                        let _ = listener.send_to(&r.packet(), peer).await;
                        let _ = events.send(TftpEvent::Rejected {
                            peer,
                            file: r.file,
                            code: r.code,
                        });
                    }
                    Accept::Ignore => {}
                }
            }
            Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                if let Ok(peer) = done {
                    active.remove(&peer);
                }
            }
        }
    }
}

async fn run_transfer(
    mut t: Transfer,
    peer: SocketAddr,
    bind: BindOptions,
    events: UnboundedSender<TftpEvent>,
) -> SocketAddr {
    match bind_udp(&bind) {
        Ok(sock) => drive(&mut t, &sock, peer).await,
        Err(e) => t.abort(TransferError::Io(e.to_string())),
    }
    let file = t.file().to_string();
    let event = match t.status() {
        Status::Done => TftpEvent::Completed {
            peer,
            file,
            size: t.size(),
        },
        Status::Failed(error) => TftpEvent::Failed {
            peer,
            file,
            error: error.clone(),
        },
        Status::Running => unreachable!("drive returns only when finished"),
    };
    let _ = events.send(event);
    peer
}

async fn drive(t: &mut Transfer, sock: &UdpSocket, peer: SocketAddr) {
    let unknown_tid = Packet::Error {
        code: ErrorCode::UnknownTransferId,
        message: "unknown transfer id".into(),
    }
    .encode();
    let mut buf = vec![0u8; 2048];
    loop {
        let now = Instant::now();
        while let Some(pkt) = t.poll_transmit(now) {
            if let Err(e) = sock.send_to(&pkt, peer).await {
                t.abort(TransferError::Io(e.to_string()));
            }
        }
        let Some(deadline) = t.poll_timeout() else {
            return;
        };
        tokio::select! {
            received = sock.recv_from(&mut buf) => match received {
                Ok((n, from)) if from == peer => t.handle_packet(&buf[..n], Instant::now()),
                Ok((_, from)) => {
                    let _ = sock.send_to(&unknown_tid, from).await;
                }
                Err(e) => t.abort(TransferError::Io(e.to_string())),
            },
            _ = tokio::time::sleep_until(deadline.into()) => t.handle_timeout(Instant::now()),
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ni-tftp`
Expected: PASS — `test result: ok. 30 passed` and `test result: ok. 12 passed`.

- [ ] **Step 6: Lint**

Run: `cargo clippy -p ni-tftp --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/ni-tftp
git commit -m "feat(ni-tftp): tokio runner with per-transfer sockets and loopback tests" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```


---

### Task 12: Final verification and roadmap notes

**Files:**
- Modify: `docs/superpowers/plans/2026-11-06-00-roadmap.md`

- [ ] **Step 1: Format check**

Run: `cargo fmt --all --check`
Expected: no output, exit code 0. If it fails, run `cargo fmt --all`, re-run the tests and commit.

- [ ] **Step 2: Lint the workspace**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

- [ ] **Step 3: Docs build without broken links**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
Expected: `Finished`, no warnings.

- [ ] **Step 4: Full test suite**

Run: `cargo test --workspace`
Expected: every suite `ok`, 148 tests in total:

- `ni-bootp`: 36 (unit) + 4 (tests/loopback.rs)
- `ni-net`: 10 (unit)
- `ni-proto`: 28 (unit) + 2 (tests/frame_props.rs)
- `ni-sim`: 12 (unit) + 14 (tests/e2e.rs)
- `ni-tftp`: 30 (unit) + 12 (tests/loopback.rs)

- [ ] **Step 5: Record Plan 2 facts in the roadmap — in `docs/superpowers/plans/2026-11-06-00-roadmap.md`, insert this section directly above the line `## Plan 3 outline — Assets`**

```markdown
## Facts established during Plan 2 (2026-11-06)

- Unprivileged processes can bind UDP 67, 69 and 5000 on macOS; no root needed for the servers.
- `IP_BOUND_IF` is `socket2::Socket::bind_device_by_index_v4` (socket2 0.6.5, feature `all`).
  A wildcard (`0.0.0.0`) socket bound to `lo0` still receives unicast to 127.0.0.1.
- `lo0` cannot send to `255.255.255.255` (`ENETUNREACH`), so broadcast replies cannot be
  tested on loopback: `ni_bootp::serve` takes the reply destination as a parameter.
- nix 0.31 `LinkAddr::addr()` does not check the address length; `ni-net` checks `alen() == 6`.
- No `Transport` trait yet: the runners take a `tokio::net::UdpSocket`. The engine (Plan 4)
  owns sockets; a trait is added only if a BPF transport is ever needed.

## Open hardware checks (from Plan 2)

- Broadcast BOOTP reply out of an `IP_BOUND_IF` socket reaches the board (and only via that port).
- RouterBOOT TFTP client: which options it sends (`tsize`? `blksize`?), and that it boots with
  `honor_blksize = false` (default). Also check whether it retransmits RRQ.
- Whether RouterBOOT sends DHCP (option 53) or plain BOOTP, per arch; capture to pcap fixtures.
```

- [ ] **Step 6: Commit**

```bash
git add docs/superpowers/plans/2026-11-06-00-roadmap.md
git commit -m "docs: record Plan 2 findings and hardware checks" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

