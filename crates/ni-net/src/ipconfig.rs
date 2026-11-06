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
    /// "Using DHCP" (also "DHCP with manual address").
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

/// When one device has several services, the one to use: a manual one
/// first (that is what the user set up for this), else the first.
pub fn pick_service(candidates: Vec<ConfiguredIpv4>) -> Option<ConfiguredIpv4> {
    let manual = candidates.iter().position(|c| c.manual().is_some());
    let mut candidates = candidates;
    match manual {
        Some(i) => Some(candidates.swap_remove(i)),
        None => candidates.into_iter().next(),
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

/// BSD name -> configured IPv4 of every network service (see
/// [`pick_service`] for devices with several). Empty if the dynamic store
/// cannot be read.
pub fn configured_ipv4_all() -> BTreeMap<String, ConfiguredIpv4> {
    let mut by_device: BTreeMap<String, Vec<ConfiguredIpv4>> = BTreeMap::new();
    let Some(store) = SCDynamicStoreBuilder::new("mac-netinstall").build() else {
        return BTreeMap::new();
    };
    let Some(keys) = store.get_keys("Setup:/Network/Service/[^/]+/Interface") else {
        return BTreeMap::new();
    };
    for key in keys.iter() {
        let key = key.to_string();
        let Some(iface) = dict(store.get(CFString::new(&key))) else {
            continue;
        };
        let Some(device) = string(&iface, "DeviceName") else {
            continue;
        };
        let base = key.trim_end_matches("/Interface");
        let ipv4 = dict(store.get(CFString::new(&format!("{base}/IPv4"))));
        let parsed = match ipv4 {
            Some(d) => parse_configured_ipv4(
                string(&d, "ConfigMethod").as_deref(),
                &strings(&d, "Addresses"),
                &strings(&d, "SubnetMasks"),
            ),
            None => parse_configured_ipv4(None, &[], &[]),
        };
        by_device.entry(device).or_default().push(parsed);
    }
    by_device
        .into_iter()
        .filter_map(|(d, c)| Some((d, pick_service(c)?)))
        .collect()
}

/// The configured IPv4 of `ifname`'s network service, if it has one.
pub fn configured_ipv4(ifname: &str) -> Option<ConfiguredIpv4> {
    configured_ipv4_all().remove(ifname)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(list: &[&str]) -> Vec<String> {
        list.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn manual_dhcp_and_other_methods() {
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

    #[test]
    fn a_manual_service_wins() {
        let dhcp = parse_configured_ipv4(Some("DHCP"), &[], &[]);
        let manual = parse_configured_ipv4(Some("Manual"), &s(&["10.1.1.1"]), &s(&["255.0.0.0"]));
        assert_eq!(
            pick_service(vec![dhcp.clone(), manual.clone()]),
            Some(manual)
        );
        assert_eq!(pick_service(vec![dhcp.clone()]), Some(dhcp));
        assert_eq!(pick_service(vec![]), None);
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
