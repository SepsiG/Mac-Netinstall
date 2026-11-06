//! Engine settings and the network plan derived from an interface.

use crate::event::Failure;
use crate::profile::Profile;
use ni_net::Interface;
use ni_proto::{Mac, SessionConfig};
use ni_tftp::TransferConfig;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

/// Ports and addresses. The defaults are the real protocol ports with
/// broadcast destinations; tests use port 0 (ephemeral) and loopback
/// destinations, since `lo0` cannot broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ports {
    /// BOOTP/DHCP server port (67).
    pub bootp: u16,
    /// TFTP listener port (69).
    pub tftp: u16,
    /// Netinstall port (5000), local side.
    pub netinstall: u16,
    /// Local address of all three listeners (`0.0.0.0` receives broadcasts).
    pub bind_ip: Ipv4Addr,
    /// Where BOOTP replies go (`255.255.255.255:68`).
    pub bootp_reply_to: SocketAddr,
    /// Where Netinstall frames go (`255.255.255.255:5000`).
    pub netinstall_dest: SocketAddr,
}

impl Default for Ports {
    fn default() -> Self {
        Ports {
            bootp: 67,
            tftp: 69,
            netinstall: 5000,
            bind_ip: Ipv4Addr::UNSPECIFIED,
            bootp_reply_to: SocketAddr::from((Ipv4Addr::BROADCAST, 68)),
            netinstall_dest: SocketAddr::from((Ipv4Addr::BROADCAST, 5000)),
        }
    }
}

/// Everything `start` needs.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// BSD name of the boot interface, e.g. `en7`.
    pub interface: String,
    /// Our address; default: the interface's first IPv4 address.
    pub server_ip: Option<Ipv4Addr>,
    /// First and last address handed out; default: see [`default_pool`].
    pub pool: Option<(Ipv4Addr, Ipv4Addr)>,
    /// Source MAC of our Netinstall frames; default: the interface's MAC.
    pub server_mac: Option<Mac>,
    /// The active profile: given to new devices when auto-repeat is on.
    /// Its version also selects the boot kernels (stable without one).
    pub profile: Option<Profile>,
    pub auto_repeat: bool,
    /// Serve only this device (BOOTP and Netinstall).
    pub only_mac: Option<Mac>,
    /// Devices preparing or flashing at the same time (at least 1).
    pub max_parallel: usize,
    /// How long a finished device is ignored (it reboots, and must not be
    /// flashed again).
    pub cooldown: Duration,
    pub session: SessionConfig,
    pub tftp: TransferConfig,
    pub lease_secs: u32,
    pub ports: Ports,
    /// How often to check that the interface is still up and has the
    /// server IP; `None` disables the check.
    pub link_check: Option<Duration>,
}

impl EngineConfig {
    /// Defaults for `interface`: 4 in parallel, 5 min cooldown, no
    /// profile, auto-repeat off.
    pub fn new(interface: &str) -> Self {
        EngineConfig {
            interface: interface.to_string(),
            server_ip: None,
            pool: None,
            server_mac: None,
            profile: None,
            auto_repeat: false,
            only_mac: None,
            max_parallel: 4,
            cooldown: Duration::from_secs(5 * 60),
            session: SessionConfig::default(),
            tftp: TransferConfig::default(),
            lease_secs: 600,
            ports: Ports::default(),
            link_check: Some(Duration::from_secs(2)),
        }
    }
}

/// The addresses the engine works with, resolved from the interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetPlan {
    pub server_ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub pool_start: Ipv4Addr,
    pub pool_end: Ipv4Addr,
    #[serde(with = "crate::event::mac_str")]
    pub server_mac: Mac,
}

/// Network and broadcast address of `ip`/`mask` (`mask` must be a prefix).
pub fn subnet(ip: Ipv4Addr, mask: Ipv4Addr) -> (Ipv4Addr, Ipv4Addr) {
    let (ip, mask) = (u32::from(ip), u32::from(mask));
    ((ip & mask).into(), (ip | !mask).into())
}

