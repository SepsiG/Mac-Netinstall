//! The engine: one tokio task owning the Netinstall socket, all install
//! sessions (one per device MAC) and the BOOTP/TFTP runner tasks.
//!
//! Commands go in through an [`EngineHandle`] (an mpsc channel); events
//! come out on a bounded `tokio::sync::broadcast` channel
//! ([`EngineHandle::subscribe`]). A slow subscriber misses events
//! (`RecvError::Lagged`) instead of slowing the engine down;
//! [`EngineHandle::snapshot`] gives the full current state at any time.
//!
//! The task ends (stopping everything) when the last handle is dropped.
//! Only the engine task holds the event channel's `Sender` (handles and
//! the asset download hook hold a `WeakSender`), so if the task ends for
//! any reason — including a panic — subscribers get `RecvError::Closed`
//! and commands fail with `engine_gone`.

mod core;
mod device;
mod start;

pub use device::MAX_DEVICES;

/// How many times, in this process, a stop's aborted tasks (listeners) did
/// not end in time (logged as "aborted engine tasks did not end in time";
/// their ports may then still be busy). For tests: it should stay 0.
#[doc(hidden)]
pub fn reap_timeouts() -> u64 {
    core::REAP_TIMEOUTS.load(std::sync::atomic::Ordering::Relaxed)
}

use crate::config::{EngineConfig, NetPlan};
use crate::event::{DeviceInfoView, DeviceState, EngineEvent, Failure, KernelState, StartInfo};
use crate::prepare::asset_failure;
use crate::profile::Profile;
use ni_assets::{AssetEvent, Assets, HttpConfig, Version};
use ni_proto::Mac;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};

/// Capacity of the event broadcast channel.
pub const EVENT_CAPACITY: usize = 1024;

/// One device in a [`Snapshot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceView {
    #[serde(with = "crate::event::mac_str")]
    pub mac: Mac,
    pub ip: Option<Ipv4Addr>,
    pub boot_arch: Option<String>,
    pub info: Option<DeviceInfoView>,
    /// Name of the profile assigned with `flash` (or in use).
    pub profile: Option<String>,
    /// That profile's configuration mode: `keep`, `default`, `empty` or
    /// `script`.
    pub config: Option<String>,
    /// RouterOS version of the current (or last) install attempt, once
    /// resolved.
    pub version: Option<String>,
    /// Where that profile's packages come from: `download` or `local`.
    pub source: Option<String>,
    /// Install attempts so far (preparations started). Lets a client that
    /// missed events tell "finished twice" from "finished once".
    pub attempt: u64,
    /// When the engine first saw the device (Unix time, milliseconds).
    pub first_seen_ms: u64,
    /// When its state last changed (Unix time, milliseconds).
    pub updated_ms: u64,
    pub state: DeviceState,
}

/// The engine's state at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub running: bool,
    pub started: Option<StartInfo>,
    pub network: Option<NetPlan>,
    /// Version of the boot kernels being served, once ready.
    pub kernel_version: Option<String>,
    /// The boot kernel state: `downloading` or `unavailable` while running
    /// means "running but no kernels" (nothing is served).
    pub kernels: KernelState,
    pub auto_repeat: bool,
    /// Name of the active profile.
    pub profile: Option<String>,
    /// Only this device is served (see `set_only_mac`).
    #[serde(with = "crate::event::mac_opt")]
    pub only_mac: Option<Mac>,
    /// Concurrency limit (0 when not running).
    pub max_parallel: usize,
    /// Post-flash cooldown in seconds (0 when not running).
    pub cooldown_secs: u64,
    /// Why the link is down (the last `link_down` reason), `None` while it
    /// is up or the engine is not running.
    pub link_down: Option<String>,
    /// Sends are refused with `EHOSTUNREACH` (macOS's Local Network
    /// permission is missing): `local_network_denied` was reported and no
    /// send has worked since.
    #[serde(default)]
    pub local_network_denied: bool,
    /// Sorted by MAC.
    pub devices: Vec<DeviceView>,
}

