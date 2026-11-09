//! Events and errors: stable codes with parameters, never localized text.
//!
//! Everything here is serde-serializable; the CLI prints it as JSON lines
//! and the Tauri UI (Plan 6) translates `code` (plus `params`) itself.
//! A `detail` parameter, where present, is an English diagnostic for logs.

use ni_proto::{DeviceInfo, Mac};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};

/// `Mac` as `"48:A9:8A:11:22:33"` (ni-proto has no serde support).
pub mod mac_str {
    use ni_proto::Mac;
    use serde::{de, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(mac: &Mac, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(mac)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Mac, D::Error> {
        String::deserialize(d)?.parse().map_err(de::Error::custom)
    }
}

/// `Option<Mac>` as `"48:A9:8A:11:22:33"` or `null`.
pub mod mac_opt {
    use ni_proto::Mac;
    use serde::{de, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(mac: &Option<Mac>, s: S) -> Result<S::Ok, S::Error> {
        match mac {
            Some(mac) => s.collect_str(mac),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Mac>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|s| s.parse().map_err(de::Error::custom))
            .transpose()
    }
}

/// A failure: a stable `code` (an i18n key) and named parameters.
///
/// Used as the error type of every engine command, inside
/// [`DeviceState::Failed`], and for engine-level [`EngineEvent::Error`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub code: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

impl Failure {
    pub fn new(code: &str) -> Self {
        Failure {
            code: code.to_string(),
            params: BTreeMap::new(),
        }
    }

    /// Add a parameter.
    pub fn with(mut self, key: &str, value: impl fmt::Display) -> Self {
        self.params.insert(key.to_string(), value.to_string());
        self
    }

    /// A parameter's value, if present.
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(String::as_str)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.code)?;
        for (i, (k, v)) in self.params.iter().enumerate() {
            let sep = if i == 0 { " (" } else { ", " };
            write!(f, "{sep}{k}: {v}")?;
        }
        if !self.params.is_empty() {
            f.write_str(")")?;
        }
        Ok(())
    }
}

impl std::error::Error for Failure {}

/// Why a discovered device is not being flashed yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    /// No profile assigned and auto-repeat is off: call `flash`.
    NoProfile,
    /// The concurrency limit is reached; it starts when a slot frees up.
    Concurrency,
}

/// Where a device is in its lifecycle (one per MAC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceState {
    /// It asked for a boot file over BOOTP and got an address.
    BootpSeen {
        boot_arch: String,
        ip: Ipv4Addr,
    },
    /// Its boot kernel is being sent over TFTP.
    KernelServing {
        sent: u64,
        total: u64,
    },
    /// Its Netinstall hello arrived (see [`EngineEvent::Discovered`]).
    Discovered,
    Waiting {
        reason: WaitReason,
    },
    /// Packages and script are being downloaded / read. `version` is the
    /// RouterOS version being installed, once known (it is known at once
    /// when the profile's version is the one the boot kernels were
    /// resolved to; otherwise it is resolved during preparation).
    /// `config` is the profile's configuration mode (`keep`, `default`,
    /// `empty`, `script`).
    Preparing {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<String>,
    },
    /// The install session runs; `phase` is [`ni_proto::Phase::name`],
    /// `version` the RouterOS version being installed.
    Flashing {
        version: String,
        phase: String,
        sent: u64,
        total: u64,
    },
    /// Installed; the device reboots. Ignored for the cooldown, and after
    /// it until the device netboots again (a new BOOTP request).
    Done,
    /// See [`Failure`]; `retry` re-arms it. A failed device that netboots
    /// again (a new BOOTP request, e.g. after a power cycle) is reset to
    /// `bootp_seen`, and with auto-repeat (or an assigned profile) it is
    /// flashed again automatically.
    Failed {
        code: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        params: BTreeMap<String, String>,
    },
}

impl DeviceState {
    pub fn failed(f: Failure) -> Self {
        DeviceState::Failed {
            code: f.code,
            params: f.params,
        }
    }

    /// Stable name of the state (the serialized `state` tag).
    pub fn name(&self) -> &'static str {
        match self {
            DeviceState::BootpSeen { .. } => "bootp_seen",
            DeviceState::KernelServing { .. } => "kernel_serving",
            DeviceState::Discovered => "discovered",
            DeviceState::Waiting { .. } => "waiting",
            DeviceState::Preparing { .. } => "preparing",
            DeviceState::Flashing { .. } => "flashing",
            DeviceState::Done => "done",
            DeviceState::Failed { .. } => "failed",
        }
    }

    /// Preparing or flashing: counts against the concurrency limit.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            DeviceState::Preparing { .. } | DeviceState::Flashing { .. }
        )
    }
}

