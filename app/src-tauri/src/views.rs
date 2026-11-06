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

/// The download cache, for the settings view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheInfo {
    /// For display only, with `~` for the home folder.
    pub location: String,
    pub bytes: u64,
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
        };
        let routed: BTreeSet<String> = ["en7".to_string()].into();
        let v = serde_json::to_value(InterfaceView::new(&i, &routed)).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "name": "en7", "display_name": "USB 10/100/1000 LAN", "kind": "ethernet",
                "mac": "48:A9:8A:01:02:03",
                "ipv4": [{"address": "192.168.88.1", "prefix": 24}],
                "up": true, "running": false, "default_route": true
            })
        );
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
