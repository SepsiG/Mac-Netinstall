//! The Dock tile while the app runs: a badge with the number of devices
//! being prepared or flashed (none when idle), and one informational
//! bounce when a device finishes or fails while the app is in the
//! background.
//!
//! [`DockTracker`] follows the engine's events (and a snapshot after
//! missed events); `lib.rs` applies its [`DockUpdate`]s to the window.

use ni_engine::{DeviceState, DeviceView, EngineEvent};
use ni_proto::Mac;
use std::collections::BTreeSet;

/// The devices being prepared or flashed, as the engine reported them.
#[derive(Debug, Default)]
pub struct DockTracker {
    active: BTreeSet<Mac>,
}

/// What an event changed on the Dock tile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DockUpdate {
    /// The badge text changed to this (`None`: no badge).
    pub badge: Option<Option<String>>,
    /// A device being prepared or flashed finished (done or failed).
    pub finished: bool,
}

/// The badge for `busy` devices: their number, or none when idle.
pub fn badge_label(busy: usize) -> Option<String> {
    (busy > 0).then(|| busy.to_string())
}

/// Bounce the Dock icon once (informational) only for a finished device
/// while the app is not in front; the user is looking otherwise.
pub fn should_bounce(finished: bool, app_focused: bool) -> bool {
    finished && !app_focused
}

impl DockTracker {
    /// Devices being prepared or flashed.
    pub fn busy(&self) -> usize {
        self.active.len()
    }

    /// The current badge text.
    pub fn label(&self) -> Option<String> {
        badge_label(self.busy())
    }

    /// Follow one engine event.
    pub fn on_event(&mut self, event: &EngineEvent) -> DockUpdate {
        let before = self.label();
        let mut finished = false;
        match event {
            EngineEvent::Device { mac, state } => {
                if state.is_active() {
                    self.active.insert(*mac);
                } else {
                    let was_active = self.active.remove(mac);
                    finished = was_active
                        && matches!(state, DeviceState::Done | DeviceState::Failed { .. });
                }
            }
            EngineEvent::Forgotten { mac } => {
                self.active.remove(mac);
            }
            // A stopped (or replaced) engine prepares and flashes nothing.
            EngineEvent::Stopped => self.active.clear(),
            _ => {}
        }
        self.update(before, finished)
    }

    /// Start over from a snapshot's devices (after missed events or an
    /// engine replacement). Never asks for attention.
    pub fn resync(&mut self, devices: &[DeviceView]) -> DockUpdate {
        let before = self.label();
        self.active = devices
            .iter()
            .filter(|d| d.state.is_active())
            .map(|d| d.mac)
            .collect();
        self.update(before, false)
    }

    fn update(&self, before: Option<String>, finished: bool) -> DockUpdate {
        let now = self.label();
        DockUpdate {
            badge: (now != before).then_some(now),
            finished,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_engine::Failure;

    const A: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 1]);
    const B: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 2]);

    fn device(mac: Mac, state: DeviceState) -> EngineEvent {
        EngineEvent::Device { mac, state }
    }

    fn preparing() -> DeviceState {
        DeviceState::Preparing {
            version: None,
            config: None,
        }
    }

    fn flashing() -> DeviceState {
        DeviceState::Flashing {
            version: "7.25.1".into(),
            phase: "files".into(),
            sent: 1,
            total: 2,
        }
    }

    fn view(mac: Mac, state: DeviceState) -> DeviceView {
        DeviceView {
            mac,
            ip: None,
            boot_arch: None,
            info: None,
            profile: None,
            config: None,
            version: None,
            attempt: 1,
            first_seen_ms: 0,
            updated_ms: 0,
            state,
        }
    }

    #[test]
    fn the_badge_is_the_number_of_busy_devices_and_empty_when_idle() {
        assert_eq!(badge_label(0), None);
        assert_eq!(badge_label(1).as_deref(), Some("1"));
        assert_eq!(badge_label(12).as_deref(), Some("12"));
    }

    #[test]
    fn preparing_and_flashing_count_until_done_or_failed() {
        let mut t = DockTracker::default();
        let u = t.on_event(&device(A, preparing()));
        assert_eq!(u.badge, Some(Some("1".into())));
        assert!(!u.finished);
        // Preparing -> flashing: the same device, no change.
        assert_eq!(t.on_event(&device(A, flashing())), DockUpdate::default());
        assert_eq!(
            t.on_event(&device(B, flashing())).badge,
            Some(Some("2".into()))
        );
        let u = t.on_event(&device(A, DeviceState::Done));
        assert_eq!((u.badge, u.finished), (Some(Some("1".into())), true));
        let u = t.on_event(&device(B, DeviceState::failed(Failure::new("x"))));
        assert_eq!((u.badge, u.finished), (Some(None), true));
        assert_eq!(t.busy(), 0);
    }

    #[test]
    fn only_a_busy_device_finishing_counts_as_finished() {
        let mut t = DockTracker::default();
        // Never prepared here (e.g. a failure before preparing).
        let u = t.on_event(&device(A, DeviceState::failed(Failure::new("x"))));
        assert_eq!(u, DockUpdate::default());
        assert_eq!(
            t.on_event(&device(A, DeviceState::Done)),
            DockUpdate::default()
        );
        // Leaving the busy states other than by finishing.
        t.on_event(&device(A, preparing()));
        let u = t.on_event(&device(A, DeviceState::Discovered));
        assert_eq!((u.badge, u.finished), (Some(None), false));
    }

    #[test]
    fn stop_and_forget_clear_devices() {
        let mut t = DockTracker::default();
        t.on_event(&device(A, preparing()));
        t.on_event(&device(B, flashing()));
        let u = t.on_event(&EngineEvent::Forgotten { mac: A });
        assert_eq!(u.badge, Some(Some("1".into())));
        let u = t.on_event(&EngineEvent::Stopped);
        assert_eq!((u.badge, u.finished), (Some(None), false));
        // Unrelated events change nothing.
        assert_eq!(
            t.on_event(&EngineEvent::KernelsReady {
                version: "7.25.1".into()
            }),
            DockUpdate::default()
        );
    }

    #[test]
    fn a_snapshot_resets_the_count_without_attention() {
        let mut t = DockTracker::default();
        t.on_event(&device(A, preparing()));
        let u = t.resync(&[
            view(A, DeviceState::Done),
            view(B, flashing()),
            view(Mac([0x48, 0xa9, 0x8a, 0, 0, 3]), preparing()),
        ]);
        assert_eq!((u.badge, u.finished), (Some(Some("2".into())), false));
        assert_eq!(t.resync(&[]).badge, Some(None));
        assert_eq!(t.resync(&[]), DockUpdate::default());
    }

    #[test]
    fn the_dock_bounces_only_for_a_finished_device_in_the_background() {
        assert!(should_bounce(true, false));
        assert!(!should_bounce(true, true));
        assert!(!should_bounce(false, false));
    }
}
