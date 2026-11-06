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
