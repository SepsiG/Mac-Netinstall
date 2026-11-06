//! Minimal BOOTP/DHCP server for RouterBOOT network boot.

pub mod arch;
pub mod packet;

pub use arch::{BootArch, VendorClass};
pub use packet::{MessageType, PacketError, ReplyKind, Request, CLIENT_PORT, SERVER_PORT};
