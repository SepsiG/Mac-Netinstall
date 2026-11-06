//! Network interface enumeration via `getifaddrs(3)`.
//!
//! On macOS `getifaddrs` returns one entry per address: an `AF_LINK` entry
//! carrying the interface index and MAC address, plus one entry per IPv4/IPv6
//! address. [`list_interfaces`] folds them into one [`Interface`] per name.

use crate::error::NetError;
use ni_proto::Mac;
use nix::ifaddrs::getifaddrs;
use nix::net::if_::{if_nametoindex, InterfaceFlags};
use std::collections::BTreeMap;
use std::io;
use std::net::Ipv4Addr;
use std::num::NonZeroU32;

/// One network interface (e.g. `en7`, `lo0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// BSD name, e.g. `en0`.
    pub name: String,
    /// Kernel interface index (what `IP_BOUND_IF` expects).
    pub index: u32,
    /// Hardware address; `None` for interfaces without one (e.g. `lo0`).
    pub mac: Option<Mac>,
    /// IPv4 addresses with their netmasks.
    pub ipv4: Vec<(Ipv4Addr, Ipv4Addr)>,
    /// `IFF_UP`: administratively up.
    pub is_up: bool,
    /// `IFF_RUNNING`: resources allocated (on macOS: link is active).
    pub is_running: bool,
    /// `IFF_LOOPBACK`.
    pub is_loopback: bool,
}

/// List all interfaces, sorted by name.
pub fn list_interfaces() -> io::Result<Vec<Interface>> {
    let mut by_name: BTreeMap<String, Interface> = BTreeMap::new();
    for ifa in getifaddrs()? {
        let entry = by_name
            .entry(ifa.interface_name.clone())
            .or_insert_with(|| Interface {
                name: ifa.interface_name.clone(),
                index: 0,
                mac: None,
                ipv4: Vec::new(),
                is_up: false,
                is_running: false,
                is_loopback: false,
            });
        entry.is_up |= ifa.flags.contains(InterfaceFlags::IFF_UP);
        entry.is_running |= ifa.flags.contains(InterfaceFlags::IFF_RUNNING);
        entry.is_loopback |= ifa.flags.contains(InterfaceFlags::IFF_LOOPBACK);
        let Some(addr) = ifa.address else { continue };
        if let Some(link) = addr.as_link_addr() {
            entry.index = link.ifindex() as u32;
            // `LinkAddr::addr` does not check the address length itself.
            if link.alen() == 6 {
                entry.mac = link.addr().map(Mac);
            }
        } else if let Some(sin) = addr.as_sockaddr_in() {
            let mask = ifa
                .netmask
                .as_ref()
                .and_then(|m| m.as_sockaddr_in())
                .map(|m| m.ip())
                .unwrap_or(Ipv4Addr::UNSPECIFIED);
            entry.ipv4.push((sin.ip(), mask));
        }
    }
    let mut out: Vec<Interface> = by_name.into_values().collect();
    for iface in &mut out {
        if iface.index == 0 {
            if let Ok(index) = if_nametoindex(iface.name.as_str()) {
                iface.index = index;
            }
        }
    }
    Ok(out)
}

/// Interface index for `name`, as needed by `IP_BOUND_IF`.
pub fn interface_index(name: &str) -> Result<NonZeroU32, NetError> {
    if_nametoindex(name)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or_else(|| NetError::UnknownInterface(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lo0_is_listed_as_loopback_with_127_0_0_1() {
        let all = list_interfaces().unwrap();
        let lo0 = all.iter().find(|i| i.name == "lo0").expect("lo0 listed");
        assert!(lo0.is_loopback);
        assert!(lo0.is_up);
        assert!(lo0.index > 0);
        assert_eq!(lo0.mac, None);
        assert!(lo0
            .ipv4
            .contains(&(Ipv4Addr::LOCALHOST, Ipv4Addr::new(255, 0, 0, 0))));
    }

    #[test]
    fn some_interface_has_a_mac() {
        // Every Mac has at least one Ethernet-like interface (en0).
        let all = list_interfaces().unwrap();
        assert!(all.iter().any(|i| i.mac.is_some() && !i.is_loopback));
    }

    #[test]
    fn interface_index_matches_listing() {
        let all = list_interfaces().unwrap();
        let lo0 = all.iter().find(|i| i.name == "lo0").unwrap();
        assert_eq!(interface_index("lo0").unwrap().get(), lo0.index);
    }

    #[test]
    fn unknown_interface_is_an_error() {
        let err = interface_index("nosuch0").unwrap_err();
        assert!(matches!(err, NetError::UnknownInterface(name) if name == "nosuch0"));
    }
}
