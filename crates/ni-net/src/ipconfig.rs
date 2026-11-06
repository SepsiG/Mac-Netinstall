//! The IPv4 configuration macOS keeps for an interface's network service
//! (System Settings → Network → the adapter → Details → TCP/IP).
//!
//! Unlike the live address, this is there without a cable: macOS removes
//! a manual address from the interface while there is no carrier, but the
//! service keeps it (`Setup:/Network/Service/<id>/IPv4` in the dynamic
//! store: `ConfigMethod`, `Addresses`, `SubnetMasks`; the service's
//! `Interface` entry names the BSD device).

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::propertylist::CFPropertyList;
use core_foundation::string::CFString;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use system_configuration::dynamic_store::SCDynamicStoreBuilder;

/// How a service gets its IPv4 address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ipv4Method {
    /// "Manually": a fixed address and mask.
    Manual,
    /// "Using DHCP". ("DHCP with manual address", `INFORM`, is
    /// [`Ipv4Method::Manual`]: the address is fixed.)
    Dhcp,
    /// Anything else (`BOOTP`, `LinkLocal`, `PPP`, `Off`, ...), as macOS
    /// names it.
    Other(String),
}

impl Ipv4Method {
    pub fn as_str(&self) -> &str {
        match self {
            Ipv4Method::Manual => "manual",
            Ipv4Method::Dhcp => "dhcp",
            Ipv4Method::Other(s) => s,
        }
    }
}

/// An interface's configured IPv4 (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredIpv4 {
    pub method: Ipv4Method,
    /// The manual address (`None` unless `Manual` with an address).
    pub address: Option<Ipv4Addr>,
    pub netmask: Option<Ipv4Addr>,
}

impl ConfiguredIpv4 {
    /// The manual address with its mask, if configured that way.
    pub fn manual(&self) -> Option<(Ipv4Addr, Ipv4Addr)> {
        match self.method {
            Ipv4Method::Manual => Some((self.address?, self.netmask?)),
            _ => None,
        }
    }
}

/// Turn a service's `IPv4` entry into a [`ConfiguredIpv4`]: the method and
/// the first address and mask. A manual configuration without a usable
/// mask gets none (the caller decides).
pub fn parse_configured_ipv4(
    method: Option<&str>,
    addresses: &[String],
    masks: &[String],
) -> ConfiguredIpv4 {
    let method = match method {
        Some("Manual") | Some("INFORM") => Ipv4Method::Manual,
        Some("DHCP") => Ipv4Method::Dhcp,
        Some(other) => Ipv4Method::Other(other.to_string()),
        None => Ipv4Method::Other("none".into()),
    };
    let ip = |list: &[String]| list.first().and_then(|s| s.parse::<Ipv4Addr>().ok());
    let (address, netmask) = match method {
        Ipv4Method::Manual => (ip(addresses), ip(masks)),
        _ => (None, None),
    };
    ConfiguredIpv4 {
        method,
        address,
        netmask,
    }
}

fn dict(p: Option<CFPropertyList>) -> Option<CFDictionary<CFString, CFType>> {
    let d = p?.downcast_into::<core_foundation::dictionary::CFDictionary>()?;
    // SAFETY: a property-list dictionary has CFString keys.
    Some(unsafe { CFDictionary::wrap_under_get_rule(d.as_concrete_TypeRef()) })
}

fn string(d: &CFDictionary<CFString, CFType>, key: &'static str) -> Option<String> {
    d.find(CFString::from_static_string(key))?
        .downcast::<CFString>()
        .map(|s| s.to_string())
}

fn strings(d: &CFDictionary<CFString, CFType>, key: &'static str) -> Vec<String> {
    let Some(v) = d.find(CFString::from_static_string(key)) else {
        return Vec::new();
    };
    let Some(arr) = v.downcast::<CFArray>() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            // SAFETY: array items are CF objects.
            let t = unsafe { CFType::wrap_under_get_rule(*item) };
            t.downcast::<CFString>().map(|s| s.to_string())
        })
        .collect()
}

/// One network service as read from the dynamic store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEntry {
    pub id: String,
    /// BSD name of its interface.
    pub device: String,
    /// Disabled in System Settings (`__INACTIVE__`).
    pub inactive: bool,
    pub ipv4: ConfiguredIpv4,
}

