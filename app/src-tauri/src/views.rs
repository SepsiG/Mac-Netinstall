//! What the commands return to the web view (serde, no localized text,
//! no file system paths except a display form of the cache location).

use ni_engine::{Failure, Profile};
use ni_net::Interface;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

/// An IPv4 address with its prefix length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ipv4View {
    pub address: Ipv4Addr,
    pub prefix: u32,
}

/// One network interface for the picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceView {
    /// BSD name, e.g. `en7`.
    pub name: String,
    /// e.g. `USB 10/100/1000 LAN` (from macOS), if known.
    pub display_name: Option<String>,
    /// [`ni_net::InterfaceKind::as_str`].
    pub kind: String,
    /// `None` if unknown (macOS may hide it).
    pub mac: Option<String>,
    pub ipv4: Vec<Ipv4View>,
    pub up: bool,
    pub running: bool,
    /// It carries an IPv4 default route (probably a real LAN).
    pub default_route: bool,
    /// A real wired Ethernet NIC (built-in, USB, Thunderbolt or dock
    /// adapter): the only interfaces the window lists.
    pub netinstall_capable: bool,
    /// How macOS is set up to address it (System Settings → TCP/IP);
    /// `None` without a network service.
    pub configured_ipv4: Option<ConfiguredIpv4View>,
    /// The automatic DHCP pool for its manual address
    /// ([`ni_engine::auto_pool`]); `None` without one.
    pub auto_pool: Option<PoolView>,
}

/// [`ni_net::ConfiguredIpv4`] for the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfiguredIpv4View {
    /// `manual`, `dhcp` or what macOS calls it (`BOOTP`, `none`, ...).
    pub method: String,
    pub address: Option<Ipv4Addr>,
    pub prefix: Option<u32>,
}

/// A DHCP address range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolView {
    pub start: Ipv4Addr,
    pub end: Ipv4Addr,
}

impl InterfaceView {
    pub fn new(i: &Interface, routed: &BTreeSet<String>) -> Self {
        InterfaceView {
            name: i.name.clone(),
            display_name: i.display_name.clone(),
            kind: i.kind.as_str().to_string(),
            mac: i.mac.map(|m| m.to_string()),
            ipv4: i
                .ipv4
                .iter()
                .map(|(a, m)| Ipv4View {
                    address: *a,
                    prefix: u32::from(*m).leading_ones(),
                })
                .collect(),
            up: i.is_up,
            running: i.is_running,
            default_route: routed.contains(&i.name),
            netinstall_capable: i.is_netinstall_capable(),
            configured_ipv4: i.configured_ipv4.as_ref().map(|c| ConfiguredIpv4View {
                method: c.method.as_str().to_string(),
                address: c.address,
                prefix: c.netmask.map(|m| u32::from(m).leading_ones()),
            }),
            auto_pool: i
                .configured_ipv4
                .as_ref()
                .and_then(|c| c.manual())
                .and_then(|(ip, mask)| ni_engine::auto_pool(ip, mask).ok())
                .map(|(start, end)| PoolView { start, end }),
        }
    }
}

/// What `start` and `preflight` need from the window; everything else
/// comes from the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRequest {
    pub interface: String,
    /// Name of the active profile.
    pub profile: Option<String>,
    pub auto_repeat: bool,
}

/// A profile file that could not be used: its file name and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokenProfile {
    pub file: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilesView {
    pub profiles: Vec<Profile>,
    pub broken: Vec<BrokenProfile>,
}

/// Extra packages published for one version and architecture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagesView {
    /// The resolved version (a channel is resolved first).
    pub version: String,
    pub arch: String,
    pub packages: Vec<String>,
}

/// A registered `.rsc` script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptView {
    /// Lowercase file name: what profiles refer to.
    pub name: String,
    pub size: u64,
}

/// A registered local `.npk` file, with what its header says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFileView {
    /// Lowercase file name: what profiles refer to.
    pub name: String,
    pub size: u64,
    /// The package name in its header (`system` for a main package).
    pub package: String,
    /// The architecture it is built for (as devices name it).
    pub arch: String,
    /// Its version, if the header names a final release.
    pub version: Option<String>,
}

/// A release channel and the version it names now (`None`: unknown, e.g.
/// offline and never resolved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelView {
    pub channel: String,
    pub version: Option<String>,
}

