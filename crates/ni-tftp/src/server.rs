//! tokio runner: a listener plus one task (and ephemeral socket) per transfer.

use crate::accept::{accept, Accept, FileMap, TransferConfig};
use crate::packet::{ErrorCode, Packet};
use crate::transfer::{Status, Transfer, TransferError};
use ni_net::{bind_udp, is_transient_recv_error, BindOptions};
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::{Id, JoinError, JoinSet};

/// Runner settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TftpConfig {
    pub transfer: TransferConfig,
    /// Our IPv4 address on the boot interface: the address clients send
    /// their RRQ to (the `siaddr` handed out by BOOTP). Each transfer
    /// socket binds `server_ip:0`, so DATA comes from the address the
    /// client contacted even when the listener is bound to `0.0.0.0`.
    pub server_ip: Ipv4Addr,
    /// Interface for `IP_BOUND_IF` on transfer sockets; use the
    /// listener's interface (e.g. `en7`). `None` = not bound.
    pub interface: Option<String>,
}

impl TftpConfig {
    /// Options for one transfer socket: `server_ip:0` on `interface`.
    fn transfer_socket(&self) -> BindOptions {
        let opts = BindOptions::new(0).addr(self.server_ip);
        match &self.interface {
            Some(name) => opts.interface(name.clone()),
            None => opts,
        }
    }
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
    /// The listener's `recv_from` failed with a transient error (see
    /// [`ni_net::is_transient_recv_error`]); the server keeps running.
    RecvError { error: String },
}

/// Running transfer tasks, keyed by task id so a peer is forgotten even
/// when its task panics or is cancelled.
#[derive(Default)]
struct Transfers {
    tasks: JoinSet<()>,
    /// Task id -> (peer, file).
    running: HashMap<Id, (SocketAddr, String)>,
}

impl Transfers {
    fn contains(&self, peer: &SocketAddr) -> bool {
        self.running.values().any(|(p, _)| p == peer)
    }

    fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    fn spawn<F>(&mut self, peer: SocketAddr, file: String, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let handle = self.tasks.spawn(task);
        self.running.insert(handle.id(), (peer, file));
    }

    /// Wait for a task to end; returns its peer and file, plus the join
    /// error if it panicked or was cancelled. `None` when no task runs.
    async fn join_next(&mut self) -> Option<(SocketAddr, String, Option<JoinError>)> {
        let (id, error) = match self.tasks.join_next_with_id().await? {
            Ok((id, ())) => (id, None),
            Err(e) => (e.id(), Some(e)),
        };
        let (peer, file) = self.running.remove(&id)?;
        Some((peer, file, error))
    }
}

/// Serve read requests arriving on `listener` until receiving fails
/// fatally.
///
/// Each accepted RRQ runs as its own task with its own socket (a new
/// transfer ID, as RFC 1350 requires) bound to `cfg.server_ip:0`. A
/// repeated RRQ from a peer whose transfer is still running is ignored.
/// Aborting this task aborts all running transfers.
///
/// Receive errors: transient ones ([`ni_net::is_transient_recv_error`],
/// e.g. ICMP errors or `ENOBUFS`) are reported as
/// [`TftpEvent::RecvError`] and serving continues; any other error is
/// returned.
pub async fn serve(
    listener: UdpSocket,
    files: Arc<FileMap>,
    cfg: TftpConfig,
    events: UnboundedSender<TftpEvent>,
) -> io::Result<()> {
    let mut transfers = Transfers::default();
    let mut buf = vec![0u8; 2048];
    loop {
        tokio::select! {
            received = listener.recv_from(&mut buf) => {
                let (n, peer) = match received {
                    Ok(r) => r,
                    Err(e) if is_transient_recv_error(&e) => {
                        let _ = events.send(TftpEvent::RecvError { error: e.to_string() });
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                if transfers.contains(&peer) {
                    continue;
                }
                match accept(&buf[..n], &files, &cfg.transfer, Instant::now()) {
                    Accept::Start(t) => {
                        let file = t.file().to_string();
                        let _ = events.send(TftpEvent::Started {
                            peer,
                            file: file.clone(),
                            size: t.size(),
                        });
                        transfers.spawn(
                            peer,
                            file,
                            run_transfer(t, peer, cfg.transfer_socket(), events.clone()),
                        );
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
            Some((peer, file, error)) = transfers.join_next(), if !transfers.is_empty() => {
                if let Some(e) = error {
                    let _ = events.send(TftpEvent::Failed {
                        peer,
                        file,
                        error: TransferError::Io(format!("transfer task ended: {e}")),
                    });
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
) {
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
                Ok((n, from)) => {
                    // RFC 1350: answer a stray packet with error 5, but
                    // never answer an ERROR (that could loop forever).
                    if !matches!(Packet::parse(&buf[..n]), Ok(Packet::Error { .. })) {
                        let _ = sock.send_to(&unknown_tid, from).await;
                    }
                }
                Err(e) if is_transient_recv_error(&e) => {}
                Err(e) => t.abort(TransferError::Io(e.to_string())),
            },
            _ = tokio::time::sleep_until(deadline.into()) => t.handle_timeout(Instant::now()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }

    #[tokio::test]
    async fn finished_and_panicked_transfers_release_their_peer() {
        let mut transfers = Transfers::default();
        transfers.spawn(peer(1), "ok".into(), async {});
        transfers.spawn(peer(2), "boom".into(), async { panic!("forced failure") });
        assert!(transfers.contains(&peer(1)));
        assert!(transfers.contains(&peer(2)));
        let mut ended = Vec::new();
        while let Some((p, file, error)) = transfers.join_next().await {
            ended.push((p, file, error.map(|e| e.is_panic())));
        }
        ended.sort_by_key(|(p, ..)| p.port());
        assert_eq!(
            ended,
            vec![
                (peer(1), "ok".to_string(), None),
                (peer(2), "boom".to_string(), Some(true)),
            ]
        );
        assert!(!transfers.contains(&peer(1)));
        assert!(!transfers.contains(&peer(2)));
        assert!(transfers.is_empty());
    }
}
