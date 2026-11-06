//! Error type shared by [`crate::iface`] and [`crate::udp`].

use std::io;
use thiserror::Error;

/// Errors from interface lookup and [`crate::bind_udp`].
#[derive(Debug, Error)]
pub enum NetError {
    /// Another socket holds the port (another DHCP/TFTP server running?).
    #[error("UDP port {port} is already in use")]
    AddrInUse { port: u16 },
    #[error("unknown network interface {0:?}")]
    UnknownInterface(String),
    #[error("{op} failed: {source}")]
    Io {
        op: &'static str,
        #[source]
        source: io::Error,
    },
}
