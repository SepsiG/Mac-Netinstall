//! Engine settings and the network plan derived from an interface.

use crate::event::Failure;
use crate::profile::Profile;
use ni_net::Interface;
use ni_proto::{Mac, SessionConfig};
use ni_tftp::TransferConfig;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

/// Shortest allowed cooldown: a flashed device must not be served again
/// while it reboots (RouterBOOT may netboot once more).
pub const MIN_COOLDOWN: Duration = Duration::from_secs(30);

/// Longest allowed cooldown (a day): longer ones are mistakes, and
/// `Instant + cooldown` must not overflow.
pub const MAX_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);

/// Most devices prepared or flashed at the same time.
pub const MAX_PARALLEL: usize = 64;

/// `invalid_cooldown` (`min_secs`, `max_secs`, `secs`) if `cooldown` is
/// below `min` or above [`MAX_COOLDOWN`].
pub fn check_cooldown(cooldown: Duration, min: Duration) -> Result<(), Failure> {
    if cooldown < min || cooldown > MAX_COOLDOWN {
        return Err(Failure::new("invalid_cooldown")
            .with("min_secs", min.as_secs())
            .with("max_secs", MAX_COOLDOWN.as_secs())
            .with("secs", cooldown.as_secs()));
    }
    Ok(())
}

/// `invalid_max_parallel` (`min`, `max`, `value`) unless `1 ..=`
/// [`MAX_PARALLEL`].
pub fn check_max_parallel(n: usize) -> Result<(), Failure> {
    if !(1..=MAX_PARALLEL).contains(&n) {
        return Err(Failure::new("invalid_max_parallel")
            .with("min", 1)
            .with("max", MAX_PARALLEL)
            .with("value", n));
    }
    Ok(())
}

/// Netmask used with a configured server IP that is not on the interface
/// (yet): `/24`.
pub const DEFAULT_NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);

type ListFn = dyn Fn() -> io::Result<Vec<Interface>> + Send + Sync;

/// Where the engine reads the interface list from (`start` and the link
/// watchdog). [`InterfaceSource::system`] (the default) asks the OS;
/// tests inject a fake one to simulate a lost carrier or an unplugged
/// adapter.
#[derive(Clone)]
pub struct InterfaceSource(Arc<ListFn>);

impl InterfaceSource {
    /// The live list ([`ni_net::list_interfaces`]).
    pub fn system() -> Self {
        InterfaceSource(Arc::new(ni_net::list_interfaces))
    }

    /// Any function returning an interface list.
    pub fn new(f: impl Fn() -> io::Result<Vec<Interface>> + Send + Sync + 'static) -> Self {
        InterfaceSource(Arc::new(f))
    }

    pub fn list(&self) -> io::Result<Vec<Interface>> {
        (self.0)()
    }
}

impl Default for InterfaceSource {
    fn default() -> Self {
        InterfaceSource::system()
    }
}

impl fmt::Debug for InterfaceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InterfaceSource")
    }
}

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
    /// Our address; default: the interface's first IPv4 address. Set it
    /// to start while the interface has no link: macOS removes a manual
    /// IPv4 address while there is no carrier (see [`resolve_network`]).
    pub server_ip: Option<Ipv4Addr>,
    /// Netmask for `server_ip` while it is not on the interface; default
    /// [`DEFAULT_NETMASK`]. Ignored when the interface carries the address
    /// (its own netmask is used).
    pub netmask: Option<Ipv4Addr>,
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
    /// Devices preparing or flashing at the same time (1 ..=
    /// [`MAX_PARALLEL`]).
    pub max_parallel: usize,
    /// How long a finished device is ignored (it reboots, and must not be
    /// flashed again); at least `min_cooldown`. After the cooldown a done
    /// device is served again only once it netboots (a new BOOTP request):
    /// stray hellos alone are ignored (`awaiting_bootp`).
    pub cooldown: Duration,
    /// Lower bound for `cooldown`: [`MIN_COOLDOWN`]. Tests lower it.
    #[doc(hidden)]
    pub min_cooldown: Duration,
    pub session: SessionConfig,
    pub tftp: TransferConfig,
    pub lease_secs: u32,
    pub ports: Ports,
    /// How often to check the interface (link watchdog); `None` disables
    /// the check. A missing carrier or server IP is a `link_down` warning
    /// (and `link_up` when it is back); the engine keeps running.
    pub link_check: Option<Duration>,
    /// How long the interface may be missing from the interface list
    /// (adapter unplugged) before the engine stops with `interface_gone`.
    pub interface_grace: Duration,
    /// Where the interface list comes from (default: the OS).
    pub interfaces: InterfaceSource,
}

