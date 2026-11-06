//! Checks before `start`: interface, addresses, ports, assets.
//!
//! The pure parts ([`check_network`]) take an interface list, so they are
//! tested without touching the machine; [`check_ports`] binds and releases
//! the listeners exactly as `start` would (no `SO_REUSEPORT`), so a running
//! DHCP/TFTP server (e.g. Internet Sharing's `bootpd`, launchd `tftpd`) or
//! another netinstall shows up as `port_in_use`.
//!
//! Not checkable: macOS's Local Network permission cannot be queried by an
//! app. Without it, sends to the local network fail with `EHOSTUNREACH`
//! (reported as `send_failed` / `bootp_send_failed` / `recv_error`
//! warnings later).

use crate::config::{
    check_pool_reserved, link_status, resolve_addresses, subnet, EngineConfig, LinkStatus, NetPlan,
};
use crate::event::Failure;
use crate::profile::Profile;
use ni_assets::Assets;
use ni_bootp::{BootpConfig, BootpResponder};
use ni_net::{bind_udp, BindOptions, Interface, InterfaceKind, NetError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// `start` would fail or devices could not boot.
    Error,
    /// Works, but probably not what was meant.
    Warning,
}

/// One preflight result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    #[serde(flatten)]
    pub failure: Failure,
}

impl Finding {
    fn error(f: Failure) -> Self {
        Finding {
            severity: Severity::Error,
            failure: f,
        }
    }

    fn warning(f: Failure) -> Self {
        Finding {
            severity: Severity::Warning,
            failure: f,
        }
    }
}

/// All findings, plus the resolved network if the interface allowed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightReport {
    pub findings: Vec<Finding>,
    pub network: Option<NetPlan>,
}

impl PreflightReport {
    /// No [`Severity::Error`] findings.
    pub fn ok(&self) -> bool {
        self.findings.iter().all(|f| f.severity != Severity::Error)
    }
}

/// Interface and address checks. Errors: `unknown_interface`,
/// `interface_down`, `no_ipv4`, `server_ip_not_on_interface`, `no_mac`,
/// `invalid_netmask`, `server_ip_not_host`, `pool_inverted`,
/// `pool_outside_subnet`, `pool_includes_reserved` (the network or
/// broadcast address), `invalid_network` (what `ni-bootp` refuses, e.g. a
/// pool with no usable address; `detail`). Warnings: `link_down`
/// (`reason`: `no_ip`, `no_carrier`; no cable yet? With a configured
/// server IP a missing address is only this warning: macOS removes a
/// manual address while there is no carrier), `unsuitable_interface`
/// (`kind`: loopback; for tests). Error: `interface_not_ethernet`
/// (`interface`, `kind`): see [`not_ethernet`]; `no_manual_ipv4` /
/// `ipv4_dhcp` (`interface`, `service`: the adapter's name in System
/// Settings) instead of `no_ipv4` when no address can be had: macOS is not
/// configured with a manual IPv4 for the adapter (`ipv4_dhcp` is only a
/// warning while it has a DHCP address).
/// `interface_not_ethernet` (`interface`, `kind`) unless `iface` can
/// netinstall a RouterBOARD ([`ni_net::Interface::is_netinstall_capable`]:
/// a real wired Ethernet NIC). Loopback is let through: tests and the
/// simulator run on `lo0`, and nothing real can boot from it anyway.
pub fn not_ethernet(iface: &Interface) -> Option<Failure> {
    (!iface.is_netinstall_capable() && iface.kind != InterfaceKind::Loopback).then(|| {
        Failure::new("interface_not_ethernet")
            .with("interface", &iface.name)
            .with("kind", iface.kind.as_str())
    })
}

