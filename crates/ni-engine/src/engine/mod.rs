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

mod core;
mod device;
mod start;

use crate::config::{EngineConfig, NetPlan};
use crate::event::{DeviceInfoView, DeviceState, EngineEvent, Failure, StartInfo};
use crate::prepare::asset_failure;
use crate::profile::Profile;
use ni_assets::{AssetEvent, Assets, HttpConfig};
use ni_proto::Mac;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
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
    pub auto_repeat: bool,
    /// Name of the active profile.
    pub profile: Option<String>,
    /// Sorted by MAC.
    pub devices: Vec<DeviceView>,
}

type Reply<T> = oneshot::Sender<Result<T, Failure>>;

pub(crate) enum Command {
    Start(Box<EngineConfig>, Reply<StartInfo>),
    Stop(Reply<()>),
    Flash(Mac, Box<Profile>, Reply<()>),
    Retry(Mac, Reply<()>),
    AutoRepeat(bool, Reply<()>),
    Snapshot(oneshot::Sender<Snapshot>),
}

/// Controls a running engine task. Cheap to clone.
#[derive(Clone)]
pub struct EngineHandle {
    cmds: mpsc::Sender<Command>,
    events: broadcast::Sender<EngineEvent>,
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

impl EngineHandle {
    /// Create the asset cache at `cache_root` and spawn the engine task
    /// (idle until [`EngineHandle::start`]). Must be called inside a tokio
    /// runtime. Error: `assets_failed`.
    pub fn spawn(cache_root: impl Into<PathBuf>, http: HttpConfig) -> Result<Self, Failure> {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let sink = events.clone();
        let assets = Assets::new(cache_root, http)
            .map_err(|e| asset_failure("assets_failed", &e))?
            .with_events(Arc::new(move |e| {
                if let Some(ev) = download_event(e) {
                    let _ = sink.send(ev);
                }
            }));
        let assets = Arc::new(assets);
        let (cmds, rx) = mpsc::channel(32);
        tokio::spawn(core::run(assets.clone(), events.clone(), rx));
        Ok(EngineHandle {
            cmds,
            events,
            assets,
        })
    }

    /// Events from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
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
    /// the background ([`EngineEvent::KernelsReady`]). Errors:
    /// `already_running`, `invalid_profile`-family codes, network codes
    /// from [`crate::resolve_network`], `invalid_network`, `port_in_use`,
    /// `bind_failed`.
    pub async fn start(&self, cfg: EngineConfig) -> Result<StartInfo, Failure> {
        self.call(|tx| Command::Start(Box::new(cfg), tx)).await?
    }

    /// Cancel all sessions and release the ports. Error: `not_running`.
    pub async fn stop(&self) -> Result<(), Failure> {
        self.call(Command::Stop).await?
    }

    /// Install `profile` on a device. A device that has not said hello yet
    /// keeps the profile for when it does. Errors: `not_running`,
    /// `unknown_device`, `device_busy`, `device_done`, profile validation.
    pub async fn flash(&self, mac: Mac, profile: Profile) -> Result<(), Failure> {
        self.call(|tx| Command::Flash(mac, Box::new(profile), tx))
            .await?
    }

    /// Re-arm a failed device. Errors: `not_running`, `unknown_device`,
    /// `not_failed`.
    pub async fn retry(&self, mac: Mac) -> Result<(), Failure> {
        self.call(|tx| Command::Retry(mac, tx)).await?
    }

    /// With auto-repeat on, waiting devices get the active profile.
    /// Errors: `not_running`, `no_profile` (turning it on without one).
    pub async fn set_auto_repeat(&self, on: bool) -> Result<(), Failure> {
        self.call(|tx| Command::AutoRepeat(on, tx)).await?
    }

    pub async fn snapshot(&self) -> Result<Snapshot, Failure> {
        self.call(Command::Snapshot).await
    }
}
