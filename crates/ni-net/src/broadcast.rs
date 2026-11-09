//! Sending to the limited broadcast with a fallback to the subnet-directed
//! one.
//!
//! macOS may refuse a datagram to `255.255.255.255` with `EHOSTUNREACH` or
//! `ENETUNREACH` (seen before the app's Local Network permission was
//! granted). The same datagram to the interface's directed broadcast
//! (e.g. `172.30.3.255` for `172.30.0.2/22`, same port) reaches the same
//! devices on a directly attached link, so [`BroadcastTarget`] retries it
//! there and, once that worked, keeps using it. If both fail, the error of
//! the directed send is returned (it says why the permission check failed,
//! if that is the cause: see [`is_host_unreachable`]).

use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};

/// `EHOSTUNREACH` ("No route to host"): on macOS, what a send to the local
/// network fails with while the app's Local Network permission is denied
/// (or not yet answered).
pub fn is_host_unreachable(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::HostUnreachable || e.raw_os_error() == Some(nix::libc::EHOSTUNREACH)
}

/// `EHOSTUNREACH` or `ENETUNREACH`: the errors that make
/// [`BroadcastTarget`] try the directed broadcast.
pub fn is_unreachable(e: &io::Error) -> bool {
    is_host_unreachable(e)
        || e.kind() == io::ErrorKind::NetworkUnreachable
        || e.raw_os_error() == Some(nix::libc::ENETUNREACH)
}

/// The subnet-directed broadcast of `ip`/`netmask`, or `None` for a /31 or
/// /32 (no broadcast address).
pub fn directed_broadcast(ip: Ipv4Addr, netmask: Ipv4Addr) -> Option<Ipv4Addr> {
    let mask = u32::from(netmask);
    (mask.leading_ones() <= 30 && mask.count_ones() == mask.leading_ones())
        .then(|| Ipv4Addr::from(u32::from(ip) | !mask))
}

/// Where one socket's broadcasts go (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastTarget {
    dest: SocketAddr,
    directed: Option<SocketAddr>,
    use_directed: bool,
    switched: bool,
}

impl From<SocketAddr> for BroadcastTarget {
    fn from(dest: SocketAddr) -> Self {
        BroadcastTarget::new(dest)
    }
}

impl BroadcastTarget {
    /// Always `dest` (no fallback).
    pub fn new(dest: SocketAddr) -> Self {
        BroadcastTarget {
            dest,
            directed: None,
            use_directed: false,
            switched: false,
        }
    }

    /// `dest`; if it is the limited broadcast, with the directed broadcast
    /// of `ip`/`netmask` (same port) as the fallback.
    pub fn with_subnet(dest: SocketAddr, ip: Ipv4Addr, netmask: Ipv4Addr) -> Self {
        let directed = (dest.ip() == Ipv4Addr::BROADCAST)
            .then(|| directed_broadcast(ip, netmask))
            .flatten()
            .map(|b| SocketAddr::from((b, dest.port())));
        BroadcastTarget {
            directed,
            ..BroadcastTarget::new(dest)
        }
    }

    /// Where the next datagram goes first.
    pub fn current(&self) -> SocketAddr {
        match self.directed {
            Some(d) if self.use_directed => d,
            _ => self.dest,
        }
    }

    /// The fallback, if any.
    pub fn directed(&self) -> Option<SocketAddr> {
        self.directed
    }

    /// `true` once, right after the send that switched to the directed
    /// broadcast (for logging it once).
    pub fn take_switched(&mut self) -> bool {
        std::mem::take(&mut self.switched)
    }

