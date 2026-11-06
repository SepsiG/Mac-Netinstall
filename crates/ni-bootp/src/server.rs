//! tokio runner for [`BootpResponder`].

use crate::arch::BootArch;
use crate::packet::{PacketError, ReplyKind};
use crate::responder::{BootpResponder, IgnoreReason, Outcome};
use ni_net::{RecvErrorAction, RecvErrorGuard};
use ni_proto::Mac;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Instant;
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
    mut responder: BootpResponder,
    reply_to: SocketAddr,
    events: UnboundedSender<BootpEvent>,
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
                        let _ = events.send(BootpEvent::RecvError {
                            error: e.to_string(),
                            count,
                        });
                    }
                    tokio::time::sleep(RecvErrorGuard::BACKOFF).await;
                    continue;
                }
            },
        };
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
            Ok(Outcome::Declined { mac, ip }) => BootpEvent::Declined { mac, ip },
            Err(error) => BootpEvent::Malformed { from, error },
        };
        let _ = events.send(event);
    }
}