impl EngineConfig {
    /// Defaults for `interface`: one device at a time, 5 min cooldown, no
    /// profile, auto-repeat off.
    pub fn new(interface: &str) -> Self {
        EngineConfig {
            interface: interface.to_string(),
            server_ip: None,
            netmask: None,
            pool: None,
            server_mac: None,
            profile: None,
            auto_repeat: false,
            only_mac: None,
            max_parallel: 1,
            cooldown: Duration::from_secs(5 * 60),
            min_cooldown: MIN_COOLDOWN,
            session: SessionConfig::default(),
            tftp: TransferConfig::default(),
            lease_secs: 600,
            ports: Ports::default(),
            link_check: Some(Duration::from_secs(2)),
            interface_grace: Duration::from_secs(10),
            interfaces: InterfaceSource::system(),
        }
    }

    /// Settings `start` refuses. Codes: `invalid_cooldown` (`min_secs`,
    /// `max_secs`, `secs`), `invalid_max_parallel` (`min`, `max`, `value`), profile
    /// validation codes.
    pub fn validate(&self) -> Result<(), Failure> {
        check_cooldown(self.cooldown, self.min_cooldown)?;
        check_max_parallel(self.max_parallel)?;
        if let Some(p) = &self.profile {
            p.validate()?;
        }
        Ok(())
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
/// `02:00:00:00:00:00` count as unknown), `pool_includes_reserved`.
///
/// A configured `server_ip` that is not on the interface is accepted
/// (with `cfg.netmask`, default [`DEFAULT_NETMASK`]) while the interface
/// has no link — no IPv4 address at all, or not up/running: macOS removes
/// a manual address while there is no carrier and restores it with the
/// link. On an interface that has other addresses it is
/// `server_ip_not_on_interface`.
pub fn resolve_network(ifaces: &[Interface], cfg: &EngineConfig) -> Result<NetPlan, Failure> {
    let plan = resolve_addresses(ifaces, cfg)?;
    check_pool_reserved(&plan)?;
    Ok(plan)
}

/// [`resolve_network`] without the pool check (preflight reports that
/// separately).
pub(crate) fn resolve_addresses(
    ifaces: &[Interface],
    cfg: &EngineConfig,
) -> Result<NetPlan, Failure> {
    let iface = ifaces
        .iter()
        .find(|i| i.name == cfg.interface)
        .ok_or_else(|| Failure::new("unknown_interface").with("interface", &cfg.interface))?;
    let (server_ip, netmask) = match cfg.server_ip {
        // The address macOS is configured with (System Settings → TCP/IP →
        // Manually); there without link, when macOS has removed it from
        // the interface. Else the live address.
        None => match iface.configured_ipv4.as_ref().and_then(|c| c.manual()) {
            Some((ip, mask))
                if iface.ipv4.is_empty()
                    || !iface.is_up
                    || !iface.is_running
                    || iface.ipv4.iter().any(|(a, _)| *a == ip) =>
            {
                iface
                    .ipv4
                    .iter()
                    .copied()
                    .find(|(a, _)| *a == ip)
                    .unwrap_or((ip, mask))
            }
            _ => *iface
                .ipv4
                .first()
                .ok_or_else(|| Failure::new("no_ipv4").with("interface", &iface.name))?,
        },
        Some(ip) => match iface.ipv4.iter().find(|(a, _)| *a == ip) {
            Some(found) => *found,
            None if iface.ipv4.is_empty() || !iface.is_up || !iface.is_running => {
                (ip, cfg.netmask.unwrap_or(DEFAULT_NETMASK))
            }
            None => {
                return Err(Failure::new("server_ip_not_on_interface")
                    .with("interface", &iface.name)
                    .with("ip", ip))
            }
        },
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

/// A pool must not contain the subnet's network or broadcast address
/// (`pool_includes_reserved`; /31 and /32 have neither). The BOOTP pool
/// would skip them anyway, but a pool given that way is a mistake.
pub fn check_pool_reserved(plan: &NetPlan) -> Result<(), Failure> {
    let mask = u32::from(plan.netmask);
    if mask.leading_ones() + mask.trailing_zeros() != 32 || mask.leading_ones() > 30 {
        return Ok(());
    }
    let (net, bcast) = subnet(plan.server_ip, plan.netmask);
    let (start, end) = (u32::from(plan.pool_start), u32::from(plan.pool_end));
    let inside = |ip: Ipv4Addr| (start..=end).contains(&u32::from(ip));
    if inside(net) || inside(bcast) {
        return Err(Failure::new("pool_includes_reserved")
            .with("start", plan.pool_start)
            .with("end", plan.pool_end)
            .with("network", net)
            .with("broadcast", bcast));
    }
    Ok(())
}

/// The watchdog's view of the boot interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStatus {
    /// Listed, up, and carrying the server IP.
    Up,
    /// Listed but unusable right now; the reason is a stable code:
    /// `interface_down` (not `IFF_UP`), `no_ip` (the server IP is missing:
    /// on macOS that is what a lost carrier looks like), `no_carrier`.
    Down(&'static str),
    /// Not listed at all (e.g. the USB adapter was unplugged).
    Missing,
}

/// The status of interface `name` for `server_ip`.
pub fn link_status(ifaces: &[Interface], name: &str, server_ip: Ipv4Addr) -> LinkStatus {
    let Some(i) = ifaces.iter().find(|i| i.name == name) else {
        return LinkStatus::Missing;
    };
    if !i.is_up {
        LinkStatus::Down("interface_down")
    } else if !i.ipv4.iter().any(|(a, _)| *a == server_ip) {
        LinkStatus::Down("no_ip")
    } else if !i.is_running {
        LinkStatus::Down("no_carrier")
    } else {
        LinkStatus::Up
    }
}

/// The interface still exists, is up and carries `server_ip`.
pub fn interface_usable(ifaces: &[Interface], name: &str, server_ip: Ipv4Addr) -> bool {
    link_status(ifaces, name, server_ip) == LinkStatus::Up
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
        configured_ipv4: None,
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
    fn a_configured_server_ip_works_without_link() {
        // No carrier: macOS removed the manual address.
        let mut en7 = iface("en7", &[], Some(MAC));
        en7.is_running = false;
        let mut cfg = EngineConfig::new("en7");
        assert_eq!(
            resolve_network(&[en7.clone()], &cfg).unwrap_err().code,
            "no_ipv4"
        );
        cfg.server_ip = Some(ip(1));
        let plan = resolve_network(&[en7.clone()], &cfg).unwrap();
        assert_eq!(
            (plan.server_ip, plan.netmask, plan.pool_start, plan.pool_end),
            (ip(1), MASK24, ip(1), ip(254))
        );
        cfg.netmask = Some(Ipv4Addr::new(255, 255, 255, 240));
        let plan = resolve_network(&[en7.clone()], &cfg).unwrap();
        assert_eq!(
            (plan.netmask, plan.pool_end),
            (cfg.netmask.unwrap(), ip(14))
        );
        // On an interface with link and other addresses it is a mistake.
        let other = iface("en7", &[(ip(9), MASK24)], Some(MAC));
        assert_eq!(
            resolve_network(&[other], &cfg).unwrap_err().code,
            "server_ip_not_on_interface"
        );
        // When the address is there, its own netmask wins.
        let back = iface("en7", &[(ip(1), MASK24)], Some(MAC));
        assert_eq!(resolve_network(&[back], &cfg).unwrap().netmask, MASK24);
    }

    #[test]
    fn a_pool_must_not_contain_network_or_broadcast() {
        let ifaces = [iface("en7", &[(ip(1), MASK24)], Some(MAC))];
        let mut cfg = EngineConfig::new("en7");
        for pool in [(ip(0), ip(10)), (ip(200), ip(255))] {
            cfg.pool = Some(pool);
            let err = resolve_network(&ifaces, &cfg).unwrap_err();
            assert_eq!(err.code, "pool_includes_reserved");
            assert_eq!(err.param("broadcast"), Some("192.168.88.255"));
        }
        cfg.pool = Some((ip(1), ip(254)));
        assert!(resolve_network(&ifaces, &cfg).is_ok());
        // /31 has no network or broadcast address.
        let p2p = [iface(
            "en7",
            &[(ip(0), Ipv4Addr::new(255, 255, 255, 254))],
            Some(MAC),
        )];
        cfg.pool = Some((ip(0), ip(1)));
        assert!(resolve_network(&p2p, &cfg).is_ok());
    }

    #[test]
    fn link_status_tells_down_from_missing() {
        let mut en7 = iface("en7", &[(ip(1), MASK24)], Some(MAC));
        assert_eq!(link_status(&[en7.clone()], "en7", ip(1)), LinkStatus::Up);
        assert_eq!(
            link_status(&[en7.clone()], "en8", ip(1)),
            LinkStatus::Missing
        );
        assert_eq!(
            link_status(&[en7.clone()], "en7", ip(2)),
            LinkStatus::Down("no_ip")
        );
        en7.is_running = false;
        assert_eq!(
            link_status(&[en7.clone()], "en7", ip(1)),
            LinkStatus::Down("no_carrier")
        );
        en7.is_up = false;
        assert_eq!(
            link_status(&[en7], "en7", ip(1)),
            LinkStatus::Down("interface_down")
        );
    }

    fn manual(addr: Ipv4Addr, mask: Ipv4Addr) -> Option<ni_net::ConfiguredIpv4> {
        Some(ni_net::ConfiguredIpv4 {
            method: ni_net::Ipv4Method::Manual,
            address: Some(addr),
            netmask: Some(mask),
        })
    }

    #[test]
    fn the_configured_manual_address_serves_without_link() {
        // No cable: macOS removed the manual address from the interface,
        // but the service still has it.
        let mut en7 = iface("en7", &[], Some(MAC));
        en7.is_running = false;
        en7.configured_ipv4 = manual(ip(1), MASK24);
        let plan = resolve_network(&[en7.clone()], &EngineConfig::new("en7")).unwrap();
        assert_eq!((plan.server_ip, plan.netmask), (ip(1), MASK24));
        // The subnet (the BOOTP pool skips the server's own address).
        assert_eq!((plan.pool_start, plan.pool_end), (ip(1), ip(254)));
        // With link, the live address is the same one.
        en7.is_running = true;
        en7.ipv4 = vec![(ip(1), MASK24)];
        let plan = resolve_network(&[en7.clone()], &EngineConfig::new("en7")).unwrap();
        assert_eq!(plan.server_ip, ip(1));
        // DHCP-configured, no address: nothing to serve with.
        en7.ipv4.clear();
        en7.configured_ipv4 = Some(ni_net::ConfiguredIpv4 {
            method: ni_net::Ipv4Method::Dhcp,
            address: None,
            netmask: None,
        });
        let err = resolve_network(&[en7], &EngineConfig::new("en7")).unwrap_err();
        assert_eq!(err.code, "no_ipv4");
    }

    #[test]
    fn cooldown_has_a_lower_bound() {
        let mut cfg = EngineConfig::new("en7");
        cfg.cooldown = Duration::from_secs(29);
        let err = cfg.validate().unwrap_err();
        assert_eq!(
            err,
            Failure::new("invalid_cooldown")
                .with("min_secs", 30)
                .with("max_secs", 86400)
                .with("secs", 29)
        );
        cfg.cooldown = MIN_COOLDOWN;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn cooldown_has_an_upper_bound_of_a_day() {
        let mut cfg = EngineConfig::new("en7");
        cfg.cooldown = MAX_COOLDOWN;
        assert!(cfg.validate().is_ok());
        for secs in [MAX_COOLDOWN.as_secs() + 1, u64::MAX] {
            cfg.cooldown = Duration::from_secs(secs);
            let err = cfg.validate().unwrap_err();
            assert_eq!(err.code, "invalid_cooldown");
            assert_eq!(err.param("max_secs"), Some("86400"));
            assert_eq!(err.param("min_secs"), Some("30"));
        }
        // Not even a test's lower minimum lifts the upper bound.
        let err = check_cooldown(Duration::MAX, Duration::ZERO).unwrap_err();
        assert_eq!(err.code, "invalid_cooldown");
    }

    #[test]
    fn max_parallel_is_bounded() {
        let mut cfg = EngineConfig::new("en7");
        for (n, ok) in [
            (0, false),
            (1, true),
            (MAX_PARALLEL, true),
            (MAX_PARALLEL + 1, false),
        ] {
            cfg.max_parallel = n;
            assert_eq!(cfg.validate().is_ok(), ok, "{n}");
        }
        let err = check_max_parallel(0).unwrap_err();
        assert_eq!(err.code, "invalid_max_parallel");
        assert_eq!(
            (err.param("min"), err.param("max"), err.param("value")),
            (Some("1"), Some("64"), Some("0"))
        );
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
        assert_eq!(cfg.max_parallel, 1, "one at a time unless chosen");
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
