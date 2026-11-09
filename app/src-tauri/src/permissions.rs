//! The two macOS permissions the app needs, asked for up front (so the
//! prompts do not pop up in the middle of a flash) and checked:
//!
//! - **Local Network** (Privacy & Security): without it every send to the
//!   local network fails with `EHOSTUNREACH`. macOS asks the first time
//!   the app sends there; [`request`] does that on purpose with a harmless
//!   datagram (an empty mDNS query to `224.0.0.251:5353`, and a datagram to
//!   the discard port of each netinstall-capable adapter's directed
//!   broadcast) and reads the answer from the result of those sends.
//! - **Incoming connections** (the Application Firewall, if it is on):
//!   macOS asks when the app first listens; [`request`] opens a UDP socket
//!   and waits for a datagram briefly. The firewall's state is read with
//!   `socketfilterfw` (which needs no admin rights for reading).
//!
//! [`open_settings`] opens System Settings at the right pane: only the two
//! constant URLs below, never one from the web view.

use serde::{Deserialize, Serialize};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// System Settings → Privacy & Security → Local Network.
pub const LOCAL_NETWORK_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork";
/// System Settings → Network → Firewall.
pub const FIREWALL_URL: &str = "x-apple.systempreferences:com.apple.preference.security?Firewall";

const SOCKETFILTERFW: &str = "/usr/libexec/ApplicationFirewall/socketfilterfw";
/// An mDNS message with no questions: every responder ignores it.
const EMPTY_MDNS_QUERY: [u8; 12] = [0; 12];
const MDNS: SocketAddr = SocketAddr::V4(std::net::SocketAddrV4::new(
    Ipv4Addr::new(224, 0, 0, 251),
    5353,
));
/// The discard port: nothing answers a datagram sent there.
const DISCARD_PORT: u16 = 9;
/// How long the listening socket waits (the firewall prompt appears
/// meanwhile; the answer is not needed for the check).
const LISTEN_FOR: Duration = Duration::from_millis(300);

/// The Local Network permission, as the probe sends saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalNetwork {
    /// A send to the local network went out.
    Granted,
    /// Refused with `EHOSTUNREACH`: denied, or the prompt is not answered
    /// yet.
    Denied,
    /// No send could say (e.g. no network at all).
    Unknown,
}

/// The Application Firewall, as far as it concerns this app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Firewall {
    /// Off: nothing to allow.
    Off,
    /// On, and this app may receive incoming connections.
    Allowed,
    /// On, and this app is blocked (or everything is blocked).
    Blocked,
    /// On, but whether this app is allowed could not be read.
    Unknown,
}

/// What [`request`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsView {
    pub local_network: LocalNetwork,
    pub firewall: Firewall,
}

/// A System Settings pane [`open_settings`] can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsPane {
    LocalNetwork,
    Firewall,
}

impl SettingsPane {
    pub fn url(self) -> &'static str {
        match self {
            SettingsPane::LocalNetwork => LOCAL_NETWORK_URL,
            SettingsPane::Firewall => FIREWALL_URL,
        }
    }
}

/// The permission, from the results of the probe sends: one that went out
/// means granted; otherwise `EHOSTUNREACH` means denied.
pub fn local_network_from(results: &[io::Result<()>]) -> LocalNetwork {
    if results.iter().any(|r| r.is_ok()) {
        LocalNetwork::Granted
    } else if results
        .iter()
        .any(|r| r.as_ref().is_err_and(ni_net::is_host_unreachable))
    {
        LocalNetwork::Denied
    } else {
        LocalNetwork::Unknown
    }
}

/// `socketfilterfw --getglobalstate`: `Some(state)` (0 off, 1 on, 2 block
/// all).
pub fn parse_global_state(out: &str) -> Option<u8> {
    let rest = &out[out.find("State = ")? + "State = ".len()..];
    rest.chars().next()?.to_digit(10).map(|d| d as u8)
}

