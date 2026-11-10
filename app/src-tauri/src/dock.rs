//! The Dock tile while the app runs: a badge with the number of devices
//! being prepared or flashed (none when idle), and one informational
//! bounce when a device finishes or fails while the app is in the
//! background.
//!
//! [`follow`] keeps a [`DockTracker`] in step with the engine (its events,
//! and a snapshot whenever events were missed or the engine task was
//! replaced); `lib.rs` applies the [`DockUpdate`]s to the window.

use ni_engine::{DeviceState, DeviceView, EngineEvent};
use ni_proto::Mac;
use std::collections::BTreeSet;
use std::future::Future;
use tokio::sync::broadcast::{self, error::RecvError};

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
/// while the app is not in front (the user is looking otherwise) and not
/// quitting.
pub fn should_bounce(finished: bool, app_focused: bool, quitting: bool) -> bool {
    finished && !app_focused && !quitting
}

/// Where [`follow`] gets the engine's events and devices from (the
/// current engine task; a fake in tests).
pub trait DockSource {
    /// A new subscription to the current engine task's events.
    fn subscribe(&mut self) -> broadcast::Receiver<EngineEvent>;
    /// The current engine task's devices (`None`: it did not answer).
    fn devices(&self) -> impl Future<Output = Option<Vec<DeviceView>>> + Send;
    /// Resolves when the engine task is replaced: `true`, or `false` when
    /// there will be no more engine (the app is quitting).
    fn replaced(&mut self) -> impl Future<Output = bool> + Send;
}