type Reply<T> = oneshot::Sender<Result<T, Failure>>;

pub(crate) enum Command {
    Start(Box<EngineConfig>, Reply<StartInfo>),
    Stop(Reply<()>),
    Flash(Mac, Box<Profile>, Reply<()>),
    Retry(Mac, Reply<()>),
    Cancel(Mac, Reply<()>),
    Forget(Mac, Reply<()>),
    AutoRepeat(bool, Reply<()>),
    ActiveProfile(Option<Box<Profile>>, Reply<()>),
    OnlyMac(Option<Mac>, Reply<()>),
    MaxParallel(usize, Reply<()>),
    Cooldown(Duration, Reply<()>),
    Snapshot(oneshot::Sender<Snapshot>),
    InUse(oneshot::Sender<BTreeSet<Version>>),
    /// Makes the engine task panic (tests of `engine_gone`).
    #[cfg(test)]
    Panic,
}

/// Controls a running engine task. Cheap to clone.
#[derive(Clone)]
pub struct EngineHandle {
    cmds: mpsc::Sender<Command>,
    /// Weak: only the engine task owns the `Sender` (see the module docs).
    events: broadcast::WeakSender<EngineEvent>,
    assets: Arc<Assets>,
}

fn gone() -> Failure {
    Failure::new("engine_gone")
}

/// Asset downloads as engine events (cache hits are not reported).
fn download_event(e: AssetEvent) -> Option<EngineEvent> {
    match e {
        AssetEvent::DownloadStarted { file, total } => Some(EngineEvent::Download {
            file,
            done: 0,
            total,
        }),
        AssetEvent::DownloadProgress { file, done, total } => {
            Some(EngineEvent::Download { file, done, total })
        }
        AssetEvent::DownloadFinished { file, size } => Some(EngineEvent::Download {
            file,
            done: size,
            total: Some(size),
        }),
        AssetEvent::CacheHit { .. } => None,
    }
}

/// The `netinstall-<version>.tar.gz` download carries the boot kernels.
fn is_kernel_tarball(file: &str) -> bool {
    file.starts_with("netinstall-") && file.ends_with(".tar.gz")
}

impl EngineHandle {
    /// Create the asset cache at `cache_root` and spawn the engine task
    /// (idle until [`EngineHandle::start`]). Must be called inside a tokio
    /// runtime. Error: `assets_failed`.
    pub fn spawn(cache_root: impl Into<PathBuf>, http: HttpConfig) -> Result<Self, Failure> {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let weak = events.downgrade();
        let sink = weak.clone();
        let (internal, internal_rx) = mpsc::channel(64);
        let progress = internal.clone();
        let assets = Assets::new(cache_root, http)
            .map_err(|e| asset_failure("assets_failed", &e))?
            .with_events(Arc::new(move |e| {
                let Some(ev) = download_event(e) else { return };
                if let EngineEvent::Download { file, done, total } = &ev {
                    if is_kernel_tarball(file) {
                        // Dropped if the engine is behind; the next one
                        // carries the newer count anyway.
                        let _ = progress.try_send(core::Internal::Download {
                            file: file.clone(),
                            done: *done,
                            total: *total,
                        });
                    }
                }
                if let Some(tx) = sink.upgrade() {
                    let _ = tx.send(ev);
                }
            }));
        let assets = Arc::new(assets);
        let (cmds, rx) = mpsc::channel(32);
        tokio::spawn(core::run(
            assets.clone(),
            events,
            rx,
            (internal, internal_rx),
        ));
        Ok(EngineHandle {
            cmds,
            events: weak,
            assets,
        })
    }