/// `socketfilterfw --getappblocked <app>`: `Some(true)` if blocked.
pub fn parse_app_blocked(out: &str) -> Option<bool> {
    let out = out.to_ascii_lowercase();
    if out.contains("permitted") || out.contains("allowed") {
        Some(false)
    } else if out.contains("blocked") {
        Some(true)
    } else {
        None
    }
}

/// The firewall's verdict from its global state and the app's entry.
pub fn firewall_from(global: Option<u8>, app_blocked: Option<bool>) -> Firewall {
    match (global, app_blocked) {
        (Some(0), _) => Firewall::Off,
        (Some(2), _) => Firewall::Blocked,
        (Some(_), Some(false)) => Firewall::Allowed,
        (Some(_), Some(true)) => Firewall::Blocked,
        _ => Firewall::Unknown,
    }
}

/// The `.app` bundle the executable is in (the firewall lists bundles),
/// else the executable itself.
fn app_path(exe: &Path) -> PathBuf {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .unwrap_or(exe)
        .to_path_buf()
}

fn socketfilterfw(args: &[&std::ffi::OsStr]) -> Option<String> {
    let out = std::process::Command::new(SOCKETFILTERFW)
        .args(args)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn firewall_state() -> Firewall {
    let global = socketfilterfw(&["--getglobalstate".as_ref()])
        .as_deref()
        .and_then(parse_global_state);
    if global == Some(0) {
        return Firewall::Off;
    }
    let blocked = std::env::current_exe().ok().and_then(|exe| {
        let app = app_path(&exe);
        socketfilterfw(&["--getappblocked".as_ref(), app.as_os_str()])
            .as_deref()
            .and_then(parse_app_blocked)
    });
    firewall_from(global, blocked)
}

/// The directed broadcasts of the netinstall-capable adapters with an
/// IPv4 address (interface name, address).
fn adapter_broadcasts() -> Vec<(String, SocketAddr)> {
    let Ok(ifaces) = crate::core::list_interfaces() else {
        return Vec::new();
    };
    ifaces
        .into_iter()
        .filter(|i| i.netinstall_capable)
        .flat_map(|i| {
            let name = i.name.clone();
            i.ipv4.into_iter().filter_map(move |a| {
                let mask = Ipv4Addr::from(
                    u32::MAX
                        .checked_shl(32u32.saturating_sub(a.prefix))
                        .unwrap_or(0),
                );
                let b = ni_net::directed_broadcast(a.address, mask)?;
                Some((name.clone(), SocketAddr::from((b, DISCARD_PORT))))
            })
        })
        .collect()
}

/// Send the probe datagrams; each send's result.
async fn probe_sends() -> Vec<io::Result<()>> {
    let mut results = Vec::new();
    match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => results.push(s.send_to(&EMPTY_MDNS_QUERY, MDNS).await.map(|_| ())),
        Err(e) => results.push(Err(e)),
    }
    for (iface, to) in adapter_broadcasts() {
        let opts = ni_net::BindOptions::new(0).interface(iface).broadcast(true);
        let r = match ni_net::bind_udp(&opts) {
            Ok(s) => s.send_to(&EMPTY_MDNS_QUERY, to).await.map(|_| ()),
            Err(e) => Err(io::Error::other(e.to_string())),
        };
        results.push(r);
    }
    results
}

/// Listen briefly (the firewall asks about incoming connections now).
async fn listen_briefly() {
    if let Ok(s) = tokio::net::UdpSocket::bind("0.0.0.0:0").await {
        let mut buf = [0u8; 64];
        let _ = tokio::time::timeout(LISTEN_FOR, s.recv_from(&mut buf)).await;
    }
}

/// Make macOS ask for both permissions now (if it has not yet), then
/// report what it says. Asking again later only checks.
pub async fn request() -> PermissionsView {
    let (sends, ()) = tokio::join!(probe_sends(), listen_briefly());
    for r in &sends {
        if let Err(e) = r {
            tracing::debug!("permission probe send failed: {e}");
        }
    }
    let local_network = local_network_from(&sends);
    let firewall = tokio::task::spawn_blocking(firewall_state)
        .await
        .unwrap_or(Firewall::Unknown);
    tracing::info!(?local_network, ?firewall, "permissions checked");
    PermissionsView {
        local_network,
        firewall,
    }
}

