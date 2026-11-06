//! Every stable code the engine reports, so a UI can check that it
//! translates all of them ([`catalog`]).
//!
//! The test below scans this crate's sources: a new `Failure::new("…")`,
//! `ignored` reason or link-down reason that is not listed here fails it.

use serde::Serialize;

/// [`crate::Failure`] codes: command errors, device failures (`failed`
/// states), engine `error` and `warning` events, and preflight findings.
pub const FAILURES: &[&str] = &[
    "already_running",
    "assets_failed",
    "bind_failed",
    "bootp_send_failed",
    "cancelled",
    "default_route",
    "device_busy",
    "device_done",
    "device_in_cooldown",
    "duplicate_package",
    "engine_gone",
    "flash_rejected",
    "flash_timeout",
    "interface_down",
    "interface_gone",
    "interface_list_failed",
    "interface_not_ethernet",
    "invalid_cooldown",
    "invalid_max_parallel",
    "invalid_name",
    "invalid_netmask",
    "invalid_network",
    "invalid_package_name",
    "invalid_plan",
    "invalid_profile",
    "invalid_script_name",
    "kernel_transfer_failed",
    "kernel_version_unknown",
    "kernels_not_cached",
    "kernels_offline_fallback",
    "kernels_unavailable",
    "link_down",
    "link_up",
    "no_ipv4",
    "no_mac",
    "no_profile",
    "not_cancellable",
    "not_failed",
    "not_running",
    "pool_includes_reserved",
    "pool_inverted",
    "pool_outside_subnet",
    "port_in_use",
    "profile_io",
    "profile_not_found",
    "profile_version_mismatch",
    "recv_error",
    "runner_failed",
    "script_missing",
    "send_failed",
    "server_ip_not_host",
    "server_ip_not_on_interface",
    "tftp_rejected",
    "too_many_packages",
    "unknown_device",
    "unknown_interface",
    "unsuitable_interface",
    "version_unavailable",
];

/// `reason` of an `ignored` event.
pub const IGNORED_REASONS: &[&str] = &[
    "awaiting_bootp",
    "cooldown",
    "no_boot_file",
    "not_selected",
    "pool_exhausted",
    "unsupported_device",
];

/// `reason` of a `link_down` warning (and snapshot `link_down`).
pub const LINK_REASONS: &[&str] = &["interface_down", "interface_missing", "no_carrier", "no_ip"];

/// [`crate::DeviceState`] names.
pub const DEVICE_STATES: &[&str] = &[
    "bootp_seen",
    "kernel_serving",
    "discovered",
    "waiting",
    "preparing",
    "flashing",
    "done",
    "failed",
];

/// [`crate::WaitReason`] values.
pub const WAIT_REASONS: &[&str] = &["no_profile", "concurrency"];

/// [`crate::KernelState`] names.
pub const KERNEL_STATES: &[&str] = &["idle", "downloading", "ready", "unavailable"];

/// `phase` of a `flashing` state (and of `flash_timeout` /
/// `flash_rejected`): [`ni_proto::Phase::name`].
pub const PHASES: &[&str] = &[
    "offering",
    "formatting",
    "preparing",
    "uploading",
    "finishing",
    "rebooting",
    "done",
    "failed",
];

/// `kind` of an interface (and of `unsuitable_interface`):
/// [`ni_net::InterfaceKind::as_str`].
pub const INTERFACE_KINDS: &[&str] = &[
    "ethernet",
    "thunderbolt_ip",
    "virtual",
    "wifi",
    "loopback",
    "bridge",
    "apple_wireless",
    "tunnel",
    "other",
];

/// `service` parameter values.
pub const SERVICES: &[&str] = &["bootp", "tftp", "netinstall"];

/// Every code list, as one serializable value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Catalog {
    pub failures: Vec<&'static str>,
    /// `cause` of `assets_failed`, `kernels_unavailable`,
    /// `version_unavailable`, `script_missing`: [`ni_assets::AssetError::code`].
    pub asset_causes: Vec<&'static str>,
    pub ignored_reasons: Vec<&'static str>,
    pub link_reasons: Vec<&'static str>,
    pub device_states: Vec<&'static str>,
    pub wait_reasons: Vec<&'static str>,
    pub kernel_states: Vec<&'static str>,
    pub phases: Vec<&'static str>,
    pub interface_kinds: Vec<&'static str>,
    pub services: Vec<&'static str>,
}