fn inactive(d: &CFDictionary<CFString, CFType>) -> bool {
    d.find(CFString::from_static_string("__INACTIVE__"))
        .is_some()
}

/// A service from its entities: the service itself (`Setup:/Network/
/// Service/<id>`, which may carry `__INACTIVE__`), its `Interface` and its
/// `IPv4`. `None` without a device name.
pub fn service_entry(
    id: &str,
    service: Option<&CFDictionary<CFString, CFType>>,
    iface: &CFDictionary<CFString, CFType>,
    ipv4: Option<&CFDictionary<CFString, CFType>>,
) -> Option<ServiceEntry> {
    let device = string(iface, "DeviceName")?;
    let parsed = match ipv4 {
        Some(d) => parse_configured_ipv4(
            string(d, "ConfigMethod").as_deref(),
            &strings(d, "Addresses"),
            &strings(d, "SubnetMasks"),
        ),
        None => parse_configured_ipv4(None, &[], &[]),
    };
    Some(ServiceEntry {
        id: id.to_string(),
        device,
        inactive: service.is_some_and(inactive) || ipv4.is_some_and(inactive),
        ipv4: parsed,
    })
}

/// BSD name -> the configured IPv4 of its service: inactive services are
/// ignored; of several services for one device the first in `order`
/// (`Setup:/Network/Global/IPv4` `ServiceOrder`) wins, services missing
/// from the order come after it.
pub fn select_services(
    services: Vec<ServiceEntry>,
    order: &[String],
) -> BTreeMap<String, ConfiguredIpv4> {
    let rank = |id: &str| order.iter().position(|o| o == id).unwrap_or(usize::MAX);
    let mut active: Vec<ServiceEntry> = services.into_iter().filter(|s| !s.inactive).collect();
    active.sort_by_key(|s| rank(&s.id));
    let mut out = BTreeMap::new();
    for s in active {
        out.entry(s.device).or_insert(s.ipv4);
    }
    out
}

/// BSD name -> configured IPv4 of every active network service (see
/// [`select_services`]). Empty if the dynamic store cannot be read.
pub fn configured_ipv4_all() -> BTreeMap<String, ConfiguredIpv4> {
    let Some(store) = SCDynamicStoreBuilder::new("mac-netinstall").build() else {
        return BTreeMap::new();
    };
    let Some(keys) = store.get_keys("Setup:/Network/Service/[^/]+/Interface") else {
        return BTreeMap::new();
    };
    let get = |key: &str| dict(store.get(CFString::new(key)));
    let order = get("Setup:/Network/Global/IPv4")
        .map(|d| strings(&d, "ServiceOrder"))
        .unwrap_or_default();
    let services = keys
        .iter()
        .filter_map(|key| {
            let key = key.to_string();
            let base = key.trim_end_matches("/Interface");
            let id = base.rsplit('/').next().unwrap_or(base);
            service_entry(
                id,
                get(base).as_ref(),
                &get(&key)?,
                get(&format!("{base}/IPv4")).as_ref(),
            )
        })
        .collect();
    select_services(services, &order)
}