pub fn check_network(ifaces: &[Interface], cfg: &EngineConfig) -> (Vec<Finding>, Option<NetPlan>) {
    let mut out = Vec::new();
    let Some(iface) = ifaces.iter().find(|i| i.name == cfg.interface) else {
        let f = Failure::new("unknown_interface").with("interface", &cfg.interface);
        return (vec![Finding::error(f)], None);
    };
    let resolved = resolve_addresses(ifaces, cfg);
    if !iface.is_up {
        out.push(Finding::error(
            Failure::new("interface_down").with("interface", &iface.name),
        ));
    } else {
        let reason = match &resolved {
            Ok(plan) => match link_status(ifaces, &iface.name, plan.server_ip) {
                LinkStatus::Down(reason) => Some(reason),
                _ => None,
            },
            Err(_) => (!iface.is_running).then_some("no_carrier"),
        };
        if let Some(reason) = reason {
            out.push(Finding::warning(
                Failure::new("link_down")
                    .with("interface", &iface.name)
                    .with("reason", reason),
            ));
        }
    }
    if let Some(f) = not_ethernet(iface) {
        out.push(if cfg.allow_non_ethernet {
            Finding::warning(f)
        } else {
            Finding::error(f)
        });
    } else if iface.kind != InterfaceKind::Ethernet {
        // Loopback: tests and simulations only.
        out.push(Finding::warning(
            Failure::new("unsuitable_interface")
                .with("interface", &iface.name)
                .with("kind", iface.kind.as_str()),
        ));
    }
    let method = iface.configured_ipv4.as_ref().map(|c| c.method.clone());
    let dhcp = || {
        Failure::new("ipv4_dhcp")
            .with("interface", &iface.name)
            .with(
                "service",
                iface.display_name.as_deref().unwrap_or(&iface.name),
            )
    };
    let plan = match resolved {
        Ok(plan) => plan,
        // No address to serve with: say what to set up in macOS.
        Err(f) if f.code == "no_ipv4" && cfg.server_ip.is_none() => {
            out.push(Finding::error(match method {
                Some(ni_net::Ipv4Method::Dhcp) => dhcp(),
                _ => Failure::new("no_manual_ipv4")
                    .with("interface", &iface.name)
                    .with(
                        "service",
                        iface.display_name.as_deref().unwrap_or(&iface.name),
                    ),
            }));
            return (out, None);
        }
        Err(f) => {
            out.push(Finding::error(f));
            return (out, None);
        }
    };
    if cfg.server_ip.is_none() && method == Some(ni_net::Ipv4Method::Dhcp) {
        // An address from someone else's DHCP server: works, but the
        // network is probably not a separate one for flashing.
        out.push(Finding::warning(dhcp()));
    }
    let mask = u32::from(plan.netmask);
    if mask.leading_ones() + mask.trailing_zeros() != 32 {
        out.push(Finding::error(
            Failure::new("invalid_netmask").with("netmask", plan.netmask),
        ));
        return (out, None);
    }
    let errors_before = out.iter().filter(|f| f.severity == Severity::Error).count();
    let (net, bcast) = subnet(plan.server_ip, plan.netmask);
    if mask.leading_ones() <= 30 && (plan.server_ip == net || plan.server_ip == bcast) {
        out.push(Finding::error(
            Failure::new("server_ip_not_host").with("ip", plan.server_ip),
        ));
    }
    if u32::from(plan.pool_start) > u32::from(plan.pool_end) {
        out.push(Finding::error(
            Failure::new("pool_inverted")
                .with("start", plan.pool_start)
                .with("end", plan.pool_end),
        ));
    }
    let inside = |ip| subnet(ip, plan.netmask).0 == net;
    if !inside(plan.pool_start) || !inside(plan.pool_end) {
        out.push(Finding::error(
            Failure::new("pool_outside_subnet")
                .with("start", plan.pool_start)
                .with("end", plan.pool_end)
                .with("network", format!("{net}/{}", mask.leading_ones())),
        ));
    }
    if out.iter().filter(|f| f.severity == Severity::Error).count() == errors_before {
        if let Err(f) = check_pool_reserved(&plan) {
            out.push(Finding::error(f));
        } else if let Err(e) = BootpResponder::new(BootpConfig {
            server_ip: plan.server_ip,
            netmask: plan.netmask,
            pool_start: plan.pool_start,
            pool_end: plan.pool_end,
            boot_files: Default::default(),
            lease_secs: cfg.lease_secs,
        }) {
            out.push(Finding::error(
                Failure::new("invalid_network").with("detail", e),
            ));
        }
    }
    (out, Some(plan))
}

/// Warning `default_route` (`interface`) if the boot interface carries an
/// IPv4 default route (`routed`: see [`ni_net::default_route_interfaces`]):
/// it is probably a real LAN, where the BOOTP pool may collide with live
/// hosts and another DHCP server may answer the devices.
pub fn check_default_route(routed: &BTreeSet<String>, cfg: &EngineConfig) -> Option<Finding> {
    routed
        .contains(&cfg.interface)
        .then(|| Finding::warning(Failure::new("default_route").with("interface", &cfg.interface)))
}

