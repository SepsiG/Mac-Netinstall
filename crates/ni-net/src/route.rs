//! Which interfaces carry an IPv4 default route.
//!
//! Used by preflight: an interface with a default route is probably a
//! real LAN (a router, other hosts), where our BOOTP pool may collide
//! with live addresses. The routing table is read with
//! `/usr/sbin/netstat -rn -f inet` (scoped default routes of secondary
//! interfaces are listed there too, flag `I`).

use std::collections::BTreeSet;
use std::io;
use std::process::Command;

/// Interfaces with an IPv4 default route (`default` destination through a
/// gateway, flag `G`), from `netstat -rn -f inet` output.
pub fn parse_default_routes(netstat: &str) -> BTreeSet<String> {
    netstat
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (dest, _gateway, flags, netif) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            (dest == "default" && flags.contains('G')).then(|| netif.to_string())
        })
        .collect()
}

/// Interfaces with an IPv4 default route, from the live routing table.
pub fn default_route_interfaces() -> io::Result<BTreeSet<String>> {
    let out = Command::new("/usr/sbin/netstat")
        .args(["-rn", "-f", "inet"])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("netstat failed: {}", out.status)));
    }
    Ok(parse_default_routes(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Routing tables

Internet:
Destination        Gateway            Flags               Netif Expire
default            192.168.100.1      UGScg                 en0
default            192.168.88.254     UGScIg                en7
default            link#20            UCSIg               utun4
10                 10.214.12.1        UGSc                utun4
127.0.0.1          127.0.0.1          UH                    lo0
";

    #[test]
    fn default_routes_through_a_gateway_are_found() {
        let found: Vec<String> = parse_default_routes(SAMPLE).into_iter().collect();
        assert_eq!(found, ["en0", "en7"]);
        assert!(parse_default_routes("").is_empty());
    }

    #[test]
    fn the_live_table_can_be_read() {
        // Whatever the machine's routes are, reading them works.
        default_route_interfaces().unwrap();
    }
}