/// Default pool: the subnet's first 254 host addresses (the whole subnet
/// of a /24). The server IP inside it is skipped by the BOOTP pool.
/// For /31 and /32 the pool is the subnet itself.
pub fn default_pool(server_ip: Ipv4Addr, netmask: Ipv4Addr) -> (Ipv4Addr, Ipv4Addr) {
    let (net, bcast) = subnet(server_ip, netmask);
    let (net, bcast) = (u32::from(net), u32::from(bcast));
    if bcast - net < 2 {
        return (net.into(), bcast.into());
    }
    let first = net + 1;
    let last = (bcast - 1).min(net + 254);
    (first.into(), last.into())
}

/// Resolve `cfg` against the interface list. Codes: `unknown_interface`,
/// `no_ipv4`, `server_ip_not_on_interface`, `no_mac` (neither a configured
/// server MAC nor a known interface MAC; placeholders such as
/// `02:00:00:00:00:00` count as unknown).
pub fn resolve_network(ifaces: &[Interface], cfg: &EngineConfig) -> Result<NetPlan, Failure> {
    let iface = ifaces
        .iter()
        .find(|i| i.name == cfg.interface)
        .ok_or_else(|| Failure::new("unknown_interface").with("interface", &cfg.interface))?;
    let (server_ip, netmask) = match cfg.server_ip {
        None => *iface
            .ipv4
            .first()
            .ok_or_else(|| Failure::new("no_ipv4").with("interface", &iface.name))?,
        Some(ip) => *iface.ipv4.iter().find(|(a, _)| *a == ip).ok_or_else(|| {
            Failure::new("server_ip_not_on_interface")
                .with("interface", &iface.name)
                .with("ip", ip)
        })?,
    };
    let (pool_start, pool_end) = cfg.pool.unwrap_or_else(|| default_pool(server_ip, netmask));
    // Never fall back to a placeholder: macOS may mask link addresses as
    // 02:00:00:00:00:00, and frames from it would collide between Macs.
    let server_mac = cfg
        .server_mac
        .or(iface.mac)
        .and_then(ni_net::known_mac)
        .ok_or_else(|| Failure::new("no_mac").with("interface", &iface.name))?;
    Ok(NetPlan {
        server_ip,
        netmask,
        pool_start,
        pool_end,
        server_mac,
    })
}

/// The interface still exists, is up and carries `server_ip`.
pub fn interface_usable(ifaces: &[Interface], name: &str, server_ip: Ipv4Addr) -> bool {
    ifaces
        .iter()
        .any(|i| i.name == name && i.is_up && i.ipv4.iter().any(|(a, _)| *a == server_ip))
}

