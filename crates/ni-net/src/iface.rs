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
use crate::ipconfig::{configured_ipv4_all, ConfiguredIpv4};
use crate::link::{controller_classes, hardware_addresses, known_mac};
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
    /// A real wired Ethernet NIC, whatever the bus: built-in, PCIe, USB
    /// adapters, Thunderbolt Ethernet adapters, docks. The only kind that
    /// can network-boot a RouterBOARD.
    Ethernet,
    /// A Thunderbolt port used for IP over Thunderbolt (`Thunderbolt 1`,
    /// a member of the Thunderbolt Bridge): looks like Ethernet, is not.
    ThunderboltIp,
    /// Software or host-internal links that look like Ethernet: the Mac's
    /// USB-C ports in USB device mode (`anpi*`, "Ethernet Adapter (en2)"),
    /// virtual machine and fake Ethernet interfaces (`vmenet*`, `feth*`,
    /// `vlan*`).
    Virtual,
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
            InterfaceKind::ThunderboltIp => "thunderbolt_ip",
            InterfaceKind::Virtual => "virtual",
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

/// Classify from the BSD name, the `AF_LINK` `sdl_type` (if any),
/// whether SystemConfiguration reports the interface as Wi-Fi, its display
/// name, and the IOKit classes of its controller and providers
/// (`controllers`, nearest first; see [`crate::link::controller_classes`]).
///
/// IOKit decides between a real NIC and a look-alike (Thunderbolt IP port,
/// Wi-Fi, USB device mode) by class, never by bus: a USB or Thunderbolt
/// Ethernet adapter is Ethernet. Names and the display name are only the
/// fallback when IOKit knows nothing.
pub fn classify(
    name: &str,
    link_type: Option<u8>,
    is_wifi: bool,
    display_name: Option<&str>,
    controllers: &[&str],
) -> InterfaceKind {
    let prefixed = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
    if prefixed(&["awdl", "llw", "nan"]) {
        return InterfaceKind::AppleWireless;
    }
    if prefixed(&["anpi", "vmenet", "feth", "vlan"]) {
        // Apple's internal network peripheral links (`anpi0`), VM and fake
        // Ethernet, VLANs on top of something else.
        return InterfaceKind::Virtual;
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
    let any_class = |test: &dyn Fn(&str) -> bool| controllers.iter().any(|c| test(c));
    if any_class(&|c| c.contains("ThunderboltIP")) {
        return InterfaceKind::ThunderboltIp;
    }
    if is_wifi || any_class(&|c| c.contains("WLAN") || c.contains("AirPort") || c.contains("80211"))
    {
        return InterfaceKind::WiFi;
    }
    if any_class(&|c| c == "IOUSBDeviceInterface" || c.starts_with("AppleUSBDevice")) {
        // The Mac is the USB *device* here (its own USB-C port).
        return InterfaceKind::Virtual;
    }
    match link_type {
        Some(IFT_LOOP) => InterfaceKind::Loopback,
        Some(IFT_BRIDGE) => InterfaceKind::Bridge,
        Some(IFT_GIF | IFT_STF) => InterfaceKind::Tunnel,
        Some(IFT_ETHER) if controllers.is_empty() && is_thunderbolt_port_name(display_name) => {
            InterfaceKind::ThunderboltIp
        }
        Some(IFT_ETHER) => InterfaceKind::Ethernet,
        _ if prefixed(&["lo"]) => InterfaceKind::Loopback,
        _ => InterfaceKind::Other,
    }
}

/// System Settings' name of a Thunderbolt IP port: exactly
/// `Thunderbolt <number>` (Thunderbolt Ethernet adapters are named
/// otherwise, e.g. `Thunderbolt Ethernet Slot 1`).
fn is_thunderbolt_port_name(display_name: Option<&str>) -> bool {
    display_name
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
    /// What its network service is configured to use (System Settings →
    /// TCP/IP); there even without link. `None` without a service.
    pub configured_ipv4: Option<ConfiguredIpv4>,
}

impl Interface {
    /// A real wired Ethernet NIC ([`InterfaceKind::Ethernet`]): the only
    /// kind of interface a RouterBOARD can be netinstalled through.
    pub fn is_netinstall_capable(&self) -> bool {
        self.kind == InterfaceKind::Ethernet && !self.is_loopback
    }
}

/// The parent of a VLAN interface from `ifconfig <vlan>` output
/// (`vlan: 10 parent interface: en6`).
pub fn parse_vlan_parent(ifconfig: &str) -> Option<String> {
    let rest = ifconfig
        .lines()
        .find_map(|l| l.split_once("parent interface:"))?
        .1
        .trim();
    (!rest.is_empty() && !rest.starts_with('<')).then(|| rest.to_string())
}

/// A VLAN (`vlan*`) on a capable adapter is Ethernet too (tagged).
pub fn apply_vlan_parents(list: &mut [Interface], parents: &BTreeMap<String, String>) {
    let capable: Vec<String> = list
        .iter()
        .filter(|i| i.is_netinstall_capable())
        .map(|i| i.name.clone())
        .collect();
    for iface in list.iter_mut() {
        if parents
            .get(&iface.name)
            .is_some_and(|p| capable.contains(p))
        {
            iface.kind = InterfaceKind::Ethernet;
        }
    }
}

/// `vlan*` interface -> its parent, asking `ifconfig` (only run when
/// there are VLANs).
fn vlan_parents(list: &[Interface]) -> BTreeMap<String, String> {
    list.iter()
        .filter(|i| i.name.starts_with("vlan"))
        .filter_map(|i| {
            let out = std::process::Command::new("/sbin/ifconfig")
                .arg(&i.name)
                .output()
                .ok()?;
            let parent = parse_vlan_parent(&String::from_utf8_lossy(&out.stdout))?;
            Some((i.name.clone(), parent))
        })
        .collect()
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
                configured_ipv4: None,
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
    let controllers = controller_classes();
    let mut configured = configured_ipv4_all();
    for iface in &mut out {
        let (wifi, display) = match sc.get(&iface.name) {
            Some((wifi, display)) => (*wifi, Some(display.clone()).filter(|d| !d.is_empty())),
            None => (false, None),
        };
        let classes: Vec<&str> = controllers
            .get(&iface.name)
            .map(|c| c.iter().map(String::as_str).collect())
            .unwrap_or_default();
        iface.kind = classify(
            &iface.name,
            link_types.get(&iface.name).copied(),
            wifi,
            display.as_deref(),
            &classes,
        );
        iface.display_name = display;
        iface.configured_ipv4 = configured.remove(&iface.name);
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
    let parents = vlan_parents(&out);
    apply_vlan_parents(&mut out, &parents);
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
            ("anpi0", Some(IFT_ETHER), false, InterfaceKind::Virtual),
            ("vmenet0", Some(IFT_ETHER), false, InterfaceKind::Virtual),
            ("xyz0", None, false, InterfaceKind::Other),
        ];
        for (name, link, wifi, kind) in cases {
            assert_eq!(classify(name, link, wifi, None, &[]), kind, "{name}");
        }
        assert_eq!(InterfaceKind::AppleWireless.as_str(), "apple_wireless");
    }

    /// IOKit provider classes (controller first) as seen on real Macs.
    const USB_ADAPTER: &[&str] = &["AppleUserECM", "IOUSBHostInterface", "IOUSBHostDevice"];
    const USB_RTL8153: &[&str] = &["AppleUserRTL815x", "IOUSBHostInterface", "IOUSBHostDevice"];
    const TB_ETHERNET: &[&str] = &["AppleBCM5701Ethernet", "IOPCIDevice", "IOPCI2PCIBridge"];
    const DOCK_AQUANTIA: &[&str] = &[
        "AppleEthernetAquantiaAqtion",
        "IOPCIDevice",
        "IOPCI2PCIBridge",
    ];
    const BUILT_IN: &[&str] = &["AppleBCM5701Ethernet", "IOPCIDevice", "AppleT8103PCIeCPort"];
    const TB_IP_PORT: &[&str] = &[
        "AppleThunderboltIPPort",
        "AppleThunderboltIPService",
        "IOThunderboltXDomainServiceClientManager",
    ];
    const WLAN: &[&str] = &[
        "IOSkywalkLegacyEthernet",
        "IOUserNetworkWLAN",
        "CCIOService",
    ];
    /// The Mac's own USB-C ports in USB device mode ("Ethernet Adapter
    /// (en2)" in System Settings, and `anpi*`).
    const USB_DEVICE_MODE: &[&str] = &[
        "AppleUSBDeviceNCM11Data",
        "IOUSBDeviceInterface",
        "AppleUSBDeviceNCM11Control",
    ];

    #[test]
    fn real_ethernet_nics_are_capable_whatever_the_bus() {
        let ether = Some(IFT_ETHER);
        for (name, display, controllers) in [
            ("en7", Some("USB 10/100/1000 LAN"), USB_ADAPTER),
            ("en8", Some("USB 10/100/1G/2.5G LAN"), USB_RTL8153),
            ("en9", Some("Thunderbolt Ethernet Slot 1"), TB_ETHERNET),
            ("en10", Some("Thunderbolt Ethernet"), TB_ETHERNET),
            ("en11", Some("Dock Ethernet"), DOCK_AQUANTIA),
            ("en0", Some("Ethernet"), BUILT_IN),
            // IOKit unavailable: the name and link type decide.
            ("en12", None, &[][..]),
        ] {
            let kind = classify(name, ether, false, display, controllers);
            assert_eq!(kind, InterfaceKind::Ethernet, "{name} {display:?}");
        }
    }

    #[test]
    fn ports_wifi_and_virtual_interfaces_are_not_capable() {
        let ether = Some(IFT_ETHER);
        for (name, wifi, display, controllers, want) in [
            (
                "en1",
                false,
                Some("Thunderbolt 1"),
                TB_IP_PORT,
                InterfaceKind::ThunderboltIp,
            ),
            ("en4", false, None, TB_IP_PORT, InterfaceKind::ThunderboltIp),
            // Without IOKit, the port's display name gives it away.
            (
                "en5",
                false,
                Some("Thunderbolt 2"),
                &[][..],
                InterfaceKind::ThunderboltIp,
            ),
            ("en0", true, Some("Wi-Fi"), WLAN, InterfaceKind::WiFi),
            ("en0", false, None, WLAN, InterfaceKind::WiFi),
            (
                "en2",
                false,
                Some("Ethernet Adapter (en2)"),
                USB_DEVICE_MODE,
                InterfaceKind::Virtual,
            ),
            (
                "anpi0",
                false,
                None,
                USB_DEVICE_MODE,
                InterfaceKind::Virtual,
            ),
            (
                "bridge0",
                false,
                Some("Thunderbolt Bridge"),
                &[][..],
                InterfaceKind::Bridge,
            ),
            ("bridge100", false, None, &[][..], InterfaceKind::Bridge),
            ("utun3", false, None, &[][..], InterfaceKind::Tunnel),
            ("vmenet0", false, None, &[][..], InterfaceKind::Virtual),
        ] {
            assert_eq!(
                classify(name, ether, wifi, display, controllers),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn vlan_parents_are_read_from_ifconfig() {
        let out = "vlan0: flags=8843<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\
                   \tether 00:e0:4c:68:00:01\n\
                   \tvlan: 10 parent interface: en6\n\
                   \tstatus: active\n";
        assert_eq!(parse_vlan_parent(out).as_deref(), Some("en6"));
        assert_eq!(
            parse_vlan_parent("\tvlan: 10 parent interface: <none>\n"),
            None
        );
        assert_eq!(parse_vlan_parent(""), None);
    }

    #[test]
    fn a_vlan_on_a_capable_adapter_is_capable() {
        let iface = |name: &str, kind| Interface {
            name: name.into(),
            index: 1,
            mac: None,
            ipv4: Vec::new(),
            is_up: true,
            is_running: true,
            is_loopback: false,
            kind,
            display_name: None,
            configured_ipv4: None,
        };
        let mut list = vec![
            iface("en6", InterfaceKind::Ethernet),
            iface("en1", InterfaceKind::ThunderboltIp),
            iface("vlan0", InterfaceKind::Virtual),
            iface("vlan1", InterfaceKind::Virtual),
            iface("vlan2", InterfaceKind::Virtual),
        ];
        let parents: BTreeMap<String, String> = [
            ("vlan0".to_string(), "en6".to_string()),
            ("vlan1".to_string(), "en1".to_string()),
        ]
        .into();
        apply_vlan_parents(&mut list, &parents);
        let capable: Vec<&str> = list
            .iter()
            .filter(|i| i.is_netinstall_capable())
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(capable, ["en6", "vlan0"]);
    }

    #[test]
    fn only_ethernet_is_netinstall_capable() {
        let mut i = Interface {
            name: "en7".into(),
            index: 7,
            mac: None,
            ipv4: Vec::new(),
            is_up: true,
            is_running: true,
            is_loopback: false,
            kind: InterfaceKind::Ethernet,
            display_name: None,
            configured_ipv4: None,
        };
        assert!(i.is_netinstall_capable());
        for kind in [
            InterfaceKind::ThunderboltIp,
            InterfaceKind::Virtual,
            InterfaceKind::WiFi,
            InterfaceKind::Bridge,
            InterfaceKind::Loopback,
        ] {
            i.kind = kind;
            assert!(!i.is_netinstall_capable(), "{kind:?}");
        }
    }

    /// On a Mac with Thunderbolt IP ports `en1`/`en4` and USB-C device-mode
    /// links `anpi0`/`anpi1` (as on the development machine): none of the
    /// built-in look-alikes is offered. Skipped where those are absent.
    #[test]
    fn this_macs_interfaces_split_as_expected() {
        let all = list_interfaces().unwrap();
        let by_name = |n: &str| all.iter().find(|i| i.name == n);
        let expected = [
            ("en1", InterfaceKind::ThunderboltIp),
            ("en4", InterfaceKind::ThunderboltIp),
            ("anpi0", InterfaceKind::Virtual),
            ("anpi1", InterfaceKind::Virtual),
        ];
        if expected.iter().any(|(n, _)| by_name(n).is_none()) {
            eprintln!("not the development Mac; skipped");
            return;
        }
        for (name, kind) in expected {
            assert_eq!(by_name(name).unwrap().kind, kind, "{name}");
        }
        for name in [
            "en0", "en1", "en2", "en3", "en4", "anpi0", "anpi1", "bridge0", "lo0",
        ] {
            if let Some(i) = by_name(name) {
                assert!(!i.is_netinstall_capable(), "{name}: {:?}", i.kind);
            }
        }
        // Whatever is capable is a real NIC per IOKit.
        for i in all.iter().filter(|i| i.is_netinstall_capable()) {
            assert_eq!(i.kind, InterfaceKind::Ethernet, "{}", i.name);
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