/// Bind (and release) each listener as `start` would. Errors:
/// `port_in_use` (`service`, `port`), `bind_failed` (`service`, `detail`).
/// Ports given as 0 are skipped (they cannot collide).
pub fn check_ports(cfg: &EngineConfig) -> Vec<Finding> {
    let p = &cfg.ports;
    [
        ("bootp", p.bootp),
        ("tftp", p.tftp),
        ("netinstall", p.netinstall),
    ]
    .into_iter()
    .filter(|(_, port)| *port != 0)
    .filter_map(|(service, port)| {
        let opts = BindOptions::new(port)
            .addr(p.bind_ip)
            .interface(cfg.interface.clone());
        bind_udp(&opts)
            .err()
            .map(|e| Finding::error(bind_failure(service, &e)))
    })
    .collect()
}

/// A failed listener bind as a [`Failure`]: `port_in_use` for
/// `AddrInUse`, `unknown_interface`, else `bind_failed`.
pub fn bind_failure(service: &str, e: &NetError) -> Failure {
    match e {
        NetError::AddrInUse { port } => Failure::new("port_in_use")
            .with("service", service)
            .with("port", port),
        NetError::UnknownInterface(name) => {
            Failure::new("unknown_interface").with("interface", name)
        }
        other => Failure::new("bind_failed")
            .with("service", service)
            .with("detail", other),
    }
}

/// Can the profile's assets be had? Errors: `version_unavailable`
/// (`cause`: the asset error code), `script_missing`. Warning:
/// `kernels_not_cached` (they will be downloaded at start).
pub async fn check_assets(assets: &Assets, profile: &Profile) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Err(f) = profile.validate() {
        return vec![Finding::error(f)];
    }
    match assets.resolve(&profile.version).await {
        Err(e) => out.push(Finding::error(
            Failure::new("version_unavailable")
                .with("cause", e.code())
                .with("detail", &e),
        )),
        Ok(version) => {
            let cached = assets.cached_versions().await.unwrap_or_default();
            if !cached.contains(&version) {
                out.push(Finding::warning(
                    Failure::new("kernels_not_cached").with("version", &version),
                ));
            }
        }
    }
    for local_name in profile.scripts() {
        if let Err(e) = assets.local_file(local_name).await {
            out.push(Finding::error(
                Failure::new("script_missing")
                    .with("name", local_name)
                    .with("cause", e.code()),
            ));
        }
    }
    out
}

/// Everything: network (with `cfg.interfaces`), the default route, ports,
/// and the assets of `profile` if given.
/// The quick, local part of [`run`]: the interface, its link and
/// addresses, and the default route. Binds no port and needs no network,
/// so it can run every few seconds.
pub fn run_local(cfg: &EngineConfig) -> PreflightReport {
    let ifaces = cfg.interfaces.list().unwrap_or_default();
    let (mut findings, network) = check_network(&ifaces, cfg);
    if network.is_some() {
        let routed = ni_net::default_route_interfaces().unwrap_or_default();
        findings.extend(check_default_route(&routed, cfg));
    }
    PreflightReport { findings, network }
}