/// The boot kernels the engine serves (see [`EngineEvent::Kernels`]).
///
/// While they are not `ready` the engine runs but serves no boot file:
/// devices that netboot are `ignored` (`no_boot_file`) and RouterBOOT
/// keeps asking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum KernelState {
    /// The engine is not running.
    Idle,
    /// Being resolved and fetched for `spec` (a channel such as `stable`
    /// or an exact version). `done` / `total` are bytes of the
    /// `netinstall` tarball while it downloads (a cached set skips that).
    Downloading {
        spec: String,
        done: u64,
        total: Option<u64>,
    },
    /// Served. `offline`: the mirror was unreachable, so this is the
    /// version the channel last resolved to (from the cache).
    Ready { version: String, offline: bool },
    /// Could not be had (`kernels_unavailable` with the asset error as
    /// `cause`). The engine keeps running without boot kernels.
    Unavailable {
        code: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        params: BTreeMap<String, String>,
    },
}

impl KernelState {
    /// Stable name of the state (the serialized `state` tag).
    pub fn name(&self) -> &'static str {
        match self {
            KernelState::Idle => "idle",
            KernelState::Downloading { .. } => "downloading",
            KernelState::Ready { .. } => "ready",
            KernelState::Unavailable { .. } => "unavailable",
        }
    }
}

/// What a device said about itself in its Netinstall hello.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfoView {
    pub model: String,
    pub arch: String,
    pub license_id: String,
    pub min_os: String,
}

impl From<&DeviceInfo> for DeviceInfoView {
    fn from(info: &DeviceInfo) -> Self {
        DeviceInfoView {
            model: info.model.clone(),
            arch: info.arch.clone(),
            license_id: info.license_id.clone(),
            min_os: info.min_os.clone(),
        }
    }
}

/// Where the engine listens, as returned by `start`. Unspecified bind
/// addresses are reported with the server IP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartInfo {
    pub interface: String,
    pub server_ip: Ipv4Addr,
    pub bootp: SocketAddr,
    pub tftp: SocketAddr,
    pub netinstall: SocketAddr,
}