/// Open System Settings at `pane` (`/usr/bin/open` with a constant URL).
/// Error: `open_settings_failed` (`detail`).
pub fn open_settings(pane: SettingsPane) -> Result<(), ni_engine::Failure> {
    let failed = |d: String| ni_engine::Failure::new("open_settings_failed").with("detail", d);
    let status = std::process::Command::new("/usr/bin/open")
        .arg(pane.url())
        .status()
        .map_err(|e| failed(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(failed(status.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(code: i32) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(code))
    }

    #[test]
    fn local_network_from_the_probe_sends() {
        // macOS errno values.
        const EHOSTUNREACH: i32 = 65;
        const ENETUNREACH: i32 = 51;
        assert_eq!(
            local_network_from(&[err(EHOSTUNREACH), Ok(())]),
            LocalNetwork::Granted
        );
        assert_eq!(
            local_network_from(&[err(ENETUNREACH), err(EHOSTUNREACH)]),
            LocalNetwork::Denied
        );
        assert_eq!(
            local_network_from(&[err(ENETUNREACH)]),
            LocalNetwork::Unknown
        );
        assert_eq!(local_network_from(&[]), LocalNetwork::Unknown);
    }

    #[test]
    fn firewall_output_is_parsed() {
        assert_eq!(
            parse_global_state("Firewall is enabled. (State = 1)\n"),
            Some(1)
        );
        assert_eq!(
            parse_global_state("Firewall is disabled. (State = 0)"),
            Some(0)
        );
        assert_eq!(
            parse_global_state(
                "Firewall is blocking all non-essential incoming connections. (State = 2)"
            ),
            Some(2)
        );
        assert_eq!(parse_global_state("nonsense"), None);
        assert_eq!(
            parse_app_blocked("The application /Applications/Mac Netinstall.app is permitted to receive incoming connections"),
            Some(false)
        );
        assert_eq!(
            parse_app_blocked("The application /Applications/Mac Netinstall.app is blocked from receiving incoming connections"),
            Some(true)
        );
        assert_eq!(
            parse_app_blocked("The application is not part of the firewall"),
            None
        );
        assert_eq!(firewall_from(Some(0), None), Firewall::Off);
        assert_eq!(firewall_from(Some(2), Some(false)), Firewall::Blocked);
        assert_eq!(firewall_from(Some(1), Some(false)), Firewall::Allowed);
        assert_eq!(firewall_from(Some(1), Some(true)), Firewall::Blocked);
        assert_eq!(firewall_from(Some(1), None), Firewall::Unknown);
        assert_eq!(firewall_from(None, None), Firewall::Unknown);
    }

    #[test]
    fn the_app_bundle_is_found() {
        assert_eq!(
            app_path(Path::new(
                "/Applications/Mac Netinstall.app/Contents/MacOS/mac-netinstall"
            )),
            Path::new("/Applications/Mac Netinstall.app")
        );
        assert_eq!(
            app_path(Path::new("/usr/local/bin/x")),
            Path::new("/usr/local/bin/x")
        );
    }

    #[test]
    fn only_constant_settings_urls() {
        assert_eq!(SettingsPane::LocalNetwork.url(), LOCAL_NETWORK_URL);
        assert_eq!(SettingsPane::Firewall.url(), FIREWALL_URL);
        for url in [LOCAL_NETWORK_URL, FIREWALL_URL] {
            assert!(url.starts_with("x-apple.systempreferences:"));
        }
        // The window names a pane, not a URL.
        assert_eq!(
            serde_json::from_str::<SettingsPane>("\"local_network\"").unwrap(),
            SettingsPane::LocalNetwork
        );
        assert!(serde_json::from_str::<SettingsPane>("\"https://example.com\"").is_err());
    }
}
