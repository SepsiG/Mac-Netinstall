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