/// The configured IPv4 of `ifname`'s network service, if it has one.
pub fn configured_ipv4(ifname: &str) -> Option<ConfiguredIpv4> {
    configured_ipv4_all().remove(ifname)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_foundation::boolean::CFBoolean;

    fn s(list: &[&str]) -> Vec<String> {
        list.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn manual_dhcp_and_other_methods() {
        let inform = parse_configured_ipv4(Some("INFORM"), &s(&["10.0.0.1"]), &s(&["255.0.0.0"]));
        assert_eq!(
            inform.method,
            Ipv4Method::Manual,
            "DHCP with a manual address"
        );
        let m = parse_configured_ipv4(
            Some("Manual"),
            &s(&["192.168.88.1"]),
            &s(&["255.255.255.0"]),
        );
        assert_eq!(m.method, Ipv4Method::Manual);
        assert_eq!(
            m.manual(),
            Some((
                Ipv4Addr::new(192, 168, 88, 1),
                Ipv4Addr::new(255, 255, 255, 0)
            ))
        );
        let d = parse_configured_ipv4(Some("DHCP"), &[], &[]);
        assert_eq!((d.method.as_str(), d.manual()), ("dhcp", None));
        let ppp = parse_configured_ipv4(Some("PPP"), &s(&["10.0.0.1"]), &[]);
        assert_eq!((ppp.method.as_str(), ppp.address), ("PPP", None));
        assert_eq!(
            parse_configured_ipv4(None, &[], &[]).method.as_str(),
            "none"
        );
    }

    #[test]
    fn a_manual_entry_without_a_usable_address_is_not_manual() {
        let m = parse_configured_ipv4(Some("Manual"), &s(&["nonsense"]), &s(&["255.255.255.0"]));
        assert_eq!(m.manual(), None);
        let m = parse_configured_ipv4(Some("Manual"), &s(&["192.168.88.1"]), &[]);
        assert_eq!(m.manual(), None);
    }

    type Dict = CFDictionary<CFString, CFType>;

    fn cfdict(pairs: &[(&str, CFType)]) -> Dict {
        let pairs: Vec<(CFString, CFType)> = pairs
            .iter()
            .map(|(k, v)| (CFString::new(k), v.clone()))
            .collect();
        CFDictionary::from_CFType_pairs(&pairs)
    }

    fn text(v: &str) -> CFType {
        CFString::new(v).as_CFType()
    }

    fn list(vs: &[&str]) -> CFType {
        let items: Vec<CFString> = vs.iter().map(|v| CFString::new(v)).collect();
        CFArray::from_CFTypes(&items).as_CFType()
    }

    fn manual_ipv4(addr: &str) -> Dict {
        cfdict(&[
            ("ConfigMethod", text("Manual")),
            ("Addresses", list(&[addr])),
            ("SubnetMasks", list(&["255.255.255.0"])),
        ])
    }

    #[test]
    fn services_are_read_from_their_dictionaries() {
        let iface = cfdict(&[("DeviceName", text("en6"))]);
        let e = service_entry("A", None, &iface, Some(&manual_ipv4("192.168.88.1"))).unwrap();
        assert_eq!(
            (e.id.as_str(), e.device.as_str(), e.inactive),
            ("A", "en6", false)
        );
        assert_eq!(
            e.ipv4.manual(),
            Some((
                Ipv4Addr::new(192, 168, 88, 1),
                Ipv4Addr::new(255, 255, 255, 0)
            ))
        );
        // Disabled in System Settings: `__INACTIVE__` on the service.
        let off = cfdict(&[("__INACTIVE__", CFBoolean::true_value().as_CFType())]);
        let e = service_entry("B", Some(&off), &iface, None).unwrap();
        assert!(e.inactive);
        assert_eq!(e.ipv4.method.as_str(), "none");
        // No device: not an interface's service.
        assert!(service_entry("C", None, &cfdict(&[]), None).is_none());
    }

    #[test]
    fn inactive_services_are_ignored_and_the_service_order_wins() {
        let entry =
            |id: &str, device: &str, inactive: bool, method: &str, addr: &str| ServiceEntry {
                id: id.into(),
                device: device.into(),
                inactive,
                ipv4: parse_configured_ipv4(Some(method), &s(&[addr]), &s(&["255.255.255.0"])),
            };
        let services = vec![
            entry("late", "en6", false, "Manual", "10.0.0.1"),
            entry("early", "en6", false, "DHCP", ""),
            entry("off", "en7", true, "Manual", "10.0.0.2"),
            entry("unordered", "en8", false, "Manual", "10.0.0.3"),
        ];
        let order = s(&["early", "late"]);
        let picked = select_services(services, &order);
        assert_eq!(picked["en6"].method, Ipv4Method::Dhcp, "first in the order");
        assert!(!picked.contains_key("en7"), "inactive");
        assert_eq!(picked["en8"].address, Some(Ipv4Addr::new(10, 0, 0, 3)));
    }

    #[test]
    fn the_dynamic_store_is_readable() {
        let all = configured_ipv4_all();
        eprintln!("configured IPv4: {all:?}");
        // Wi-Fi has a service on every Mac that has Wi-Fi.
        if let Some(wifi) = all.get("en0") {
            assert!(!wifi.method.as_str().is_empty());
        }
        assert_eq!(configured_ipv4("no-such-if0"), None);
    }
}
