//! Minimal BOOTP/DHCP server for RouterBOOT network boot.
//!
//! RouterBOOT sends a BOOTP or DHCP request with its architecture in the
//! vendor class (option 60). The reply assigns an address from a MAC-keyed
//! pool and names the boot kernel to fetch over TFTP from `siaddr`.

pub mod arch;
pub mod packet;
pub mod pool;
pub mod responder;

pub use arch::{BootArch, VendorClass};
pub use packet::{MessageType, PacketError, ReplyKind, Request, CLIENT_PORT, SERVER_PORT};
pub use pool::{LeasePool, PoolError};
pub use responder::{BootpConfig, BootpResponder, ConfigError, IgnoreReason, Outcome, Reply};
