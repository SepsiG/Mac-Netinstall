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
use std::time::{Duration, Instant};
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
    /// Set `SO_REUSEADDR` and `SO_REUSEPORT`. **Off by default, and keep it
    /// off for the listeners on :67, :69 and :5000**: on BSD a second bind
    /// to a port succeeds when *both* sockets set `SO_REUSEPORT`, so a
    /// listener with reuse on would silently share the port with another
    /// server that also uses it (and split its traffic). Without reuse,
    /// any existing holder of the port, with or without `SO_REUSEPORT`,
    /// is reported as [`NetError::AddrInUse`]. Per-transfer TFTP sockets
    /// (ephemeral ports) do not need reuse either.
    pub reuse: bool,
}

impl BindOptions {
    /// `0.0.0.0:port`, no interface, no broadcast, reuse **off**.
    pub fn new(port: u16) -> Self {
        BindOptions {
            addr: Ipv4Addr::UNSPECIFIED,
            port,
            interface: None,
            broadcast: false,
            reuse: false,
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

/// Receive-error policy shared by the BOOTP and TFTP runners: `true` if a
/// failed `recv_from` should be reported and the loop continued, `false`
/// if the socket is unusable and the server should stop.
///
/// Transient (keep serving):
/// - `WouldBlock`, `Interrupted`: spurious wakeups / signals;
/// - `ConnectionReset`, `ConnectionRefused`, `EHOSTUNREACH`,
///   `ENETUNREACH`, `EHOSTDOWN`: an ICMP error for an earlier send;
/// - `ENETDOWN`: the link flapped (cable replugged during a reboot);
/// - `ENOBUFS`, `ENOMEM`: momentary kernel buffer shortage.
///
/// Everything else (e.g. `EBADF`, `ENOTSOCK`, `EINVAL`) is fatal.
pub fn is_transient_recv_error(e: &io::Error) -> bool {
    use io::ErrorKind::*;
    if matches!(
        e.kind(),
        WouldBlock | Interrupted | ConnectionReset | ConnectionRefused
    ) {
        return true;
    }
    matches!(
        e.raw_os_error(),
        Some(
            nix::libc::ENOBUFS
                | nix::libc::ENOMEM
                | nix::libc::EHOSTUNREACH
                | nix::libc::ENETUNREACH
                | nix::libc::EHOSTDOWN
                | nix::libc::ENETDOWN
        )
    )
}

/// What a receive loop should do after a failed `recv_from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvErrorAction {
    /// Return the error: the socket is unusable.
    Fatal,
    /// Sleep [`RecvErrorGuard::BACKOFF`], then receive again. With
    /// `report: Some(n)`, emit one event covering the `n` errors since
    /// the last report (this one included).
    Retry { report: Option<u32> },
}

/// Receive-error bookkeeping shared by the BOOTP and TFTP runners.
///
/// Policy:
/// - errors that are not [`is_transient_recv_error`] are fatal at once;
/// - transient errors back off [`Self::BACKOFF`] before the next receive,
///   so a persistent error cannot spin the loop;
/// - [`Self::MAX_CONSECUTIVE`] transient errors in a row (no successful
///   receive in between, about 2 s of backoff) are treated as fatal;
/// - reports are coalesced: the first error is reported, then at most
///   one report per [`Self::REPORT_INTERVAL`], carrying the count of
///   errors since the previous report.
#[derive(Debug, Clone, Default)]
pub struct RecvErrorGuard {
    consecutive: u32,
    unreported: u32,
    last_report: Option<Instant>,
}

impl RecvErrorGuard {
    /// Pause after a transient error.
    pub const BACKOFF: Duration = Duration::from_millis(20);
    /// Transient errors in a row that end the server.
    pub const MAX_CONSECUTIVE: u32 = 100;
    /// Minimum time between two reports.
    pub const REPORT_INTERVAL: Duration = Duration::from_secs(1);

    /// Classify a failed receive at `now`.
    pub fn on_error(&mut self, e: &io::Error, now: Instant) -> RecvErrorAction {
        if !is_transient_recv_error(e) {
            return RecvErrorAction::Fatal;
        }
        self.consecutive += 1;
        if self.consecutive >= Self::MAX_CONSECUTIVE {
            return RecvErrorAction::Fatal;
        }
        self.unreported = self.unreported.saturating_add(1);
        let due = match self.last_report {
            None => true,
            Some(t) => now.saturating_duration_since(t) >= Self::REPORT_INTERVAL,
        };
        if !due {
            return RecvErrorAction::Retry { report: None };
        }
        self.last_report = Some(now);
        let n = std::mem::take(&mut self.unreported);
        RecvErrorAction::Retry { report: Some(n) }
    }

    /// A receive succeeded: the consecutive-error count starts over.
    pub fn on_success(&mut self) {
        self.consecutive = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let holder = bind_udp(&lo0(0)).unwrap();
        let port = holder.local_addr().unwrap().port();
        let err = bind_udp(&lo0(port)).unwrap_err();
        assert!(
            matches!(err, NetError::AddrInUse { port: p } if p == port),
            "{err:?}"
        );
    }

    #[test]
    fn reuse_is_off_by_default() {
        assert!(!BindOptions::new(67).reuse);
    }

    /// A server that set `SO_REUSEPORT` itself must still be detected:
    /// our default bind (no reuse) fails with `AddrInUse`.
    #[tokio::test]
    async fn port_held_with_so_reuseport_reports_addr_in_use() {
        let holder = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        holder.set_reuse_address(true).unwrap();
        holder.set_reuse_port(true).unwrap();
        let any = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
        holder.bind(&any.into()).unwrap();
        let port = holder.local_addr().unwrap().as_socket().unwrap().port();
        let err = bind_udp(&lo0(port)).unwrap_err();
        assert!(
            matches!(err, NetError::AddrInUse { port: p } if p == port),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn two_reuse_sockets_can_share_a_port() {
        let first = bind_udp(&lo0(0).reuse(true)).unwrap();
        let port = first.local_addr().unwrap().port();
        bind_udp(&lo0(port).reuse(true)).unwrap();
    }

    #[tokio::test]
    async fn unknown_interface_fails_before_bind() {
        let err = bind_udp(&BindOptions::new(0).interface("nosuch0")).unwrap_err();
        assert!(matches!(err, NetError::UnknownInterface(_)));
    }

    #[test]
    fn transient_recv_errors() {
        use io::ErrorKind::*;
        for kind in [WouldBlock, Interrupted, ConnectionReset, ConnectionRefused] {
            assert!(is_transient_recv_error(&io::Error::from(kind)), "{kind:?}");
        }
        for errno in [
            nix::libc::ENOBUFS,
            nix::libc::ENOMEM,
            nix::libc::EHOSTUNREACH,
            nix::libc::ENETUNREACH,
            nix::libc::EHOSTDOWN,
            nix::libc::ENETDOWN,
        ] {
            let e = io::Error::from_raw_os_error(errno);
            assert!(is_transient_recv_error(&e), "{e}");
        }
        for errno in [nix::libc::EBADF, nix::libc::ENOTSOCK, nix::libc::EINVAL] {
            let e = io::Error::from_raw_os_error(errno);
            assert!(!is_transient_recv_error(&e), "{e}");
        }
        assert!(!is_transient_recv_error(&io::Error::other("boom")));
    }

    fn enobufs() -> io::Error {
        io::Error::from_raw_os_error(nix::libc::ENOBUFS)
    }

    #[test]
    fn guard_reports_first_error_then_at_most_once_per_second() {
        let t0 = Instant::now();
        let mut g = RecvErrorGuard::default();
        assert_eq!(
            g.on_error(&enobufs(), t0),
            RecvErrorAction::Retry { report: Some(1) }
        );
        for i in 1..10 {
            let now = t0 + Duration::from_millis(20 * i);
            assert_eq!(
                g.on_error(&enobufs(), now),
                RecvErrorAction::Retry { report: None }
            );
        }
        g.on_success();
        // Still within the second: suppressed, but counted.
        assert_eq!(
            g.on_error(&enobufs(), t0 + Duration::from_millis(500)),
            RecvErrorAction::Retry { report: None }
        );
        // A second later: one report covering the 10 suppressed errors too.
        assert_eq!(
            g.on_error(&enobufs(), t0 + Duration::from_millis(1000)),
            RecvErrorAction::Retry { report: Some(11) }
        );
    }

    #[test]
    fn guard_gives_up_after_max_consecutive_transient_errors() {
        let t0 = Instant::now();
        let mut g = RecvErrorGuard::default();
        for _ in 1..RecvErrorGuard::MAX_CONSECUTIVE {
            assert!(matches!(
                g.on_error(&enobufs(), t0),
                RecvErrorAction::Retry { .. }
            ));
        }
        assert_eq!(g.on_error(&enobufs(), t0), RecvErrorAction::Fatal);
    }

    #[test]
    fn guard_success_resets_the_consecutive_count() {
        let t0 = Instant::now();
        let mut g = RecvErrorGuard::default();
        for _ in 0..3 {
            for _ in 1..RecvErrorGuard::MAX_CONSECUTIVE {
                assert_ne!(g.on_error(&enobufs(), t0), RecvErrorAction::Fatal);
            }
            g.on_success();
        }
    }

    #[test]
    fn guard_treats_non_transient_errors_as_fatal_immediately() {
        let mut g = RecvErrorGuard::default();
        let e = io::Error::from_raw_os_error(nix::libc::EBADF);
        assert_eq!(g.on_error(&e, Instant::now()), RecvErrorAction::Fatal);
    }

    #[test]
    fn backoff_is_short() {
        assert_eq!(RecvErrorGuard::BACKOFF, Duration::from_millis(20));
    }

    #[tokio::test]
    async fn broadcast_flag_is_applied() {
        let sock = bind_udp(&lo0(0).broadcast(true)).unwrap();
        assert!(sock.broadcast().unwrap());
    }
}