    /// Events from now on. If the engine task has ended, the receiver
    /// reports `RecvError::Closed` at once.
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        match self.events.upgrade() {
            Some(tx) => tx.subscribe(),
            None => broadcast::channel(1).1,
        }
    }

    /// The engine's asset manager (for preflight, registering scripts, ...).
    pub fn assets(&self) -> &Arc<Assets> {
        &self.assets
    }

    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, Failure> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(make(tx)).await.map_err(|_| gone())?;
        rx.await.map_err(|_| gone())
    }

    /// Bind the listeners and start serving. Boot kernels are fetched in
    /// the background ([`EngineEvent::KernelsReady`]). With a configured
    /// server IP this works while the interface has no link (reported as
    /// a `link_down` warning right after `started`). Errors:
    /// `already_running`, `invalid_cooldown`, profile validation codes,
    /// `interface_list_failed`, network codes from
    /// [`crate::resolve_network`] (incl. `pool_includes_reserved`),
    /// `invalid_network`, `port_in_use`, `bind_failed`.
    pub async fn start(&self, cfg: EngineConfig) -> Result<StartInfo, Failure> {
        self.call(|tx| Command::Start(Box::new(cfg), tx)).await?
    }

    /// Cancel all sessions and release the ports. Error: `not_running`.
    pub async fn stop(&self) -> Result<(), Failure> {
        self.call(Command::Stop).await?
    }

    /// Install `profile` on a device. A device that has not said hello yet
    /// keeps the profile for when it does. A device already waiting for a
    /// slot keeps its place in the queue. Errors: `not_running`,
    /// `unknown_device`, `device_busy`, `device_done`, profile validation,
    /// `profile_version_mismatch` (`version`, `kernel_version`): an exact
    /// version other than the boot kernels' (the device already runs the
    /// kernel it was served, so it cannot install another version); a
    /// channel is checked once resolved, and the device fails with that
    /// code. `kernel_version_unknown` (`mac`): the device was not seen
    /// netbooting and no kernels are served right now, so what it runs
    /// cannot be checked (a device in that state that would be installed
    /// automatically fails with that code).
    ///
    /// The kernel version of a device comes from the boot file name it was
    /// given (`kernel-<version>-<arch>.elf`, see
    /// [`crate::prepare::served_kernel_name`]) and is confirmed by the file
    /// it fetches over TFTP.
    pub async fn flash(&self, mac: Mac, profile: Profile) -> Result<(), Failure> {
        self.call(|tx| Command::Flash(mac, Box::new(profile), tx))
            .await?
    }

    /// Re-arm a failed device. Errors: `not_running`, `unknown_device`,
    /// `not_failed`. (A failed device that netboots again is reset by its
    /// BOOTP request and, with auto-repeat or an assigned profile, flashed
    /// again without `retry`.)
    pub async fn retry(&self, mac: Mac) -> Result<(), Failure> {
        self.call(|tx| Command::Retry(mac, tx)).await?
    }

    /// Stop one device. A device being prepared or flashed fails with
    /// `cancelled`; **a device cancelled while flashing may be left
    /// without a bootable system** (it was formatted) and must be
    /// netbooted again, so a UI should ask first. A device waiting for a
    /// slot leaves the queue (`waiting` / `no_profile`, its profile
    /// cleared). Errors: `not_running`, `unknown_device`,
    /// `not_cancellable` (`mac`, `state`).
    pub async fn cancel(&self, mac: Mac) -> Result<(), Failure> {
        self.call(|tx| Command::Cancel(mac, tx)).await?
    }

    /// Drop a device that is not being prepared or flashed from the
    /// device list, with its cooldown: if it netboots again it is treated
    /// as new. Reported as [`EngineEvent::Forgotten`]. A done device in its
    /// cooldown is refused (it may be rebooting into its new system, and
    /// must not be served again yet). Errors: `not_running`,
    /// `unknown_device`, `device_busy`, `device_in_cooldown` (`mac`).
    pub async fn forget(&self, mac: Mac) -> Result<(), Failure> {
        self.call(|tx| Command::Forget(mac, tx)).await?
    }

    /// With auto-repeat on, waiting devices get the active profile. Turning
    /// it off takes devices that wait for a slot only because of it (no
    /// profile assigned with `flash`) out of the queue (`waiting` /
    /// `no_profile`). Errors: `not_running`, `no_profile` (turning it on
    /// without one).
    pub async fn set_auto_repeat(&self, on: bool) -> Result<(), Failure> {
        self.call(|tx| Command::AutoRepeat(on, tx)).await?
    }

    /// Change the active profile while running (`None`: no profile; that
    /// also turns auto-repeat off). A profile with another version spec
    /// than the boot kernels were fetched for withdraws the kernels and
    /// fetches the new ones in the background ([`EngineEvent::Kernels`]:
    /// `downloading`, then `ready` or `unavailable`); meanwhile devices
    /// that netboot get no boot file. Devices queued by auto-repeat (no
    /// profile assigned with `flash`) take the new profile. A device keeps
    /// the kernel version it booted: installing another one on it fails
    /// with `profile_version_mismatch` until it netboots again. The same
    /// spec keeps the kernels, unless they are `unavailable`: then they are
    /// fetched again (a retry without stopping; also with `None`). Errors: `not_running`,
    /// profile validation codes.
    pub async fn set_active_profile(&self, profile: Option<Profile>) -> Result<(), Failure> {
        self.call(|tx| Command::ActiveProfile(profile.map(Box::new), tx))
            .await?
    }

    /// Serve only `mac` from now on (`None`: every device). Others get no
    /// boot file and their hellos are `ignored` (`not_selected`); queued
    /// ones stay queued (they are not started until it is lifted); devices
    /// already being prepared or flashed are not affected. Reported as
    /// [`EngineEvent::OnlyMacChanged`]. Error: `not_running`.
    pub async fn set_only_mac(&self, mac: Option<Mac>) -> Result<(), Failure> {
        self.call(|tx| Command::OnlyMac(mac, tx)).await?
    }

    /// Change the concurrency limit. A higher limit starts waiting devices
    /// at once; a lower one lets running installs finish. Errors:
    /// `not_running`, `invalid_max_parallel` (`min`, `max`, `value`).
    pub async fn set_max_parallel(&self, n: usize) -> Result<(), Failure> {
        self.call(|tx| Command::MaxParallel(n, tx)).await?
    }

    /// Change the post-flash cooldown for devices finishing from now on
    /// (running cooldowns keep their end). Errors: `not_running`,
    /// `invalid_cooldown` (`min_secs`, `max_secs`, `secs`; at least
    /// [`crate::MIN_COOLDOWN`], at most [`crate::MAX_COOLDOWN`]).
    pub async fn set_cooldown(&self, cooldown: Duration) -> Result<(), Failure> {
        self.call(|tx| Command::Cooldown(cooldown, tx)).await?
    }

    pub async fn snapshot(&self) -> Result<Snapshot, Failure> {
        self.call(Command::Snapshot).await
    }

    /// RouterOS versions the engine needs in the cache right now: the boot
    /// kernels it serves or is fetching, and the versions of devices being
    /// prepared or flashed (empty while stopped). A UI must not delete
    /// these.
    pub async fn versions_in_use(&self) -> Result<BTreeSet<Version>, Failure> {
        self.call(Command::InUse).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::broadcast::error::RecvError;

    #[test]
    fn kernel_tarballs_are_recognised() {
        assert!(is_kernel_tarball("netinstall-7.25.1.tar.gz"));
        assert!(!is_kernel_tarball("routeros-7.25.1-arm64.npk"));
        assert!(!is_kernel_tarball("netinstall-7.25.1.tar.gz.sha256"));
    }

    #[tokio::test]
    async fn a_dead_engine_closes_the_event_stream() {
        let dir = ni_assets::testutil::tempdir("engine-dead");
        let engine = EngineHandle::spawn(dir.join("cache"), HttpConfig::default()).unwrap();
        let mut events = engine.subscribe();
        let _ = engine.cmds.send(Command::Panic).await;
        let r = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("closed in time");
        assert_eq!(r, Err(RecvError::Closed));
        assert_eq!(engine.snapshot().await.unwrap_err().code, "engine_gone");
        let late = engine.subscribe().recv().await;
        assert_eq!(late, Err(RecvError::Closed));
    }
}