    /// Send one datagram with `send(to)`: to [`Self::current`]; if the
    /// limited broadcast is refused as unreachable, again to the directed
    /// broadcast, which is used from then on if it worked.
    pub async fn send<F, Fut>(&mut self, mut send: F) -> io::Result<usize>
    where
        F: FnMut(SocketAddr) -> Fut,
        Fut: Future<Output = io::Result<usize>>,
    {
        let first = self.current();
        let e = match send(first).await {
            Ok(n) => return Ok(n),
            Err(e) => e,
        };
        let Some(directed) = self.directed.filter(|d| *d != first && is_unreachable(&e)) else {
            return Err(e);
        };
        match send(directed).await {
            Ok(n) => {
                self.use_directed = true;
                self.switched = true;
                Ok(n)
            }
            Err(e2) if is_host_unreachable(&e2) || !is_host_unreachable(&e) => Err(e2),
            Err(_) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const LIMITED: SocketAddr =
        SocketAddr::V4(std::net::SocketAddrV4::new(Ipv4Addr::BROADCAST, 68));

    fn target() -> BroadcastTarget {
        BroadcastTarget::with_subnet(
            LIMITED,
            Ipv4Addr::new(172, 30, 0, 2),
            Ipv4Addr::new(255, 255, 252, 0),
        )
    }

    fn err(code: i32) -> io::Error {
        io::Error::from_raw_os_error(code)
    }

    /// A send function answering from `script` (per destination IP) and
    /// recording where it sent.
    async fn send_all(
        t: &mut BroadcastTarget,
        script: impl Fn(SocketAddr) -> io::Result<usize>,
        sent: &RefCell<Vec<SocketAddr>>,
    ) -> io::Result<usize> {
        t.send(|to| {
            sent.borrow_mut().push(to);
            let r = script(to);
            async move { r }
        })
        .await
    }

    fn directed() -> SocketAddr {
        "172.30.3.255:68".parse().unwrap()
    }

    #[test]
    fn directed_broadcasts() {
        let ip = Ipv4Addr::new(172, 30, 0, 2);
        assert_eq!(
            directed_broadcast(ip, Ipv4Addr::new(255, 255, 252, 0)),
            Some(Ipv4Addr::new(172, 30, 3, 255))
        );
        assert_eq!(
            directed_broadcast(
                Ipv4Addr::new(192, 168, 88, 1),
                Ipv4Addr::new(255, 255, 255, 0)
            ),
            Some(Ipv4Addr::new(192, 168, 88, 255))
        );
        assert_eq!(
            directed_broadcast(ip, Ipv4Addr::new(255, 255, 255, 254)),
            None
        );
        assert_eq!(
            directed_broadcast(ip, Ipv4Addr::new(255, 255, 255, 255)),
            None
        );
        assert_eq!(directed_broadcast(ip, Ipv4Addr::new(255, 0, 255, 0)), None);
        assert_eq!(target().directed(), Some(directed()));
        // Only the limited broadcast falls back.
        let unicast = BroadcastTarget::with_subnet(
            "127.0.0.1:68".parse().unwrap(),
            ip,
            Ipv4Addr::new(255, 255, 252, 0),
        );
        assert_eq!(unicast.directed(), None);
    }

    #[test]
    fn classifies_errors() {
        assert!(is_host_unreachable(&err(nix::libc::EHOSTUNREACH)));
        assert!(!is_host_unreachable(&err(nix::libc::ENETUNREACH)));
        assert!(is_unreachable(&err(nix::libc::ENETUNREACH)));
        assert!(!is_unreachable(&err(nix::libc::EACCES)));
    }

    #[tokio::test]
    async fn a_working_limited_broadcast_is_kept() {
        let mut t = target();
        let sent = RefCell::new(Vec::new());
        assert_eq!(send_all(&mut t, |_| Ok(3), &sent).await.unwrap(), 3);
        assert_eq!(*sent.borrow(), [LIMITED]);
        assert!(!t.take_switched());
        assert_eq!(t.current(), LIMITED);
    }

    #[tokio::test]
    async fn a_refused_limited_broadcast_falls_back_and_sticks() {
        for code in [nix::libc::EHOSTUNREACH, nix::libc::ENETUNREACH] {
            let mut t = target();
            let sent = RefCell::new(Vec::new());
            let script = |to: SocketAddr| if to == LIMITED { Err(err(code)) } else { Ok(5) };
            assert_eq!(send_all(&mut t, script, &sent).await.unwrap(), 5);
            assert_eq!(*sent.borrow(), [LIMITED, directed()]);
            assert!(t.take_switched(), "reported once");
            assert!(!t.take_switched());
            // From now on straight to the directed broadcast.
            sent.borrow_mut().clear();
            send_all(&mut t, script, &sent).await.unwrap();
            assert_eq!(*sent.borrow(), [directed()]);
            assert!(!t.take_switched());
        }
    }

    #[tokio::test]
    async fn both_refused_reports_host_unreachable() {
        let mut t = target();
        let sent = RefCell::new(Vec::new());
        let e = send_all(&mut t, |_| Err(err(nix::libc::EHOSTUNREACH)), &sent)
            .await
            .unwrap_err();
        assert!(is_host_unreachable(&e));
        assert_eq!(sent.borrow().len(), 2);
        assert_eq!(t.current(), LIMITED, "not switched");
        // The limited one unreachable, the directed one without a network:
        // the host-unreachable error is what is reported.
        let script = |to: SocketAddr| {
            Err(err(if to == LIMITED {
                nix::libc::EHOSTUNREACH
            } else {
                nix::libc::ENETUNREACH
            }))
        };
        let e = send_all(&mut t, script, &sent).await.unwrap_err();
        assert!(is_host_unreachable(&e));
    }

    #[tokio::test]
    async fn other_errors_are_not_retried() {
        let mut t = target();
        let sent = RefCell::new(Vec::new());
        let e = send_all(&mut t, |_| Err(err(nix::libc::EACCES)), &sent)
            .await
            .unwrap_err();
        assert_eq!(e.raw_os_error(), Some(nix::libc::EACCES));
        assert_eq!(*sent.borrow(), [LIMITED]);
        // Without a fallback address: no retry either.
        let mut plain = BroadcastTarget::new(LIMITED);
        sent.borrow_mut().clear();
        send_all(&mut plain, |_| Err(err(nix::libc::EHOSTUNREACH)), &sent)
            .await
            .unwrap_err();
        assert_eq!(*sent.borrow(), [LIMITED]);
    }
}
