//! Minimal read-only TFTP server (RFC 1350) for RouterBOOT boot kernels.

pub mod packet;

pub use packet::{ErrorCode, Packet, PacketError, Request, DEFAULT_BLKSIZE, PORT};
