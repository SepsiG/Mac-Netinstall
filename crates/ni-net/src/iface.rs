//! Network interface enumeration via `getifaddrs(3)`.
//!
//! On macOS `getifaddrs` returns one entry per address: an `AF_LINK` entry
//! carrying the interface index and MAC address, plus one entry per IPv4/IPv6
//! address. [`list_interfaces`] folds them into one [`Interface`] per name.
//!
//! macOS may mask the `AF_LINK` address as `02:00:00:00:00:00` (see
//! [`crate::link`]). Placeholders become `None`; for non-Wi-Fi interfaces
//! the IOKit hardware address fills the gap.
//!
//! [`Interface::kind`] combines the link-layer type of the `AF_LINK` entry
//! (`sdl_type`, an `IFT_*` value), the BSD name prefix and, for Wi-Fi, the
//! SystemConfiguration interface type (Wi-Fi and Ethernet are both
//! `IFT_ETHER`). The UI uses it to hide interfaces that cannot boot a router.

use crate::error::NetError;
use crate::link::{hardware_addresses, known_mac, thunderbolt_ports};
use ni_proto::Mac;
use nix::ifaddrs::getifaddrs;
use nix::net::if_::{if_nametoindex, InterfaceFlags};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::Ipv4Addr;
use std::num::NonZeroU32;
use system_configuration::network_configuration::{get_interfaces, SCNetworkInterfaceType};

/// What kind of link an interface is, as far as booting a router goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterfaceKind {
    /// Wired Ethernet (built-in, USB or Thunderbolt adapters): the only
    /// kind that can network-boot a RouterBOARD.
    Ethernet,
    /// IEEE 802.11 (`en0` on most Macs).
    WiFi,
    Loopback,
    /// `bridge*`, e.g. Thunderbolt Bridge or Internet Sharing.
    Bridge,
    /// Apple Wireless Direct Link, low-latency WLAN and Wi-Fi Aware
    /// (`awdl*`, `llw*`, `nan*`).
    AppleWireless,
    /// VPN and tunnel interfaces (`utun*`, `ipsec*`, `gif*`, `stf*`, `ppp*`).
    Tunnel,
    Other,
}

impl InterfaceKind {
    /// Stable lowercase name for events and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            InterfaceKind::Ethernet => "ethernet",
            InterfaceKind::WiFi => "wifi",
            InterfaceKind::Loopback => "loopback",
            InterfaceKind::Bridge => "bridge",
            InterfaceKind::AppleWireless => "apple_wireless",
            InterfaceKind::Tunnel => "tunnel",
            InterfaceKind::Other => "other",
        }
    }
}

/// `IFT_ETHER` from `<net/if_types.h>`.
const IFT_ETHER: u8 = 0x06;
/// `IFT_LOOP`.
const IFT_LOOP: u8 = 0x18;
/// `IFT_GIF`.
const IFT_GIF: u8 = 0x37;
/// `IFT_STF`.
const IFT_STF: u8 = 0x39;
/// `IFT_BRIDGE`.
const IFT_BRIDGE: u8 = 0xd1;

/// Classify from the BSD name, the `AF_LINK` `sdl_type` (if any) and
/// whether SystemConfiguration reports the interface as Wi-Fi.
pub fn classify(name: &str, link_type: Option<u8>, is_wifi: bool) -> InterfaceKind {
    let prefixed = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
    if prefixed(&["awdl", "llw", "nan"]) {
        return InterfaceKind::AppleWireless;
    }
    if prefixed(&["anpi"]) {
        // Apple's internal network peripheral links (`anpi0`, ...).
        return InterfaceKind::Other;
    }
    let digits_after = |p: &str| {
        name.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    };
    if digits_after("ap") {
        // The Wi-Fi access-point interface (`ap1`), unknown to
        // SystemConfiguration.
        return InterfaceKind::WiFi;
    }
    if prefixed(&["utun", "ipsec", "gif", "stf", "ppp"]) {
        return InterfaceKind::Tunnel;
    }
    if prefixed(&["bridge"]) {
        return InterfaceKind::Bridge;
    }
    match link_type {
        Some(IFT_LOOP) => InterfaceKind::Loopback,
        Some(IFT_BRIDGE) => InterfaceKind::Bridge,
        Some(IFT_GIF | IFT_STF) => InterfaceKind::Tunnel,
        Some(IFT_ETHER) if is_wifi => InterfaceKind::WiFi,
        Some(IFT_ETHER) => InterfaceKind::Ethernet,
        _ if prefixed(&["lo"]) => InterfaceKind::Loopback,
        _ => InterfaceKind::Other,
    }
}