/// Follow the engine for the Dock tile and pass every change to `apply`,
/// until the source ends.
///
/// Missed events (a lagging subscription) and a replaced engine task are
/// handled in order, in this loop: drop the old subscription (what is
/// still queued there is older than anything a new snapshot shows),
/// subscribe again, *then* ask for the snapshot and apply it before any
/// event of the new subscription. Every event from after the snapshot is
/// therefore applied after it, and events between subscribing and the
/// snapshot only replay transitions that end where the snapshot is, so
/// the count cannot go stale (a device finishing during the resync is not
/// lost, and an old `flashing` cannot bring back a finished device).
pub async fn follow<S: DockSource>(mut source: S, mut apply: impl FnMut(DockUpdate)) {
    let mut dock = DockTracker::default();
    loop {
        let mut events = source.subscribe();
        if let Some(devices) = source.devices().await {
            apply(dock.resync(&devices));
        }
        loop {
            tokio::select! {
                // Queued events first: they happened before a replacement.
                biased;
                r = events.recv() => match r {
                    Ok(e) => apply(dock.on_event(&e)),
                    Err(RecvError::Lagged(_)) => break,
                    Err(RecvError::Closed) => {
                        if !source.replaced().await {
                            return;
                        }
                        break;
                    }
                },
                alive = source.replaced() => {
                    if !alive {
                        return;
                    }
                    break;
                }
            }
        }
    }
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
            source: None,
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
            source: None,
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
        assert!(should_bounce(true, false, false));
        assert!(!should_bounce(true, true, false), "in front");
        assert!(!should_bounce(false, false, false), "nothing finished");
        assert!(!should_bounce(true, false, true), "quitting");
    }

    /// A fake engine: its events channel, and a snapshot that runs
    /// `during` (events the engine sends while the snapshot is on its
    /// way) before answering with `devices`.
    struct Fake {
        tx: broadcast::Sender<EngineEvent>,
        snapshots: std::sync::Arc<std::sync::Mutex<Vec<Snap>>>,
        replaced: tokio::sync::mpsc::UnboundedReceiver<bool>,
    }

    struct Snap {
        during: Vec<EngineEvent>,
        devices: Vec<DeviceView>,
    }

    impl DockSource for Fake {
        fn subscribe(&mut self) -> broadcast::Receiver<EngineEvent> {
            self.tx.subscribe()
        }

        fn devices(&self) -> impl Future<Output = Option<Vec<DeviceView>>> + Send {
            let next = {
                let mut all = self.snapshots.lock().unwrap();
                (!all.is_empty()).then(|| all.remove(0))
            };
            let tx = self.tx.clone();
            async move {
                let snap = next?;
                for e in snap.during {
                    tx.send(e).unwrap();
                }
                tokio::task::yield_now().await;
                Some(snap.devices)
            }
        }

        fn replaced(&mut self) -> impl Future<Output = bool> + Send {
            let rx = &mut self.replaced;
            async move { rx.recv().await.unwrap_or(false) }
        }
    }

    /// Run [`follow`] over `fake`; returns the updates and the tracker's
    /// final badge once `fake`'s engine is gone.
    async fn run(fake: Fake) -> Vec<DockUpdate> {
        let mut got = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            follow(fake, |u| got.push(u)),
        )
        .await
        .expect("follow ends with the engine");
        got
    }

    fn fake(
        capacity: usize,
        snapshots: Vec<Snap>,
    ) -> (Fake, tokio::sync::mpsc::UnboundedSender<bool>) {
        let (tx, _) = broadcast::channel(capacity);
        let (replace, replaced) = tokio::sync::mpsc::unbounded_channel();
        let fake = Fake {
            tx,
            snapshots: std::sync::Arc::new(std::sync::Mutex::new(snapshots)),
            replaced,
        };
        (fake, replace)
    }

    /// The badge after all `updates`, starting from none.
    fn final_badge(updates: &[DockUpdate]) -> Option<String> {
        updates.iter().rev().find_map(|u| u.badge.clone()).flatten()
    }

    #[tokio::test]
    async fn a_device_finishing_while_the_snapshot_is_on_its_way_is_not_lost() {
        // The snapshot was taken while A was flashing; A finished right
        // after (its `done` arrives while the snapshot is on its way).
        // Applying the snapshot after that event would leave A busy.
        let (f, replace) = fake(
            16,
            vec![Snap {
                during: vec![device(A, DeviceState::Done)],
                devices: vec![view(A, flashing())],
            }],
        );
        let tx = f.tx.clone();
        let task = tokio::spawn(run(f));
        tokio::task::yield_now().await;
        drop(tx);
        replace.send(false).unwrap();
        let updates = task.await.unwrap();
        assert_eq!(final_badge(&updates), None, "{updates:?}");
        assert!(
            updates.iter().any(|u| u.finished),
            "A finished: {updates:?}"
        );
    }

    #[tokio::test]
    async fn stale_events_from_before_a_lag_do_not_bring_back_a_finished_device() {
        // The forwarder lags: what is still queued (A flashing) is older
        // than the snapshot taken afterwards, which shows A done.
        let (f, replace) = fake(
            2,
            vec![
                Snap {
                    during: vec![],
                    devices: vec![],
                },
                Snap {
                    during: vec![],
                    devices: vec![view(A, DeviceState::Done)],
                },
            ],
        );
        let tx = f.tx.clone();
        let task = tokio::spawn(run(f));
        // Let it subscribe and take the first snapshot, then overflow.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        for e in [
            device(B, preparing()),
            device(B, DeviceState::Discovered),
            device(A, preparing()),
            device(A, flashing()),
        ] {
            tx.send(e).unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        replace.send(false).unwrap();
        let updates = task.await.unwrap();
        assert_eq!(final_badge(&updates), None, "{updates:?}");
        assert!(!updates.iter().any(|u| u.finished), "{updates:?}");
    }

    #[tokio::test]
    async fn a_replaced_engine_is_recounted_from_its_snapshot() {
        let (f, replace) = fake(
            16,
            vec![
                Snap {
                    during: vec![],
                    devices: vec![view(A, flashing())],
                },
                Snap {
                    during: vec![],
                    devices: vec![view(B, preparing()), view(A, preparing())],
                },
            ],
        );
        let task = tokio::spawn(run(f));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        replace.send(true).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        replace.send(false).unwrap();
        let updates = task.await.unwrap();
        assert_eq!(final_badge(&updates).as_deref(), Some("2"), "{updates:?}");
    }
}
