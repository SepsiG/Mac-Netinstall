//! macOS networking helpers for mac-netinstall.
//!
//! - [`list_interfaces`]: network interfaces with MAC, IPv4 addresses, flags
//!   and [`InterfaceKind`] (Ethernet, Wi-Fi, loopback, bridge, tunnel, ...).
//! - [`bind_udp`]: UDP sockets bound to one interface with `IP_BOUND_IF`,
//!   so broadcasts go out (and are received) on the selected Ethernet port only.
//! - [`BroadcastTarget`]: the limited broadcast, falling back to the
//!   subnet-directed one when macOS refuses it.
//! - [`default_route_interfaces`]: interfaces with an IPv4 default route.
//! - [`configured_ipv4`]: the IPv4 macOS is configured to use on an
//!   interface (also while it has no link).
//!
//! No root and no BPF: everything here uses ordinary sockets.

pub mod broadcast;
pub mod error;
pub mod iface;
pub mod ipconfig;
pub mod link;
pub mod route;
pub mod udp;

#[cfg(test)]
mod test_env;

pub use broadcast::{directed_broadcast, is_host_unreachable, is_unreachable, BroadcastTarget};
pub use error::NetError;
pub use iface::{classify, interface_index, list_interfaces, Interface, InterfaceKind};
pub use ipconfig::{configured_ipv4, ConfiguredIpv4, Ipv4Method};
pub use link::{hardware_addresses, known_mac, mac_from_bytes, MASKED_MAC};
pub use route::{default_route_interfaces, parse_default_routes};
pub use udp::{bind_udp, is_transient_recv_error, BindOptions, RecvErrorAction, RecvErrorGuard};