/// All codes.
pub fn catalog() -> Catalog {
    Catalog {
        failures: FAILURES.to_vec(),
        asset_causes: ni_assets::AssetError::CODES.to_vec(),
        ignored_reasons: IGNORED_REASONS.to_vec(),
        link_reasons: LINK_REASONS.to_vec(),
        device_states: DEVICE_STATES.to_vec(),
        wait_reasons: WAIT_REASONS.to_vec(),
        kernel_states: KERNEL_STATES.to_vec(),
        phases: PHASES.to_vec(),
        interface_kinds: INTERFACE_KINDS.to_vec(),
        services: SERVICES.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DeviceState, Failure, KernelState, WaitReason};
    use ni_net::InterfaceKind;
    use ni_proto::Phase;
    use std::collections::BTreeSet;

    /// This crate's sources, without their test modules.
    const SOURCES: &[&str] = &[
        include_str!("config.rs"),
        include_str!("engine/core.rs"),
        include_str!("engine/device.rs"),
        include_str!("engine/mod.rs"),
        include_str!("engine/start.rs"),
        include_str!("event.rs"),
        include_str!("gate.rs"),
        include_str!("preflight.rs"),
        include_str!("prepare.rs"),
        include_str!("profile.rs"),
    ];

    fn code_only(src: &str) -> &str {
        src.split("#[cfg(test)]\nmod tests").next().unwrap()
    }

    /// The string literals following each `prefix` in the sources.
    fn literals_after(prefix: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for src in SOURCES.iter().map(|s| code_only(s)) {
            for (i, _) in src.match_indices(prefix) {
                let rest = &src[i + prefix.len()..];
                if let Some(end) = rest.find('"') {
                    out.insert(rest[..end].to_string());
                }
            }
        }
        out
    }

    fn assert_listed(found: BTreeSet<String>, list: &[&str], what: &str) {
        assert!(!found.is_empty(), "no {what} found: is the scan broken?");
        for code in found {
            assert!(
                list.contains(&code.as_str()),
                "{what} {code:?} is not listed"
            );
        }
    }

    #[test]
    fn every_failure_in_the_sources_is_listed() {
        let mut found = literals_after("Failure::new(\"");
        found.extend(literals_after("asset_failure(\""));
        assert_listed(found, FAILURES, "failure code");
    }

    #[test]
    fn every_listed_failure_appears_in_the_sources() {
        let all: String = SOURCES.iter().map(|s| code_only(s)).collect();
        for code in FAILURES {
            assert!(all.contains(&format!("\"{code}\"")), "{code} is stale");
        }
    }

    #[test]
    fn ignored_and_link_reasons_are_listed() {
        let mut ignored = literals_after("ignored(mac, \"");
        ignored.extend(["cooldown".to_string(), "not_selected".to_string()]);
        assert_listed(ignored, IGNORED_REASONS, "ignored reason");
        assert_eq!(crate::gate::Verdict::Cooldown.reason(), "cooldown");
        assert_eq!(crate::gate::Verdict::NotSelected.reason(), "not_selected");
        let mut link = literals_after("Down(\"");
        link.extend(literals_after("link_down(\""));
        assert_listed(link, LINK_REASONS, "link reason");
    }

    /// `samples` must hold one value of every variant, in the order of
    /// `index` (an exhaustive `match` without a wildcard arm: a new variant
    /// does not compile until it gets an index, and then fails here until
    /// it has a sample and a name in the list).
    fn names_in_order<T>(
        samples: &[T],
        index: fn(&T) -> usize,
        name: fn(&T) -> String,
        list: &[&str],
        what: &str,
    ) {
        let indices: Vec<usize> = samples.iter().map(index).collect();
        let want: Vec<usize> = (0..samples.len()).collect();
        assert_eq!(indices, want, "{what}: one sample per variant, in order");
        let names: Vec<String> = samples.iter().map(name).collect();
        assert_eq!(names, list, "{what}");
    }

    fn device_state_index(s: &DeviceState) -> usize {
        match s {
            DeviceState::BootpSeen { .. } => 0,
            DeviceState::KernelServing { .. } => 1,
            DeviceState::Discovered => 2,
            DeviceState::Waiting { .. } => 3,
            DeviceState::Preparing { .. } => 4,
            DeviceState::Flashing { .. } => 5,
            DeviceState::Done => 6,
            DeviceState::Failed { .. } => 7,
        }
    }

    fn wait_reason_index(w: &WaitReason) -> usize {
        match w {
            WaitReason::NoProfile => 0,
            WaitReason::Concurrency => 1,
        }
    }

    fn kernel_state_index(k: &KernelState) -> usize {
        match k {
            KernelState::Idle => 0,
            KernelState::Downloading { .. } => 1,
            KernelState::Ready { .. } => 2,
            KernelState::Unavailable { .. } => 3,
        }
    }

    fn phase_index(p: &Phase) -> usize {
        match p {
            Phase::Offering => 0,
            Phase::Formatting => 1,
            Phase::Preparing => 2,
            Phase::Uploading { .. } => 3,
            Phase::Finishing => 4,
            Phase::Rebooting => 5,
            Phase::Done => 6,
            Phase::Failed => 7,
        }
    }

    fn interface_kind_index(k: &InterfaceKind) -> usize {
        match k {
            InterfaceKind::Ethernet => 0,
            InterfaceKind::ThunderboltIp => 1,
            InterfaceKind::Virtual => 2,
            InterfaceKind::WiFi => 3,
            InterfaceKind::Loopback => 4,
            InterfaceKind::Bridge => 5,
            InterfaceKind::AppleWireless => 6,
            InterfaceKind::Tunnel => 7,
            InterfaceKind::Other => 8,
        }
    }

    #[test]
    fn enum_names_match_the_lists() {
        let states = [
            DeviceState::BootpSeen {
                boot_arch: "arm64".into(),
                ip: std::net::Ipv4Addr::LOCALHOST,
            },
            DeviceState::KernelServing { sent: 0, total: 0 },
            DeviceState::Discovered,
            DeviceState::Waiting {
                reason: WaitReason::NoProfile,
            },
            DeviceState::Preparing { version: None },
            DeviceState::Flashing {
                version: String::new(),
                phase: String::new(),
                sent: 0,
                total: 0,
            },
            DeviceState::Done,
            DeviceState::failed(Failure::new("cancelled")),
        ];
        names_in_order(
            &states,
            device_state_index,
            |s| s.name().to_string(),
            DEVICE_STATES,
            "device states",
        );
        names_in_order(
            &[WaitReason::NoProfile, WaitReason::Concurrency],
            wait_reason_index,
            |w| {
                serde_json::to_value(w)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            },
            WAIT_REASONS,
            "wait reasons",
        );
        let kernels = [
            KernelState::Idle,
            KernelState::Downloading {
                spec: String::new(),
                done: 0,
                total: None,
            },
            KernelState::Ready {
                version: String::new(),
                offline: false,
            },
            KernelState::Unavailable {
                code: String::new(),
                params: Default::default(),
            },
        ];
        names_in_order(
            &kernels,
            kernel_state_index,
            |k| k.name().to_string(),
            KERNEL_STATES,
            "kernel states",
        );
        let phases = [
            Phase::Offering,
            Phase::Formatting,
            Phase::Preparing,
            Phase::Uploading { file: "f".into() },
            Phase::Finishing,
            Phase::Rebooting,
            Phase::Done,
            Phase::Failed,
        ];
        names_in_order(
            &phases,
            phase_index,
            |p| p.name().to_string(),
            PHASES,
            "phases",
        );
        let kinds = [
            InterfaceKind::Ethernet,
            InterfaceKind::ThunderboltIp,
            InterfaceKind::Virtual,
            InterfaceKind::WiFi,
            InterfaceKind::Loopback,
            InterfaceKind::Bridge,
            InterfaceKind::AppleWireless,
            InterfaceKind::Tunnel,
            InterfaceKind::Other,
        ];
        names_in_order(
            &kinds,
            interface_kind_index,
            |k| k.as_str().to_string(),
            INTERFACE_KINDS,
            "interface kinds",
        );
    }

    #[test]
    fn lists_have_no_duplicates() {
        let c = catalog();
        for list in [
            &c.failures,
            &c.asset_causes,
            &c.ignored_reasons,
            &c.link_reasons,
            &c.device_states,
            &c.wait_reasons,
            &c.kernel_states,
            &c.phases,
            &c.interface_kinds,
            &c.services,
        ] {
            let set: BTreeSet<_> = list.iter().collect();
            assert_eq!(set.len(), list.len(), "{list:?}");
        }
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["asset_causes"][0], "invalid_version");
    }
}