/// A Thunderbolt networking port (IP over Thunderbolt; System Settings
/// calls it `Thunderbolt 1`, `Thunderbolt 2`, ... and puts it in the
/// Thunderbolt Bridge): IOKit says so (`iokit_port`, see
/// [`crate::link::thunderbolt_ports`]), or the display name is exactly
/// `Thunderbolt <number>`. Ethernet adapters attached over Thunderbolt
/// (`Thunderbolt Ethernet ...`) are not ports.
pub fn is_thunderbolt_port(display_name: Option<&str>, iokit_port: bool) -> bool {
    iokit_port
        || display_name
            .and_then(|d| d.strip_prefix("Thunderbolt "))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// SystemConfiguration's view: BSD name -> (is Wi-Fi, localized display
/// name such as `USB 10/100/1000 LAN`).
fn system_configuration_info() -> BTreeMap<String, (bool, String)> {
    let mut out = BTreeMap::new();
    for iface in get_interfaces().iter() {
        let Some(name) = iface.bsd_name() else {
            continue;
        };
        let wifi = matches!(
            iface.interface_type(),
            Some(SCNetworkInterfaceType::IEEE80211)
        );
        let display = iface
            .display_name()
            .map(|d| d.to_string())
            .unwrap_or_default();
        out.insert(name.to_string(), (wifi, display));
    }
    out
}

/// One network interface (e.g. `en7`, `lo0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// BSD name, e.g. `en0`.
    pub name: String,
    /// Kernel interface index (what `IP_BOUND_IF` expects).
    pub index: u32,
    /// Hardware address; `None` for interfaces without one (e.g. `lo0`) and
    /// when macOS masks it and no real one is known (never a placeholder
    /// such as `02:00:00:00:00:00`).
    pub mac: Option<Mac>,
    /// IPv4 addresses with their netmasks.
    pub ipv4: Vec<(Ipv4Addr, Ipv4Addr)>,
    /// `IFF_UP`: administratively up.
    pub is_up: bool,
    /// `IFF_RUNNING`: resources allocated. **Not a carrier indication on
    /// macOS**: Ethernet ports without a cable (`ifconfig` "status:
    /// inactive") are often still `RUNNING`. A manually configured IPv4
    /// address, however, is removed while there is no carrier, so a
    /// missing address is the reliable "no link" signal.
    pub is_running: bool,
    /// `IFF_LOOPBACK`.
    pub is_loopback: bool,
    /// Link kind; see [`classify`].
    pub kind: InterfaceKind,
    /// Localized name from System Settings (e.g. `USB 10/100/1000 LAN`),
    /// if SystemConfiguration knows the interface.
    pub display_name: Option<String>,
    /// A Thunderbolt networking port, not an Ethernet adapter (see
    /// [`is_thunderbolt_port`]); its `kind` is still `Ethernet`.
    pub thunderbolt_port: bool,
}