/// An up, running Ethernet interface for tests.
#[cfg(test)]
pub(crate) fn test_iface(name: &str, ipv4: &[(Ipv4Addr, Ipv4Addr)], mac: Option<Mac>) -> Interface {
    Interface {
        name: name.into(),
        index: 7,
        mac,
        ipv4: ipv4.to_vec(),
        is_up: true,
        is_running: true,
        is_loopback: false,
        kind: ni_net::InterfaceKind::Ethernet,
        display_name: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use test_iface as iface;

    const MASK24: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
    const MAC: Mac = Mac([2, 0, 0, 0, 0, 1]);

    fn ip(last: u8) -> Ipv4Addr {
        Ipv4Addr::new(192, 168, 88, last)
    }

    #[test]
    fn default_pool_sizes() {
        assert_eq!(default_pool(ip(1), MASK24), (ip(1), ip(254)));
        assert_eq!(
            default_pool(Ipv4Addr::new(10, 1, 2, 3), Ipv4Addr::new(255, 255, 0, 0)),
            (Ipv4Addr::new(10, 1, 0, 1), Ipv4Addr::new(10, 1, 0, 254))
        );
        assert_eq!(
            default_pool(ip(9), Ipv4Addr::new(255, 255, 255, 248)),
            (ip(9), ip(14))
        );
        assert_eq!(
            default_pool(ip(10), Ipv4Addr::new(255, 255, 255, 254)),
            (ip(10), ip(11))
        );
        assert_eq!(subnet(ip(77), MASK24), (ip(0), ip(255)));
    }

    #[test]
    fn resolve_uses_interface_defaults() {
        let ifaces = [iface("en7", &[(ip(1), MASK24)], Some(MAC))];
        let plan = resolve_network(&ifaces, &EngineConfig::new("en7")).unwrap();
        assert_eq!(
            plan,
            NetPlan {
                server_ip: ip(1),
                netmask: MASK24,
                pool_start: ip(1),
                pool_end: ip(254),
                server_mac: MAC,
            }
        );
    }

    #[test]
    fn resolve_honours_overrides_and_reports_problems() {
        let ifaces = [
            iface("en7", &[(ip(1), MASK24), (ip(2), MASK24)], Some(MAC)),
            iface("en8", &[], None),
        ];
        let mut cfg = EngineConfig::new("en7");
        cfg.server_ip = Some(ip(2));
        cfg.pool = Some((ip(100), ip(110)));
        cfg.server_mac = Some(Mac([2; 6]));
        let plan = resolve_network(&ifaces, &cfg).unwrap();
        assert_eq!(
            (
                plan.server_ip,
                plan.pool_start,
                plan.pool_end,
                plan.server_mac
            ),
            (ip(2), ip(100), ip(110), Mac([2; 6]))
        );
        let code = |cfg: &EngineConfig| resolve_network(&ifaces, cfg).unwrap_err().code;
        cfg.server_ip = Some(ip(3));
        assert_eq!(code(&cfg), "server_ip_not_on_interface");
        assert_eq!(code(&EngineConfig::new("en9")), "unknown_interface");
        assert_eq!(code(&EngineConfig::new("en8")), "no_ipv4");
        let ifaces = [iface("en8", &[(ip(1), MASK24)], None)];
        let err = resolve_network(&ifaces, &EngineConfig::new("en8")).unwrap_err();
        assert_eq!(err, Failure::new("no_mac").with("interface", "en8"));
    }

    #[test]
    fn placeholder_macs_count_as_unknown() {
        // macOS masks link addresses as 02:00:00:00:00:00; never send
        // frames from it (or from all-zero).
        for placeholder in [ni_net::MASKED_MAC, Mac::ZERO] {
            let ifaces = [iface("en8", &[(ip(1), MASK24)], Some(placeholder))];
            let err = resolve_network(&ifaces, &EngineConfig::new("en8")).unwrap_err();
            assert_eq!(err, Failure::new("no_mac").with("interface", "en8"));
            let mut cfg = EngineConfig::new("en8");
            cfg.server_mac = Some(placeholder);
            let err = resolve_network(&ifaces, &cfg).unwrap_err();
            assert_eq!(err.code, "no_mac");
            // An explicit real MAC still works (the loopback tests use one).
            cfg.server_mac = Some(MAC);
            assert_eq!(resolve_network(&ifaces, &cfg).unwrap().server_mac, MAC);
        }
    }

    #[test]
    fn usable_interface_needs_up_and_server_ip() {
        let mut ifaces = vec![iface("en7", &[(ip(1), MASK24)], Some(MAC))];
        assert!(interface_usable(&ifaces, "en7", ip(1)));
        assert!(!interface_usable(&ifaces, "en7", ip(2)));
        assert!(!interface_usable(&ifaces, "en8", ip(1)));
        ifaces[0].is_up = false;
        assert!(!interface_usable(&ifaces, "en7", ip(1)));
    }

    #[test]
    fn defaults() {
        let cfg = EngineConfig::new("en7");
        assert_eq!(cfg.max_parallel, 4);
        assert_eq!(cfg.cooldown, Duration::from_secs(300));
        assert!(!cfg.auto_repeat);
        assert_eq!(cfg.ports.bootp_reply_to.to_string(), "255.255.255.255:68");
        assert_eq!(
            cfg.ports.netinstall_dest.to_string(),
            "255.255.255.255:5000"
        );
        assert_eq!(
            (cfg.ports.bootp, cfg.ports.tftp, cfg.ports.netinstall),
            (67, 69, 5000)
        );
    }
}