/// Everything the engine reports, in order, on its broadcast channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EngineEvent {
    Started(StartInfo),
    Stopped,
    /// The boot kernel state changed (also reported for download
    /// progress). `kernels_ready` and the `kernels_unavailable` error are
    /// still sent as well.
    Kernels {
        #[serde(flatten)]
        state: KernelState,
    },
    /// Boot kernels of `version` are being served.
    KernelsReady {
        version: String,
    },
    /// A download is running (from `ni-assets`, throttled to 256 KiB).
    Download {
        file: String,
        done: u64,
        total: Option<u64>,
    },
    /// A device changed state.
    Device {
        #[serde(with = "mac_str")]
        mac: Mac,
        #[serde(flatten)]
        state: DeviceState,
    },
    /// A device's Netinstall hello, the first time it is seen.
    Discovered {
        #[serde(with = "mac_str")]
        mac: Mac,
        #[serde(flatten)]
        info: DeviceInfoView,
    },
    /// A device was dropped from the device list (`forget`).
    Forgotten {
        #[serde(with = "mac_str")]
        mac: Mac,
    },
    /// The device the engine serves exclusively changed (`set_only_mac`;
    /// `null`: every device).
    OnlyMacChanged {
        #[serde(with = "mac_opt")]
        mac: Option<Mac>,
    },
    /// A device was not served: `cooldown`, `not_selected`,
    /// `awaiting_bootp` (hellos from a done device that has not netbooted
    /// again), `no_boot_file`, `pool_exhausted` or `unsupported_device`.
    /// Reported at most every 10 s per device and reason.
    Ignored {
        #[serde(with = "mac_str")]
        mac: Mac,
        reason: String,
    },
    /// The engine cannot go on (it stops afterwards, unless the code is
    /// `kernels_unavailable`): `interface_gone` (the interface was missing
    /// for longer than the grace period), `server_ip_not_on_interface`
    /// (link and other addresses, but not ours, for 3 checks in a row:
    /// `interface`, `ip`, `addresses`), `runner_failed`, ...
    Error(Failure),
    /// Something to look at, while everything keeps running:
    /// `link_down` (`interface`, `reason`: `no_ip`, `no_carrier`,
    /// `interface_down`, `interface_missing`) once when the link is lost,
    /// `link_up` (`interface`) when it is back; `send_failed` (`service`,
    /// `count`, `detail`; at most once per second and service, not while
    /// the link is down), `local_network_denied` (`service`; sends refused
    /// with `EHOSTUNREACH`: macOS's Local Network permission is missing;
    /// once per start and again only after a send worked) and
    /// `local_network_allowed` (sends work again after that),
    /// `recv_error`, `bootp_send_failed`,
    /// `tftp_rejected`, `kernels_offline_fallback` (`version`).
    Warning(Failure),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 0x11, 0x22, 0x33]);

    fn roundtrip(e: &EngineEvent) -> serde_json::Value {
        let v = serde_json::to_value(e).unwrap();
        let back: EngineEvent = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(&back, e);
        v
    }

    #[test]
    fn device_events_are_flat_json() {
        let e = EngineEvent::Device {
            mac: MAC,
            state: DeviceState::Flashing {
                version: "7.25.1".into(),
                phase: "uploading".into(),
                sent: 10,
                total: 20,
            },
        };
        assert_eq!(
            roundtrip(&e),
            json!({"event": "device", "mac": "48:A9:8A:11:22:33", "state": "flashing",
                   "version": "7.25.1", "phase": "uploading", "sent": 10, "total": 20})
        );
        let preparing = |version: Option<&str>| EngineEvent::Device {
            mac: MAC,
            state: DeviceState::Preparing {
                version: version.map(String::from),
                config: None,
            },
        };
        assert_eq!(
            roundtrip(&preparing(None)),
            json!({"event": "device", "mac": "48:A9:8A:11:22:33", "state": "preparing"})
        );
        assert_eq!(roundtrip(&preparing(Some("7.25.1")))["version"], "7.25.1");
        let with_config = EngineEvent::Device {
            mac: MAC,
            state: DeviceState::Preparing {
                version: None,
                config: Some("empty".into()),
            },
        };
        assert_eq!(
            roundtrip(&with_config),
            json!({"event": "device", "mac": "48:A9:8A:11:22:33", "state": "preparing",
                   "config": "empty"})
        );
        let failed = EngineEvent::Device {
            mac: MAC,
            state: DeviceState::failed(Failure::new("flash_timeout").with("phase", "offering")),
        };
        assert_eq!(
            roundtrip(&failed),
            json!({"event": "device", "mac": "48:A9:8A:11:22:33", "state": "failed",
                   "code": "flash_timeout", "params": {"phase": "offering"}})
        );
        let waiting = EngineEvent::Device {
            mac: MAC,
            state: DeviceState::Waiting {
                reason: WaitReason::NoProfile,
            },
        };
        assert_eq!(roundtrip(&waiting)["reason"], "no_profile");
    }

    #[test]
    fn other_events_roundtrip() {
        let started = EngineEvent::Started(StartInfo {
            interface: "en7".into(),
            server_ip: Ipv4Addr::new(192, 168, 88, 1),
            bootp: "192.168.88.1:67".parse().unwrap(),
            tftp: "192.168.88.1:69".parse().unwrap(),
            netinstall: "192.168.88.1:5000".parse().unwrap(),
        });
        assert_eq!(roundtrip(&started)["event"], "started");
        assert_eq!(roundtrip(&started)["bootp"], "192.168.88.1:67");
        let err = EngineEvent::Error(Failure::new("interface_gone").with("interface", "en7"));
        assert_eq!(
            roundtrip(&err),
            json!({"event": "error", "code": "interface_gone", "params": {"interface": "en7"}})
        );
        let disc = EngineEvent::Discovered {
            mac: MAC,
            info: DeviceInfoView {
                model: "RB5009".into(),
                arch: "arm64".into(),
                license_id: "X".into(),
                min_os: "7.0".into(),
            },
        };
        assert_eq!(roundtrip(&disc)["model"], "RB5009");
        let kernels = EngineEvent::Kernels {
            state: KernelState::Downloading {
                spec: "stable".into(),
                done: 5,
                total: Some(10),
            },
        };
        assert_eq!(
            roundtrip(&kernels),
            json!({"event": "kernels", "state": "downloading", "spec": "stable",
                   "done": 5, "total": 10})
        );
        let unavailable = EngineEvent::Kernels {
            state: KernelState::Unavailable {
                code: "kernels_unavailable".into(),
                params: [("cause".to_string(), "network".to_string())].into(),
            },
        };
        assert_eq!(
            roundtrip(&unavailable),
            json!({"event": "kernels", "state": "unavailable", "code": "kernels_unavailable",
                   "params": {"cause": "network"}})
        );
        for e in [
            EngineEvent::Kernels {
                state: KernelState::Idle,
            },
            EngineEvent::Kernels {
                state: KernelState::Ready {
                    version: "7.25.1".into(),
                    offline: true,
                },
            },
            EngineEvent::Stopped,
            EngineEvent::KernelsReady {
                version: "7.25.1".into(),
            },
            EngineEvent::Download {
                file: "f".into(),
                done: 1,
                total: None,
            },
            EngineEvent::Ignored {
                mac: MAC,
                reason: "cooldown".into(),
            },
            EngineEvent::Warning(Failure::new("recv_error")),
        ] {
            roundtrip(&e);
        }
    }

    #[test]
    fn forgotten_and_only_mac_events() {
        assert_eq!(
            roundtrip(&EngineEvent::Forgotten { mac: MAC }),
            json!({"event": "forgotten", "mac": "48:A9:8A:11:22:33"})
        );
        assert_eq!(
            roundtrip(&EngineEvent::OnlyMacChanged { mac: Some(MAC) }),
            json!({"event": "only_mac_changed", "mac": "48:A9:8A:11:22:33"})
        );
        assert_eq!(
            roundtrip(&EngineEvent::OnlyMacChanged { mac: None }),
            json!({"event": "only_mac_changed", "mac": null})
        );
    }

    #[test]
    fn failure_display_lists_params() {
        let f = Failure::new("port_in_use")
            .with("service", "bootp")
            .with("port", 67);
        assert_eq!(f.to_string(), "port_in_use (port: 67, service: bootp)");
        assert_eq!(f.param("port"), Some("67"));
        assert_eq!(Failure::new("x").to_string(), "x");
    }

    #[test]
    fn state_names_match_serde_tags() {
        let states = [
            DeviceState::BootpSeen {
                boot_arch: "arm64".into(),
                ip: Ipv4Addr::LOCALHOST,
            },
            DeviceState::KernelServing { sent: 0, total: 1 },
            DeviceState::Discovered,
            DeviceState::Waiting {
                reason: WaitReason::Concurrency,
            },
            DeviceState::Preparing {
                version: None,
                config: None,
            },
            DeviceState::Flashing {
                version: "7.25.1".into(),
                phase: "offering".into(),
                sent: 0,
                total: 0,
            },
            DeviceState::Done,
            DeviceState::failed(Failure::new("x")),
        ];
        for s in states {
            let v = serde_json::to_value(&s).unwrap();
            assert_eq!(v["state"], s.name());
            assert_eq!(s.is_active(), matches!(s.name(), "preparing" | "flashing"));
        }
    }

    #[test]
    fn kernel_state_names_match_serde_tags() {
        for s in [
            KernelState::Idle,
            KernelState::Downloading {
                spec: "7.25.1".into(),
                done: 0,
                total: None,
            },
            KernelState::Ready {
                version: "7.25.1".into(),
                offline: false,
            },
            KernelState::Unavailable {
                code: "kernels_unavailable".into(),
                params: BTreeMap::new(),
            },
        ] {
            assert_eq!(serde_json::to_value(&s).unwrap()["state"], s.name());
        }
    }

    #[test]
    fn optional_macs_are_strings_or_null() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct T {
            #[serde(with = "mac_opt")]
            m: Option<Mac>,
        }
        let some = T { m: Some(MAC) };
        assert_eq!(
            serde_json::to_value(&some).unwrap(),
            json!({"m": "48:A9:8A:11:22:33"})
        );
        assert_eq!(
            serde_json::from_value::<T>(json!({"m": null})).unwrap(),
            T { m: None }
        );
        assert!(serde_json::from_value::<T>(json!({"m": "x"})).is_err());
    }

    #[test]
    fn bad_mac_is_a_deserialization_error() {
        let v = json!({"event": "ignored", "mac": "nope", "reason": "cooldown"});
        assert!(serde_json::from_value::<EngineEvent>(v).is_err());
    }
}
