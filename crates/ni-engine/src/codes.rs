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
    "duplicate_package",
    "engine_gone",
    "flash_rejected",
    "flash_timeout",
    "interface_down",
    "interface_gone",
    "interface_list_failed",
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
        let names: Vec<_> = states.iter().map(DeviceState::name).collect();
        assert_eq!(names, DEVICE_STATES);
        let waits: Vec<_> = [WaitReason::NoProfile, WaitReason::Concurrency]
            .iter()
            .map(|w| serde_json::to_value(w).unwrap())
            .collect();
        assert_eq!(waits, WAIT_REASONS);
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
        let names: Vec<_> = kernels.iter().map(KernelState::name).collect();
        assert_eq!(names, KERNEL_STATES);
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
        let names: Vec<_> = phases.iter().map(Phase::name).collect();
        assert_eq!(names, PHASES);
        let kinds = [
            InterfaceKind::Ethernet,
            InterfaceKind::WiFi,
            InterfaceKind::Loopback,
            InterfaceKind::Bridge,
            InterfaceKind::AppleWireless,
            InterfaceKind::Tunnel,
            InterfaceKind::Other,
        ];
        let names: Vec<_> = kinds.iter().map(|k| k.as_str()).collect();
        assert_eq!(names, INTERFACE_KINDS);
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
