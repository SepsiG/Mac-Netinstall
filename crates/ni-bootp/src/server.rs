//! tokio runner for [`BootpResponder`].
//!
//! [`serve`] owns its responder. [`serve_shared`] runs one behind a
//! [`BootpHandle`], so the caller (the engine) can replace the boot file
//! map, set a MAC policy and look up leases while the server runs.

use crate::arch::BootArch;
use crate::packet::{PacketError, ReplyKind};
use crate::responder::{BootpResponder, ConfigError, IgnoreReason, Outcome};
use ni_net::{RecvErrorAction, RecvErrorGuard};
use ni_proto::Mac;
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{Sender, UnboundedSender};

/// Decides whether a RouterBOOT client (by MAC) may be served. Called for
/// every supported request while the handle's lock is held: it must be
/// quick and must not call back into the [`BootpHandle`].
pub type MacPolicy = Arc<dyn Fn(Mac) -> bool + Send + Sync>;

struct Shared {
    responder: BootpResponder,
    policy: Option<MacPolicy>,
}

/// A [`BootpResponder`] shared between [`serve_shared`] and its owner.
/// Cheap to clone; all clones refer to the same responder.
#[derive(Clone)]
pub struct BootpHandle {
    inner: Arc<Mutex<Shared>>,
}

impl BootpHandle {
    /// Share `responder`, with no MAC policy (everyone is served).
    pub fn new(responder: BootpResponder) -> Self {
        BootpHandle {
            inner: Arc::new(Mutex::new(Shared {
                responder,
                policy: None,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// See [`BootpResponder::set_boot_files`].
    pub fn set_boot_files(&self, files: HashMap<BootArch, String>) -> Result<(), ConfigError> {
        self.lock().responder.set_boot_files(files)
    }

    /// Install (or with `None` remove) the MAC policy; see [`MacPolicy`].
    pub fn set_policy(&self, policy: Option<MacPolicy>) {
        self.lock().policy = policy;
    }

    /// Address leased to `mac`, if any.
    pub fn lease_of(&self, mac: &Mac) -> Option<Ipv4Addr> {
        self.lock().responder.lease_of(mac)
    }

    /// The MAC holding the lease for `ip`, if any.
    pub fn mac_of(&self, ip: Ipv4Addr) -> Option<Mac> {
        self.lock().responder.mac_of(ip)
    }

    fn handle(&self, datagram: &[u8]) -> Result<Outcome, PacketError> {
        let mut shared = self.lock();
        match shared.policy.clone() {
            Some(policy) => shared.responder.handle_with(datagram, &|mac| policy(mac)),
            None => shared.responder.handle(datagram),
        }
    }
}

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
    /// DHCPDECLINE: the lease was dropped and `ip` quarantined.
    Declined {
        mac: Mac,
        ip: Ipv4Addr,
    },
    /// Sending a reply failed (e.g. the interface went down).
    SendFailed {
        mac: Mac,
        error: String,
    },
    /// `recv_from` failed with a transient error (see
    /// [`ni_net::RecvErrorGuard`]); the server keeps running. Coalesced:
    /// `count` errors since the previous report, `error` the latest.
    RecvError {
        error: String,
        count: u32,
    },
}

/// Answer requests arriving on `socket` until receiving fails fatally.
///
/// Receive errors follow [`ni_net::RecvErrorGuard`]: transient ones (e.g.
/// an ICMP error for an earlier send or `ENOBUFS`) back off 20 ms and are
/// reported as coalesced [`BootpEvent::RecvError`]s while serving
/// continues; any other error, or 100 transient errors in a row, is
/// returned.
///
/// Replies go to `reply_to`: `255.255.255.255:68` in production (the
/// client has no address yet, and unicasting to `yiaddr` would need an ARP
/// entry we cannot add without root; `socket` then needs `SO_BROADCAST`
/// and `IP_BOUND_IF`). Tests pass a loopback address. Events are dropped
/// silently if the receiver is gone; abort the task to stop the server.
pub async fn serve(
    socket: UdpSocket,
    responder: BootpResponder,
    reply_to: SocketAddr,
    events: UnboundedSender<BootpEvent>,
) -> io::Result<()> {
    let handle = BootpHandle::new(responder);
    run(socket, &handle, reply_to, |e| {
        let _ = events.send(e);
    })
    .await
}

/// Like [`serve`], but the responder lives in `handle` (shared with the
/// caller) and events go to a bounded channel: when it is full, events are
/// dropped rather than queued, so a flood of junk datagrams cannot grow
/// memory. Size the channel for bursts (the engine uses 256).
pub async fn serve_shared(
    socket: UdpSocket,
    handle: BootpHandle,
    reply_to: SocketAddr,
    events: Sender<BootpEvent>,
) -> io::Result<()> {
    run(socket, &handle, reply_to, |e| {
        let _ = events.try_send(e);
    })
    .await
}

async fn run(
    socket: UdpSocket,
    handle: &BootpHandle,
    reply_to: SocketAddr,
    emit: impl Fn(BootpEvent),
) -> io::Result<()> {
    let mut buf = vec![0u8; 1500];
    let mut recv_errors = RecvErrorGuard::default();
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(r) => {
                recv_errors.on_success();
                r
            }
            Err(e) => match recv_errors.on_error(&e, Instant::now()) {
                RecvErrorAction::Fatal => return Err(e),
                RecvErrorAction::Retry { report } => {
                    if let Some(count) = report {
                        emit(BootpEvent::RecvError {
                            error: e.to_string(),
                            count,
                        });
                    }
                    tokio::time::sleep(RecvErrorGuard::BACKOFF).await;
                    continue;
                }
            },
        };
        let outcome = handle.handle(&buf[..n]);
        let event = match outcome {
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
            Ok(Outcome::Declined { mac, ip }) => BootpEvent::Declined { mac, ip },
            Err(error) => BootpEvent::Malformed { from, error },
        };
        emit(event);
    }
}
