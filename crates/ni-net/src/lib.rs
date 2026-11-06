//! macOS networking helpers for mac-netinstall.
//!
//! - [`list_interfaces`]: network interfaces with MAC, IPv4 addresses, flags
//!   and [`InterfaceKind`] (Ethernet, Wi-Fi, loopback, bridge, tunnel, ...).
//! - [`bind_udp`]: UDP sockets bound to one interface with `IP_BOUND_IF`,
//!   so broadcasts go out (and are received) on the selected Ethernet port only.
//!
//! No root and no BPF: everything here uses ordinary sockets.

pub mod error;
pub mod iface;
pub mod udp;

pub use error::NetError;
pub use iface::{classify, interface_index, list_interfaces, Interface, InterfaceKind};
pub use udp::{bind_udp, is_transient_recv_error, BindOptions, RecvErrorAction, RecvErrorGuard};
