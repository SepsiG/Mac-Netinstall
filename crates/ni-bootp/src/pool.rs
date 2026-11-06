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

/// Hands out addresses from `start..=end`, never an excluded one (the
/// server's own IP, the subnet's network and broadcast addresses) and
/// never a quarantined one. A MAC keeps its address for the pool's
/// lifetime; leases never expire (a Netinstall run is short).
///
/// Addresses are only ever consumed (a declined address is quarantined,
/// not returned), so allocation walks a cursor upwards: amortized O(1)
/// per new lease, and O(1) to report exhaustion.
#[derive(Debug, Clone)]
pub struct LeasePool {
    start: u64,
    /// Next candidate address (u64 so `end = 255.255.255.255` cannot wrap).
    next: u64,
    end: u64,
    /// Excluded at construction plus quarantined later; only addresses
    /// inside `start..=end` are ever stored.
    blocked: HashSet<Ipv4Addr>,
    leases: HashMap<Mac, Ipv4Addr>,
}

impl LeasePool {
    pub fn new(start: Ipv4Addr, end: Ipv4Addr, exclude: &[Ipv4Addr]) -> Result<Self, PoolError> {
        if u32::from(start) > u32::from(end) {
            return Err(PoolError::Inverted { start, end });
        }
        let mut pool = LeasePool {
            start: u32::from(start).into(),
            next: u32::from(start).into(),
            end: u32::from(end).into(),
            blocked: HashSet::new(),
            leases: HashMap::new(),
        };
        for &ip in exclude {
            pool.quarantine(ip);
        }
        Ok(pool)
    }

    fn contains(&self, ip: Ipv4Addr) -> bool {
        (self.start..=self.end).contains(&u64::from(u32::from(ip)))
    }

    /// Size of the range minus excluded and quarantined addresses
    /// (addresses already leased still count).
    pub fn usable(&self) -> u64 {
        self.end - self.start + 1 - self.blocked.len() as u64
    }

    /// Address for `mac`: its existing lease, else the lowest address not
    /// yet handed out. `None` when the pool is exhausted.
    pub fn lease(&mut self, mac: Mac) -> Option<Ipv4Addr> {
        if let Some(&ip) = self.leases.get(&mac) {
            return Some(ip);
        }
        while self.next <= self.end {
            // `next <= end <= u32::MAX`, so the cast is lossless.
            let ip = Ipv4Addr::from(self.next as u32);
            self.next += 1;
            if !self.blocked.contains(&ip) {
                self.leases.insert(mac, ip);
                return Some(ip);
            }
        }
        None
    }

    /// Existing lease for `mac`, without allocating.
    pub fn get(&self, mac: &Mac) -> Option<Ipv4Addr> {
        self.leases.get(mac).copied()
    }

    /// The MAC holding the lease for `ip`, if any (TFTP peers are known by
    /// address only). Linear in the number of leases.
    pub fn mac_of(&self, ip: Ipv4Addr) -> Option<Mac> {
        self.leases
            .iter()
            .find(|(_, &leased)| leased == ip)
            .map(|(&mac, _)| mac)
    }

    /// Never hand out `ip` (again) for this pool's lifetime. Addresses
    /// outside the pool are ignored (nothing to protect, and the blocked
    /// set must not grow from arbitrary input).
    pub fn quarantine(&mut self, ip: Ipv4Addr) {
        if self.contains(ip) {
            self.blocked.insert(ip);
        }
    }

    /// The client declined its address (DHCPDECLINE: someone else uses
    /// it). Drops `mac`'s lease and quarantines the address; returns it.
    pub fn decline(&mut self, mac: &Mac) -> Option<Ipv4Addr> {
        let ip = self.leases.remove(mac)?;
        self.quarantine(ip);
        Some(ip)
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
        let mut pool = LeasePool::new(ip(10), ip(20), &[ip(1)]).unwrap();
        let a = pool.lease(mac(1)).unwrap();
        assert_eq!(a, ip(10));
        assert_eq!(pool.lease(mac(1)), Some(a));
        assert_eq!(pool.get(&mac(1)), Some(a));
        assert_eq!(pool.lease(mac(2)), Some(ip(11)));
    }

    #[test]
    fn mac_of_finds_the_lease_holder() {
        let mut pool = LeasePool::new(ip(10), ip(20), &[]).unwrap();
        pool.lease(mac(1));
        pool.lease(mac(2));
        assert_eq!(pool.mac_of(ip(11)), Some(mac(2)));
        assert_eq!(pool.mac_of(ip(12)), None);
        pool.decline(&mac(2));
        assert_eq!(pool.mac_of(ip(11)), None);
    }