pub async fn run(
    cfg: &EngineConfig,
    assets: Option<&Assets>,
    profile: Option<&Profile>,
) -> PreflightReport {
    let ifaces = cfg.interfaces.list().unwrap_or_default();
    let (mut findings, network) = check_network(&ifaces, cfg);
    if network.is_some() {
        let routed = ni_net::default_route_interfaces().unwrap_or_default();
        findings.extend(check_default_route(&routed, cfg));
        findings.extend(check_ports(cfg));
    }
    if let (Some(assets), Some(profile)) = (assets, profile) {
        findings.extend(check_assets(assets, profile).await);
    }
    PreflightReport { findings, network }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_iface as iface;
    use crate::profile::ConfigMode;
    use ni_assets::testutil::{assets, mirror, tempdir, TestServer};
    use ni_assets::{Channel, VersionSpec};
    use ni_proto::Mac;
    use std::net::Ipv4Addr;

    const MASK24: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
    const MAC: Mac = Mac([2, 0, 0, 0, 0, 1]);

    fn ip(last: u8) -> Ipv4Addr {
        Ipv4Addr::new(192, 168, 88, last)
    }

    fn codes(findings: &[Finding]) -> Vec<(&str, Severity)> {
        findings
            .iter()
            .map(|f| (f.failure.code.as_str(), f.severity))
            .collect()
    }

    #[test]
    fn healthy_ethernet_has_no_findings() {
        let ifaces = [iface("en7", &[(ip(1), MASK24)], Some(MAC))];
        let (findings, plan) = check_network(&ifaces, &EngineConfig::new("en7"));
        assert_eq!(findings, []);
        assert_eq!(plan.unwrap().pool_end, ip(254));
    }

    #[test]
    fn interface_problems() {
        let mut en7 = iface("en7", &[(ip(1), MASK24)], Some(MAC));
        en7.is_running = false;
        en7.kind = InterfaceKind::WiFi;
        let (findings, plan) = check_network(&[en7.clone()], &EngineConfig::new("en7"));
        assert_eq!(
            codes(&findings),
            [
                ("link_down", Severity::Warning),
                ("interface_not_ethernet", Severity::Error)
            ]
        );
        assert_eq!(findings[0].failure.param("reason"), Some("no_carrier"));
        assert_eq!(findings[1].failure.param("kind"), Some("wifi"));
        assert!(plan.is_some());
        // The advanced override makes it a warning.
        let mut port = iface("en1", &[(ip(1), MASK24)], Some(MAC));
        port.kind = InterfaceKind::Virtual;
        let mut cfg = EngineConfig::new("en1");
        cfg.allow_non_ethernet = true;
        let (findings, plan) = check_network(&[port], &cfg);
        assert_eq!(
            codes(&findings),
            [("interface_not_ethernet", Severity::Warning)]
        );
        assert!(plan.is_some());
        for kind in [
            InterfaceKind::ThunderboltIp,
            InterfaceKind::Virtual,
            InterfaceKind::Bridge,
        ] {
            let mut port = iface("en1", &[(ip(1), MASK24)], Some(MAC));
            port.kind = kind;
            let (findings, _) = check_network(&[port], &EngineConfig::new("en1"));
            assert_eq!(
                codes(&findings),
                [("interface_not_ethernet", Severity::Error)],
                "{kind:?}"
            );
        }
        en7.is_up = false;
        en7.ipv4.clear();
        let (findings, plan) = check_network(&[en7], &EngineConfig::new("en7"));
        assert_eq!(
            codes(&findings),
            [
                ("interface_down", Severity::Error),
                ("interface_not_ethernet", Severity::Error),
                ("no_manual_ipv4", Severity::Error)
            ]
        );
        assert_eq!(plan, None);
        let (findings, _) = check_network(&[], &EngineConfig::new("en9"));
        assert_eq!(codes(&findings), [("unknown_interface", Severity::Error)]);
    }

    fn configured(method: ni_net::Ipv4Method) -> Option<ni_net::ConfiguredIpv4> {
        let manual = method == ni_net::Ipv4Method::Manual;
        Some(ni_net::ConfiguredIpv4 {
            method,
            address: manual.then_some(ip(1)),
            netmask: manual.then_some(MASK24),
        })
    }

    #[test]
    fn missing_manual_ipv4_is_explained_by_how_macos_is_configured() {
        let mut en7 = iface("en7", &[], Some(MAC));
        en7.display_name = Some("USB 10/100/1000 LAN".into());
        let cfg = EngineConfig::new("en7");
        // Nothing configured.
        let (findings, plan) = check_network(&[en7.clone()], &cfg);
        assert_eq!(codes(&findings), [("no_manual_ipv4", Severity::Error)]);
        assert_eq!(
            findings[0].failure.param("service"),
            Some("USB 10/100/1000 LAN")
        );
        assert_eq!(plan, None);
        // DHCP.
        en7.configured_ipv4 = configured(ni_net::Ipv4Method::Dhcp);
        let (findings, _) = check_network(&[en7.clone()], &cfg);
        assert_eq!(codes(&findings), [("ipv4_dhcp", Severity::Error)]);
        // DHCP with an address (from someone's DHCP server): a warning.
        en7.ipv4 = vec![(ip(50), MASK24)];
        let (findings, plan) = check_network(&[en7.clone()], &cfg);
        assert_eq!(codes(&findings), [("ipv4_dhcp", Severity::Warning)]);
        assert!(plan.is_some());
        // Manual, cable out: start is possible, the link is only awaited.
        en7.ipv4.clear();
        en7.is_running = false;
        en7.configured_ipv4 = configured(ni_net::Ipv4Method::Manual);
        let (findings, plan) = check_network(&[en7], &cfg);
        assert_eq!(codes(&findings), [("link_down", Severity::Warning)]);
        assert_eq!(findings[0].failure.param("reason"), Some("no_ip"));
        assert_eq!(plan.unwrap().server_ip, ip(1));
    }

    #[test]
    fn address_problems() {
        let ifaces = [iface("en7", &[(ip(0), MASK24), (ip(5), MASK24)], Some(MAC))];
        let (findings, _) = check_network(&ifaces, &EngineConfig::new("en7"));
        assert_eq!(codes(&findings), [("server_ip_not_host", Severity::Error)]);
        let mut cfg = EngineConfig::new("en7");
        cfg.server_ip = Some(ip(5));
        cfg.pool = Some((ip(200), Ipv4Addr::new(192, 168, 89, 10)));
        // (outside the subnet; the reserved-address check is skipped then)
        let (findings, _) = check_network(&ifaces, &cfg);
        assert_eq!(codes(&findings), [("pool_outside_subnet", Severity::Error)]);
        assert_eq!(
            findings[0].failure.param("network"),
            Some("192.168.88.0/24")
        );
        cfg.pool = Some((ip(20), ip(10)));
        let (findings, _) = check_network(&ifaces, &cfg);
        assert_eq!(codes(&findings), [("pool_inverted", Severity::Error)]);
        let odd = [iface(
            "en7",
            &[(ip(5), Ipv4Addr::new(255, 0, 255, 0))],
            Some(MAC),
        )];
        let (findings, plan) = check_network(&odd, &EngineConfig::new("en7"));
        assert_eq!(codes(&findings), [("invalid_netmask", Severity::Error)]);
        assert_eq!(plan, None);
    }

    #[test]
    fn missing_link_is_a_warning_with_a_configured_server_ip() {
        let mut en7 = iface("en7", &[], Some(MAC));
        en7.is_running = false;
        let mut cfg = EngineConfig::new("en7");
        let (findings, plan) = check_network(&[en7.clone()], &cfg);
        assert_eq!(
            codes(&findings),
            [
                ("link_down", Severity::Warning),
                ("no_manual_ipv4", Severity::Error)
            ]
        );
        assert_eq!(plan, None);
        cfg.server_ip = Some(ip(1));
        let (findings, plan) = check_network(&[en7.clone()], &cfg);
        assert_eq!(codes(&findings), [("link_down", Severity::Warning)]);
        assert_eq!(findings[0].failure.param("reason"), Some("no_ip"));
        assert_eq!(plan.unwrap().server_ip, ip(1));
        // Carrier but the address not (yet) there: still only a warning.
        en7.is_running = true;
        let (findings, _) = check_network(&[en7], &cfg);
        assert_eq!(codes(&findings), [("link_down", Severity::Warning)]);
    }

    #[test]
    fn pools_with_reserved_or_no_usable_addresses_are_errors() {
        let ifaces = [iface("en7", &[(ip(1), MASK24)], Some(MAC))];
        let mut cfg = EngineConfig::new("en7");
        cfg.pool = Some((ip(0), ip(20)));
        let (findings, _) = check_network(&ifaces, &cfg);
        assert_eq!(
            codes(&findings),
            [("pool_includes_reserved", Severity::Error)]
        );
        assert_eq!(findings[0].failure.param("network"), Some("192.168.88.0"));
        // Only the server's own address: ni-bootp has nothing to hand out.
        cfg.pool = Some((ip(1), ip(1)));
        let (findings, _) = check_network(&ifaces, &cfg);
        assert_eq!(codes(&findings), [("invalid_network", Severity::Error)]);
    }

    #[test]
    fn a_default_route_on_the_interface_is_a_warning() {
        let cfg = EngineConfig::new("en7");
        let routed: BTreeSet<String> = ["en0".to_string()].into();
        assert_eq!(check_default_route(&routed, &cfg), None);
        let routed: BTreeSet<String> = ["en0".to_string(), "en7".to_string()].into();
        let f = check_default_route(&routed, &cfg).unwrap();
        assert_eq!(
            (f.severity, f.failure.code.as_str()),
            (Severity::Warning, "default_route")
        );
    }

    #[tokio::test]
    async fn port_held_by_another_server_is_port_in_use() {
        let held = bind_udp(&BindOptions::new(0).addr(Ipv4Addr::LOCALHOST)).unwrap();
        let port = held.local_addr().unwrap().port();
        let mut cfg = EngineConfig::new("lo0");
        cfg.ports.bind_ip = Ipv4Addr::LOCALHOST;
        cfg.ports.bootp = 0;
        cfg.ports.tftp = port;
        cfg.ports.netinstall = 0;
        let findings = check_ports(&cfg);
        assert_eq!(codes(&findings), [("port_in_use", Severity::Error)]);
        assert_eq!(findings[0].failure.param("service"), Some("tftp"));
        assert_eq!(
            findings[0].failure.param("port"),
            Some(port.to_string().as_str())
        );
        drop(held);
        assert_eq!(check_ports(&cfg), []);
        cfg.interface = "nosuch0".into();
        assert_eq!(
            codes(&check_ports(&cfg)),
            [("unknown_interface", Severity::Error)]
        );
    }

    #[tokio::test]
    async fn the_local_check_binds_no_port_and_needs_no_assets() {
        let held = bind_udp(&BindOptions::new(0).addr(Ipv4Addr::LOCALHOST)).unwrap();
        let mut cfg = EngineConfig::new("lo0");
        cfg.server_mac = Some(MAC);
        cfg.ports.bind_ip = Ipv4Addr::LOCALHOST;
        cfg.ports.bootp = 0;
        cfg.ports.tftp = held.local_addr().unwrap().port();
        cfg.ports.netinstall = 0;
        let full = run(&cfg, None, None).await;
        assert!(full
            .findings
            .iter()
            .any(|f| f.failure.code == "port_in_use"));
        let local = run_local(&cfg);
        assert!(local
            .findings
            .iter()
            .all(|f| f.failure.code != "port_in_use"));
        assert_eq!(
            codes(&local.findings),
            [("unsuitable_interface", Severity::Warning)]
        );
        assert_eq!(local.network, full.network);
    }

    #[tokio::test]
    async fn assets_check_reports_cache_state_and_missing_script() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let dir = tempdir("preflight-assets");
        let a = assets(&srv, &dir);
        let mut profile = Profile::new("p", VersionSpec::Channel(Channel::Stable));
        let findings = check_assets(&a, &profile).await;
        assert_eq!(
            codes(&findings),
            [("kernels_not_cached", Severity::Warning)]
        );
        a.ensure_kernels(&a.resolve(&profile.version).await.unwrap())
            .await
            .unwrap();
        assert_eq!(check_assets(&a, &profile).await, []);
        profile.config = ConfigMode::Script {
            local_name: "setup.rsc".into(),
        };
        profile.mode_script = Some("mode.rsc".into());
        let findings = check_assets(&a, &profile).await;
        assert_eq!(
            codes(&findings),
            [
                ("script_missing", Severity::Error),
                ("script_missing", Severity::Error)
            ]
        );
        assert_eq!(findings[1].failure.param("name"), Some("mode.rsc"));
        profile.mode_script = None;
        profile.version = VersionSpec::Channel(Channel::Testing);
        profile.config = ConfigMode::Default;
        let findings = check_assets(&a, &profile).await;
        assert_eq!(codes(&findings), [("version_unavailable", Severity::Error)]);
        assert_eq!(findings[0].failure.param("cause"), Some("not_found"));
    }

    #[tokio::test]
    async fn run_on_loopback_warns_about_the_interface_kind() {
        let mut cfg = EngineConfig::new("lo0");
        cfg.server_mac = Some(MAC);
        cfg.ports.bootp = 0;
        cfg.ports.tftp = 0;
        cfg.ports.netinstall = 0;
        let report = run(&cfg, None, None).await;
        assert!(report.ok(), "{:?}", report.findings);
        assert_eq!(
            codes(&report.findings),
            [("unsuitable_interface", Severity::Warning)]
        );
        assert_eq!(report.network.unwrap().server_ip, Ipv4Addr::LOCALHOST);
        let json = serde_json::to_value(&report.findings[0]).unwrap();
        assert_eq!(json["severity"], "warning");
        assert_eq!(json["code"], "unsuitable_interface");
    }
}
