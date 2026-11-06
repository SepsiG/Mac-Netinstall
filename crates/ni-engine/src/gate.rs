//! Which devices may be served: post-flash cooldown and "only this MAC".
//!
//! Shared between the engine loop (Netinstall hellos) and the BOOTP runner
//! (through [`Gate::policy`]), so a device in cooldown gets no boot file.

use ni_bootp::MacPolicy;
use ni_proto::Mac;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// Verdict for one device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Recently flashed.
    Cooldown,
    /// Another device was selected (`only_mac`).
    NotSelected,
}

impl Verdict {
    /// The `reason` of an `ignored` event.
    pub fn reason(self) -> &'static str {
        match self {
            Verdict::Allow => "allowed",
            Verdict::Cooldown => "cooldown",
            Verdict::NotSelected => "not_selected",
        }
    }
}

#[derive(Debug, Default)]
struct State {
    cooldown_until: HashMap<Mac, Instant>,
    only: Option<Mac>,
}

/// Cheap to clone; clones share state.
#[derive(Debug, Clone, Default)]
pub struct Gate(Arc<Mutex<State>>);

impl Gate {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn check(&self, mac: Mac, now: Instant) -> Verdict {
        let mut s = self.lock();
        if s.only.is_some_and(|only| only != mac) {
            return Verdict::NotSelected;
        }
        match s.cooldown_until.get(&mac) {
            Some(&until) if now < until => Verdict::Cooldown,
            Some(_) => {
                s.cooldown_until.remove(&mac);
                Verdict::Allow
            }
            None => Verdict::Allow,
        }
    }

    pub fn start_cooldown(&self, mac: Mac, until: Instant) {
        self.lock().cooldown_until.insert(mac, until);
    }

    /// Drop expired cooldowns (so the map cannot grow without bound).
    pub fn prune(&self, now: Instant) {
        self.lock().cooldown_until.retain(|_, until| now < *until);
    }

    /// Devices in cooldown (expired entries not yet pruned included).
    pub fn cooldowns(&self) -> usize {
        self.lock().cooldown_until.len()
    }

    pub fn clear_cooldown(&self, mac: Mac) {
        self.lock().cooldown_until.remove(&mac);
    }

    pub fn set_only(&self, only: Option<Mac>) {
        self.lock().only = only;
    }

    /// For [`ni_bootp::BootpHandle::set_policy`]: allow iff
    /// [`Gate::check`] says [`Verdict::Allow`] now.
    pub fn policy(&self) -> MacPolicy {
        let gate = self.clone();
        Arc::new(move |mac| gate.check(mac, Instant::now()) == Verdict::Allow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const A: Mac = Mac([2, 0, 0, 0, 0, 1]);
    const B: Mac = Mac([2, 0, 0, 0, 0, 2]);

    #[test]
    fn cooldown_expires() {
        let gate = Gate::default();
        let t0 = Instant::now();
        gate.start_cooldown(A, t0 + Duration::from_secs(300));
        assert_eq!(gate.check(A, t0), Verdict::Cooldown);
        assert_eq!(gate.check(B, t0), Verdict::Allow);
        assert_eq!(gate.check(A, t0 + Duration::from_secs(300)), Verdict::Allow);
        assert_eq!(
            gate.check(A, t0),
            Verdict::Allow,
            "expired entries are dropped"
        );
        gate.start_cooldown(A, t0 + Duration::from_secs(300));
        gate.clear_cooldown(A);
        assert_eq!(gate.check(A, t0), Verdict::Allow);
    }

    #[test]
    fn expired_cooldowns_are_pruned() {
        let gate = Gate::default();
        let t0 = Instant::now();
        gate.start_cooldown(A, t0 + Duration::from_secs(1));
        gate.start_cooldown(B, t0 + Duration::from_secs(60));
        gate.prune(t0 + Duration::from_secs(2));
        assert_eq!(gate.cooldowns(), 1);
        assert_eq!(gate.check(B, t0), Verdict::Cooldown);
    }

    #[test]
    fn only_mac_wins_over_everything() {
        let gate = Gate::default();
        gate.set_only(Some(A));
        let now = Instant::now();
        assert_eq!(gate.check(A, now), Verdict::Allow);
        assert_eq!(gate.check(B, now), Verdict::NotSelected);
        gate.set_only(None);
        assert_eq!(gate.check(B, now), Verdict::Allow);
        assert_eq!(Verdict::NotSelected.reason(), "not_selected");
    }

    #[test]
    fn policy_follows_the_gate() {
        let gate = Gate::default();
        let policy = gate.policy();
        assert!(policy(A));
        gate.start_cooldown(A, Instant::now() + Duration::from_secs(60));
        assert!(!policy(A));
        assert!(policy(B));
    }
}
