//! Minimal read-only TFTP server (RFC 1350) for RouterBOOT boot kernels.
//!
//! Supports RRQ in `octet` and `netascii` (served as octet) modes and the
//! `tsize`, `timeout` and (opt-in) `blksize` options (RFC 2347-2349).
//! [`accept()`] and [`Transfer`] are sans-IO; [`serve`] runs them on tokio,
//! and [`serve_watched`] does the same with a replaceable file map, progress
//! events and a bounded event channel.

pub mod accept;
pub mod packet;
pub mod server;
pub mod transfer;

pub use accept::{accept, Accept, FileMap, Rejection, TransferConfig};
pub use packet::{ErrorCode, Packet, PacketError, Request, DEFAULT_BLKSIZE, PORT};
pub use server::{serve, serve_watched, TftpConfig, TftpEvent};
pub use transfer::{Status, Transfer, TransferError};
