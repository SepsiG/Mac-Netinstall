//! Per-device bookkeeping inside the engine loop.

use super::DeviceView;
use crate::event::{DeviceInfoView, DeviceState};
use crate::profile::Profile;
use ni_assets::Version;
use ni_bootp::BootArch;
use ni_proto::{DeviceInfo, Mac, ServerSession};
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch (0 before it).
pub(crate) fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Most devices the engine keeps track of. Beyond it the oldest finished
/// (done/failed) device is forgotten first, then the oldest idle one
/// (seen, discovered, waiting); devices being prepared or flashed never.
pub const MAX_DEVICES: usize = 1024;

pub(crate) struct Device {
    pub mac: Mac,
    pub ip: Option<Ipv4Addr>,
    pub boot_arch: Option<BootArch>,
    pub info: Option<DeviceInfo>,
    /// Assigned with `flash`; wins over the active profile.
    pub assigned: Option<Profile>,
    /// The profile of the current or queued attempt.
    pub using: Option<Profile>,
    /// The RouterOS version of the current attempt, once known.
    pub version: Option<Version>,
    /// `None` until the first state is set (nothing reported yet).
    pub state: Option<DeviceState>,
    /// Position in the concurrency queue while waiting for a slot.
    pub queued: Option<u64>,
    /// Incremented per preparation, so stale results are dropped.
    pub attempt: u64,
    pub session: Option<ServerSession>,
    /// When the last throttled progress state was reported.
    pub last_progress: Option<Instant>,
    /// Creation or last state change (for eviction).
    pub updated: Instant,
    /// Wall-clock time of creation, for the snapshot.
    pub first_seen: SystemTime,
    /// Wall-clock time of the last state change, for the snapshot.
    pub changed: SystemTime,
}

impl Device {
    pub fn new(mac: Mac) -> Self {
        Device {
            mac,
            ip: None,
            boot_arch: None,
            info: None,
            assigned: None,
            using: None,
            version: None,
            state: None,
            queued: None,
            attempt: 0,
            session: None,
            last_progress: None,
            updated: Instant::now(),
            first_seen: SystemTime::now(),
            changed: SystemTime::now(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.state.as_ref().is_some_and(DeviceState::is_active)
    }

    pub fn is(&self, name: &str) -> bool {
        self.state.as_ref().is_some_and(|s| s.name() == name)
    }

    fn is_finished(&self) -> bool {
        self.is("done") || self.is("failed")
    }

    pub fn view(&self) -> Option<DeviceView> {
        Some(DeviceView {
            mac: self.mac,
            ip: self.ip,
            boot_arch: self.boot_arch.map(|a| a.as_str().to_string()),
            info: self.info.as_ref().map(DeviceInfoView::from),
            profile: self
                .using
                .as_ref()
                .or(self.assigned.as_ref())
                .map(|p| p.name.clone()),
            version: self.version.as_ref().map(|v| v.to_string()),
            attempt: self.attempt,
            first_seen_ms: unix_ms(self.first_seen),
            updated_ms: unix_ms(self.changed),
            state: self.state.clone()?,
        })
    }
}

/// The device for `mac`, created if new; when the map is full, room is
/// made first (see [`MAX_DEVICES`]).
pub(crate) fn entry(devices: &mut BTreeMap<Mac, Device>, mac: Mac, cap: usize) -> &mut Device {
    if !devices.contains_key(&mac) {
        make_room(devices, cap);
    }
    devices.entry(mac).or_insert_with(|| Device::new(mac))
}

/// Forget devices until fewer than `cap` remain: the oldest finished ones
/// first, then the oldest idle ones; never active ones.
pub(crate) fn make_room(devices: &mut BTreeMap<Mac, Device>, cap: usize) {
    while devices.len() >= cap.max(1) {
        let oldest = |finished: bool| {
            devices
                .values()
                .filter(|d| !d.is_active() && d.is_finished() == finished)
                .min_by_key(|d| d.updated)
                .map(|d| d.mac)
        };
        match oldest(true).or_else(|| oldest(false)) {
            Some(mac) => {
                devices.remove(&mac);
            }
            None => return, // everything is busy: let the map grow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Failure, WaitReason};
    use std::time::Duration;

    fn dev(n: u8, state: DeviceState, age_ms: u64, t0: Instant) -> (Mac, Device) {
        let mac = Mac([2, 0, 0, 0, 0, n]);
        let mut d = Device::new(mac);
        d.state = Some(state);
        d.updated = t0 + Duration::from_millis(age_ms);
        (mac, d)
    }

    #[test]
    fn finished_devices_are_evicted_first_and_active_ones_never() {
        let t0 = Instant::now();
        let waiting = DeviceState::Waiting {
            reason: WaitReason::NoProfile,
        };
        let flashing = DeviceState::Flashing {
            version: "7.25.1".into(),
            phase: "uploading".into(),
            sent: 0,
            total: 1,
        };
        let mut map: BTreeMap<Mac, Device> = [
            dev(1, waiting.clone(), 0, t0),
            dev(2, DeviceState::Done, 5, t0),
            dev(3, DeviceState::failed(Failure::new("x")), 3, t0),
            dev(4, flashing, 1, t0),
        ]
        .into_iter()
        .collect();
        let macs = |m: &BTreeMap<Mac, Device>| m.keys().map(|k| k.0[5]).collect::<Vec<_>>();
        make_room(&mut map, 4);
        assert_eq!(macs(&map), [1, 2, 4], "oldest finished (failed) first");
        make_room(&mut map, 3);
        assert_eq!(macs(&map), [1, 4], "then the done one");
        make_room(&mut map, 2);
        assert_eq!(macs(&map), [4], "then idle ones");
        make_room(&mut map, 1);
        assert_eq!(macs(&map), [4], "never an active one");
        let new = Mac([2, 0, 0, 0, 0, 9]);
        entry(&mut map, new, 1);
        assert_eq!(map.len(), 2, "grows when everything is busy");
    }

    #[test]
    fn views_carry_attempts_and_wall_clock_times() {
        let mut d = Device::new(Mac([2, 0, 0, 0, 0, 1]));
        assert_eq!(d.view(), None, "nothing to show before the first state");
        d.state = Some(DeviceState::Discovered);
        d.attempt = 2;
        d.first_seen = UNIX_EPOCH + Duration::from_millis(1_000);
        d.changed = UNIX_EPOCH + Duration::from_millis(2_500);
        let v = d.view().unwrap();
        assert_eq!(
            (v.attempt, v.first_seen_ms, v.updated_ms),
            (2, 1_000, 2_500)
        );
        assert_eq!(unix_ms(UNIX_EPOCH - Duration::from_secs(1)), 0);
    }

    #[test]
    fn the_map_never_exceeds_the_cap_with_idle_devices() {
        let mut map = BTreeMap::new();
        for n in 0..(MAX_DEVICES as u32 + 100) {
            let b = n.to_be_bytes();
            let mac = Mac([2, 0, b[0], b[1], b[2], b[3]]);
            entry(&mut map, mac, MAX_DEVICES).state = Some(DeviceState::Discovered);
        }
        assert_eq!(map.len(), MAX_DEVICES);
    }
}
