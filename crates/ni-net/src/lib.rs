//! macOS networking helpers for mac-netinstall.
//!
//! - [`list_interfaces`]: network interfaces with MAC, IPv4 addresses and flags.
//!
//! No root and no BPF: everything here uses ordinary sockets.

pub mod error;
pub mod iface;

pub use error::NetError;
pub use iface::{interface_index, list_interfaces, Interface};
