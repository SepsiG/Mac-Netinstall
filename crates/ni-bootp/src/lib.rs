//! Minimal BOOTP/DHCP server for RouterBOOT network boot.
//!
//! RouterBOOT sends a BOOTP or DHCP request with its architecture in the
//! vendor class (option 60). The reply assigns an address from a MAC-keyed
//! pool and names the boot kernel to fetch over TFTP from `siaddr`.
//!
//! Only directly attached clients are served: relayed requests (`giaddr`
//! or `hops` set) are ignored. A DHCPREQUEST for an address we did not
//! lease to that MAC gets a DHCPNAK; a DHCPDECLINE quarantines the address.
//!
//! [`BootpResponder`] is sans-IO; [`serve`] runs it on a tokio socket.

pub mod arch;
pub mod packet;
pub mod pool;
pub mod responder;
pub mod server;

pub use arch::{BootArch, VendorClass};
pub use packet::{MessageType, PacketError, ReplyKind, Request, CLIENT_PORT, SERVER_PORT};
pub use pool::{LeasePool, PoolError};
pub use responder::{BootpConfig, BootpResponder, ConfigError, IgnoreReason, Outcome, Reply};
pub use server::{serve, BootpEvent};
