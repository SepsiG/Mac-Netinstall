//! MAC-keyed IPv4 address pool.

use ni_proto::Mac;
use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PoolError {
    #[error("pool start {start} is after end {end}")]
    Inverted { start: Ipv4Addr, end: Ipv4Addr },
}

/// Hands out addresses from `start..=end`, never `exclude` (the server's
/// own IP). A MAC keeps its address for the pool's lifetime; leases never
/// expire (a Netinstall run is short).
#[derive(Debug, Clone)]
pub struct LeasePool {
    start: u32,
    end: u32,
    exclude: Ipv4Addr,
    leases: HashMap<Mac, Ipv4Addr>,
    used: HashSet<Ipv4Addr>,
}

impl LeasePool {
    pub fn new(start: Ipv4Addr, end: Ipv4Addr, exclude: Ipv4Addr) -> Result<Self, PoolError> {
        if u32::from(start) > u32::from(end) {
            return Err(PoolError::Inverted { start, end });
        }
        Ok(LeasePool {
            start: start.into(),
            end: end.into(),
            exclude,
            leases: HashMap::new(),
            used: HashSet::new(),
        })
    }

    /// Address for `mac`: its existing lease, else the lowest free address.
    /// `None` when the pool is exhausted.
    pub fn lease(&mut self, mac: Mac) -> Option<Ipv4Addr> {
        if let Some(&ip) = self.leases.get(&mac) {
            return Some(ip);
        }
        let ip = (self.start..=self.end)
            .map(Ipv4Addr::from)
            .find(|ip| *ip != self.exclude && !self.used.contains(ip))?;
        self.used.insert(ip);
        self.leases.insert(mac, ip);
        Some(ip)
    }

    /// Existing lease for `mac`, without allocating.
    pub fn get(&self, mac: &Mac) -> Option<Ipv4Addr> {
        self.leases.get(mac).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(n: u8) -> Mac {
        Mac([2, 0, 0, 0, 0, n])
    }

    fn ip(last: u8) -> Ipv4Addr {
        Ipv4Addr::new(192, 168, 88, last)
    }

    #[test]
    fn same_mac_gets_same_ip() {
        let mut pool = LeasePool::new(ip(10), ip(20), ip(1)).unwrap();
        let a = pool.lease(mac(1)).unwrap();
        assert_eq!(a, ip(10));
        assert_eq!(pool.lease(mac(1)), Some(a));
        assert_eq!(pool.get(&mac(1)), Some(a));
        assert_eq!(pool.lease(mac(2)), Some(ip(11)));
    }

    #[test]
    fn skips_server_ip() {
        let mut pool = LeasePool::new(ip(1), ip(3), ip(2)).unwrap();
        assert_eq!(pool.lease(mac(1)), Some(ip(1)));
        assert_eq!(pool.lease(mac(2)), Some(ip(3)));
    }

    #[test]
    fn exhaustion_returns_none_but_known_macs_keep_their_lease() {
        let mut pool = LeasePool::new(ip(10), ip(11), ip(1)).unwrap();
        assert!(pool.lease(mac(1)).is_some());
        assert!(pool.lease(mac(2)).is_some());
        assert_eq!(pool.lease(mac(3)), None);
        assert_eq!(pool.get(&mac(3)), None);
        assert_eq!(pool.lease(mac(1)), Some(ip(10)));
    }

    #[test]
    fn single_address_pool_equal_to_server_is_always_empty() {
        let mut pool = LeasePool::new(ip(1), ip(1), ip(1)).unwrap();
        assert_eq!(pool.lease(mac(1)), None);
    }

    #[test]
    fn inverted_range_is_rejected() {
        assert_eq!(
            LeasePool::new(ip(20), ip(10), ip(1)).unwrap_err(),
            PoolError::Inverted {
                start: ip(20),
                end: ip(10)
            }
        );
    }
}