/// What the version picker offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionsView {
    pub channels: Vec<ChannelView>,
    /// Newest first (7.x, then 6.x).
    pub versions: Vec<ni_assets::ListedVersion>,
    /// When the list was fetched (Unix seconds; 0: the built-in list).
    pub fetched_unix: u64,
    /// `fresh`, `cached` or `seed`.
    pub source: ni_assets::ListSource,
    /// Why refreshing failed (an asset error code), if it did.
    pub error: Option<String>,
}

/// A license key read from a key file (line breaks removed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseKeyView {
    pub key: String,
}

/// The download cache, for the settings view: what is in it, by
/// path-free ids ([`ni_assets::Inventory`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheView {
    /// For display only, with `~` for the home folder.
    pub location: String,
    pub inventory: ni_assets::Inventory,
}

/// A failure's parameters without the ones that hold paths.
pub fn without_paths(f: Failure) -> Failure {
    let params: BTreeMap<String, String> =
        f.params.into_iter().filter(|(k, _)| k != "path").collect();
    Failure {
        code: f.code,
        params,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_net::InterfaceKind;
    use ni_proto::Mac;

    #[test]
    fn interfaces_are_flattened_for_the_ui() {
        let i = Interface {
            name: "en7".into(),
            index: 7,
            mac: Some(Mac([0x48, 0xa9, 0x8a, 1, 2, 3])),
            ipv4: vec![(
                Ipv4Addr::new(192, 168, 88, 1),
                Ipv4Addr::new(255, 255, 255, 0),
            )],
            is_up: true,
            is_running: false,
            is_loopback: false,
            kind: InterfaceKind::Ethernet,
            display_name: Some("USB 10/100/1000 LAN".into()),
            configured_ipv4: None,
        };
        let routed: BTreeSet<String> = ["en7".to_string()].into();
        let v = serde_json::to_value(InterfaceView::new(&i, &routed)).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "name": "en7", "display_name": "USB 10/100/1000 LAN", "kind": "ethernet",
                "mac": "48:A9:8A:01:02:03",
                "ipv4": [{"address": "192.168.88.1", "prefix": 24}],
                "up": true, "running": false, "default_route": true,
                "netinstall_capable": true,
                "configured_ipv4": null,
                "auto_pool": null
            })
        );
        // The configured manual address and the pool derived from it.
        let configured = Interface {
            configured_ipv4: Some(ni_net::ConfiguredIpv4 {
                method: ni_net::Ipv4Method::Manual,
                address: Some(Ipv4Addr::new(172, 30, 0, 2)),
                netmask: Some(Ipv4Addr::new(255, 255, 252, 0)),
            }),
            ..i.clone()
        };
        let v = serde_json::to_value(InterfaceView::new(&configured, &routed)).unwrap();
        assert_eq!(
            v["configured_ipv4"],
            serde_json::json!({"method": "manual", "address": "172.30.0.2", "prefix": 22})
        );
        assert_eq!(
            v["auto_pool"],
            serde_json::json!({"start": "172.30.0.3", "end": "172.30.3.254"})
        );
        let dhcp = Interface {
            configured_ipv4: Some(ni_net::ConfiguredIpv4 {
                method: ni_net::Ipv4Method::Dhcp,
                address: None,
                netmask: None,
            }),
            ..i.clone()
        };
        let v = serde_json::to_value(InterfaceView::new(&dhcp, &routed)).unwrap();
        assert_eq!(
            v["configured_ipv4"],
            serde_json::json!({"method": "dhcp", "address": null, "prefix": null})
        );
        assert_eq!(v["auto_pool"], serde_json::Value::Null);
        let tb = Interface {
            name: "en1".into(),
            display_name: Some("Thunderbolt 1".into()),
            kind: InterfaceKind::ThunderboltIp,
            ..i
        };
        let v = serde_json::to_value(InterfaceView::new(&tb, &routed)).unwrap();
        assert_eq!(v["netinstall_capable"], false);
        assert_eq!(v["kind"], "thunderbolt_ip");
    }

    #[test]
    fn path_params_are_dropped() {
        let f = Failure::new("profile_io")
            .with("op", "read")
            .with("path", "/Users/x/secret");
        let f = without_paths(f);
        assert_eq!(f.param("path"), None);
        assert_eq!(f.param("op"), Some("read"));
    }
}