    #[test]
    fn skips_server_ip() {
        let mut pool = LeasePool::new(ip(1), ip(3), &[ip(2)]).unwrap();
        assert_eq!(pool.lease(mac(1)), Some(ip(1)));
        assert_eq!(pool.lease(mac(2)), Some(ip(3)));
    }

    #[test]
    fn exhaustion_returns_none_but_known_macs_keep_their_lease() {
        let mut pool = LeasePool::new(ip(10), ip(11), &[ip(1)]).unwrap();
        assert!(pool.lease(mac(1)).is_some());
        assert!(pool.lease(mac(2)).is_some());
        assert_eq!(pool.lease(mac(3)), None);
        assert_eq!(pool.get(&mac(3)), None);
        assert_eq!(pool.lease(mac(1)), Some(ip(10)));
    }

    #[test]
    fn single_address_pool_equal_to_server_is_always_empty() {
        let mut pool = LeasePool::new(ip(1), ip(1), &[ip(1)]).unwrap();
        assert_eq!(pool.lease(mac(1)), None);
    }

    #[test]
    fn skips_every_excluded_address() {
        let mut pool = LeasePool::new(ip(0), ip(4), &[ip(0), ip(1), ip(4)]).unwrap();
        assert_eq!(pool.lease(mac(1)), Some(ip(2)));
        assert_eq!(pool.lease(mac(2)), Some(ip(3)));
        assert_eq!(pool.lease(mac(3)), None);
    }

    #[test]
    fn whole_address_space_pool_does_not_overflow() {
        let top = Ipv4Addr::new(255, 255, 255, 255);
        let mut pool = LeasePool::new(Ipv4Addr::new(255, 255, 255, 254), top, &[]).unwrap();
        assert_eq!(pool.lease(mac(1)), Some(Ipv4Addr::new(255, 255, 255, 254)));
        assert_eq!(pool.lease(mac(2)), Some(top));
        assert_eq!(pool.lease(mac(3)), None);
        assert_eq!(pool.lease(mac(4)), None);
    }

    #[test]
    fn declined_lease_is_released_and_quarantined() {
        let mut pool = LeasePool::new(ip(10), ip(12), &[]).unwrap();
        assert_eq!(pool.lease(mac(1)), Some(ip(10)));
        assert_eq!(pool.decline(&mac(1)), Some(ip(10)));
        assert_eq!(pool.get(&mac(1)), None);
        assert_eq!(pool.decline(&mac(1)), None, "nothing left to decline");
        // The MAC gets a new address; ip(10) is never handed out again.
        assert_eq!(pool.lease(mac(1)), Some(ip(11)));
        assert_eq!(pool.lease(mac(2)), Some(ip(12)));
        assert_eq!(pool.lease(mac(3)), None);
    }

    #[test]
    fn quarantined_future_address_is_skipped() {
        let mut pool = LeasePool::new(ip(10), ip(12), &[]).unwrap();
        pool.quarantine(ip(11));
        assert_eq!(pool.lease(mac(1)), Some(ip(10)));
        assert_eq!(pool.lease(mac(2)), Some(ip(12)));
        assert_eq!(pool.lease(mac(3)), None);
    }

    #[test]
    fn addresses_outside_the_pool_are_never_blocked() {
        let mut pool = LeasePool::new(ip(10), ip(12), &[ip(1), ip(11)]).unwrap();
        assert_eq!(pool.blocked.len(), 1, "out-of-range exclusion dropped");
        for last in [0, 9, 13, 255] {
            pool.quarantine(ip(last));
        }
        pool.quarantine(Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(pool.blocked.len(), 1);
    }

    #[test]
    fn usable_counts_addresses_minus_exclusions() {
        assert_eq!(LeasePool::new(ip(10), ip(12), &[]).unwrap().usable(), 3);
        assert_eq!(
            LeasePool::new(ip(10), ip(12), &[ip(11), ip(11), ip(99)])
                .unwrap()
                .usable(),
            2
        );
        assert_eq!(LeasePool::new(ip(1), ip(1), &[ip(1)]).unwrap().usable(), 0);
        let all = LeasePool::new(Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, &[]).unwrap();
        assert_eq!(all.usable(), 1 << 32);
    }

    #[test]
    fn inverted_range_is_rejected() {
        assert_eq!(
            LeasePool::new(ip(20), ip(10), &[ip(1)]).unwrap_err(),
            PoolError::Inverted {
                start: ip(20),
                end: ip(10)
            }
        );
    }
}