/// List all interfaces, sorted by name.
pub fn list_interfaces() -> io::Result<Vec<Interface>> {
    let mut by_name: BTreeMap<String, Interface> = BTreeMap::new();
    let mut link_types: BTreeMap<String, u8> = BTreeMap::new();
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
                kind: InterfaceKind::Other,
                display_name: None,
                thunderbolt_port: false,
            });
        entry.is_up |= ifa.flags.contains(InterfaceFlags::IFF_UP);
        entry.is_running |= ifa.flags.contains(InterfaceFlags::IFF_RUNNING);
        entry.is_loopback |= ifa.flags.contains(InterfaceFlags::IFF_LOOPBACK);
        let Some(addr) = ifa.address else { continue };
        if let Some(link) = addr.as_link_addr() {
            entry.index = link.ifindex() as u32;
            link_types.insert(entry.name.clone(), link.datalink_type());
            // `LinkAddr::addr` does not check the address length itself.
            if link.alen() == 6 {
                entry.mac = link.addr().map(Mac).and_then(known_mac);
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
    let sc = system_configuration_info();
    let mut hardware: Option<HashMap<String, Mac>> = None;
    let tb_ports = thunderbolt_ports();
    for iface in &mut out {
        let (wifi, display) = match sc.get(&iface.name) {
            Some((wifi, display)) => (*wifi, Some(display.clone()).filter(|d| !d.is_empty())),
            None => (false, None),
        };
        iface.kind = classify(&iface.name, link_types.get(&iface.name).copied(), wifi);
        iface.thunderbolt_port = iface.kind == InterfaceKind::Ethernet
            && is_thunderbolt_port(display.as_deref(), tb_ports.contains(&iface.name));
        iface.display_name = display;
        if iface.mac.is_none() && iface.kind != InterfaceKind::WiFi {
            // Masked (or missing): the hardware address is the one in use
            // except for Wi-Fi with a private address, which stays unknown.
            let hardware = hardware.get_or_insert_with(hardware_addresses);
            iface.mac = hardware.get(&iface.name).copied();
        }
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
        assert_eq!(lo0.kind, InterfaceKind::Loopback);
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
    fn classify_by_name_prefix_and_link_type() {
        let cases = [
            ("en7", Some(IFT_ETHER), false, InterfaceKind::Ethernet),
            ("en0", Some(IFT_ETHER), true, InterfaceKind::WiFi),
            ("lo0", Some(IFT_LOOP), false, InterfaceKind::Loopback),
            ("lo0", None, false, InterfaceKind::Loopback),
            ("bridge0", Some(IFT_BRIDGE), false, InterfaceKind::Bridge),
            ("bridge100", Some(IFT_ETHER), false, InterfaceKind::Bridge),
            (
                "awdl0",
                Some(IFT_ETHER),
                false,
                InterfaceKind::AppleWireless,
            ),
            ("llw0", Some(IFT_ETHER), false, InterfaceKind::AppleWireless),
            ("utun3", Some(1), false, InterfaceKind::Tunnel),
            ("gif0", Some(IFT_GIF), false, InterfaceKind::Tunnel),
            ("stf0", Some(IFT_STF), false, InterfaceKind::Tunnel),
            ("ap1", Some(IFT_ETHER), false, InterfaceKind::WiFi),
            ("apx", Some(IFT_ETHER), false, InterfaceKind::Ethernet),
            ("nan0", Some(IFT_ETHER), false, InterfaceKind::AppleWireless),
            ("anpi0", Some(IFT_ETHER), false, InterfaceKind::Other),
            ("xyz0", None, false, InterfaceKind::Other),
        ];
        for (name, link, wifi, kind) in cases {
            assert_eq!(classify(name, link, wifi), kind, "{name}");
        }
        assert_eq!(InterfaceKind::AppleWireless.as_str(), "apple_wireless");
    }

    #[test]
    fn thunderbolt_ports_by_iokit_or_display_name() {
        let cases = [
            (Some("Thunderbolt 1"), false, true),
            (Some("Thunderbolt 12"), false, true),
            (None, true, true),
            (Some("USB 10/100/1000 LAN"), true, true),
            // Thunderbolt-attached Ethernet adapters are real Ethernet.
            (Some("Thunderbolt Ethernet"), false, false),
            (Some("Thunderbolt Ethernet Slot 1"), false, false),
            (Some("Thunderbolt Bridge"), false, false),
            (Some("Thunderbolt "), false, false),
            (Some("USB 10/100/1000 LAN"), false, false),
            (None, false, false),
        ];
        for (display, iokit, want) in cases {
            assert_eq!(is_thunderbolt_port(display, iokit), want, "{display:?}");
        }
    }

    #[test]
    fn listed_thunderbolt_ports_are_ethernet_kind() {
        for iface in list_interfaces().unwrap() {
            if iface.thunderbolt_port {
                assert_eq!(iface.kind, InterfaceKind::Ethernet, "{}", iface.name);
            }
        }
    }

    #[test]
    fn listed_kinds_are_consistent_with_flags() {
        for iface in list_interfaces().unwrap() {
            if iface.is_loopback {
                assert_eq!(iface.kind, InterfaceKind::Loopback, "{}", iface.name);
            }
            // Wi-Fi may be masked with no usable fallback (private address).
            if iface.kind == InterfaceKind::Ethernet {
                assert!(iface.mac.is_some(), "{} has no MAC", iface.name);
            }
            assert!(
                iface.mac.and_then(known_mac) == iface.mac,
                "{}: placeholder MAC",
                iface.name
            );
        }
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
