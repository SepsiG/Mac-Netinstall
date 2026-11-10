//! The engine loop and its state machine.

use super::device::{self, Device, MAX_DEVICES};
use super::start::start;
use super::{Command, Snapshot};
use crate::config::{
    check_cooldown, check_max_parallel, link_status, EngineConfig, LinkStatus, NetPlan,
};
use crate::event::{
    DeviceInfoView, DeviceState, EngineEvent, Failure, KernelState, StartInfo, WaitReason,
};
use crate::gate::{Gate, Verdict};
use crate::prepare::{
    boot_maps, ensure_kernels, prepare, resolve_kernels, served_kernel_version, version_mismatch,
    Prepared, Target,
};
use crate::profile::Profile;
use crate::profile::{follows_kernels, kernel_spec, spec_to_string};
use ni_assets::{Assets, Channel, KernelSet, PkgArch, Version, VersionSpec};
use ni_bootp::{BootpEvent, BootpHandle, IgnoreReason};
use ni_net::{BroadcastTarget, RecvErrorAction, RecvErrorGuard};
use ni_proto::{DeviceInfo, FailReason, Frame, Mac, ServerSession};
use ni_tftp::{FileMap, TftpEvent};
use std::collections::{BTreeMap, HashMap};
use std::future::pending;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Minimum time between two throttled progress states of one device.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// Minimum time between two `ignored` events per device and reason.
const IGNORED_INTERVAL: Duration = Duration::from_secs(10);
/// Minimum time between two `send_failed` warnings.
const SEND_WARNING_INTERVAL: Duration = Duration::from_secs(1);
/// How often the `ignored` and cooldown maps are pruned (lazily, on
/// activity).
const PRUNE_INTERVAL: Duration = Duration::from_secs(1);
/// Consecutive watchdog checks that may see the interface with link and
/// other IPv4 addresses but not the server IP before the engine gives up
/// (`server_ip_not_on_interface`): the address is not coming back.
pub(crate) const FOREIGN_IP_CHECKS: u32 = 3;
/// How long `stop` waits for aborted tasks to release their sockets.
const REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// How often (in this process) aborted tasks did not end within
/// [`REAP_TIMEOUT`]; see [`super::reap_timeouts`].
pub(crate) static REAP_TIMEOUTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Messages from tasks the engine spawned.
pub(crate) enum Internal {
    /// A kernel fetch knows its version (its download progress is
    /// reported from now on).
    KernelResolved {
        epoch: u64,
        generation: u64,
        version: Version,
    },
    Kernels {
        epoch: u64,
        /// Which kernel fetch of this start (see `Running::kernel_gen`).
        generation: u64,
        result: Result<(Version, KernelSet, bool), Failure>,
    },
    /// Download progress from the asset manager (only the `netinstall`
    /// tarball, which carries the boot kernels).
    Download {
        file: String,
        done: u64,
        total: Option<u64>,
    },
    Prepared {
        epoch: u64,
        mac: Mac,
        attempt: u64,
        result: Result<Prepared, Failure>,
    },
    RunnerExited {
        epoch: u64,
        service: &'static str,
        detail: String,
    },
}

/// Sockets and channels the loop polls while running.
pub(crate) struct Io {
    pub socket: UdpSocket,
    /// Where Netinstall frames go (the limited broadcast, or the directed
    /// one if macOS refuses that).
    pub netinstall_dest: BroadcastTarget,
    pub bootp_rx: mpsc::Receiver<BootpEvent>,
    pub tftp_rx: mpsc::Receiver<TftpEvent>,
    pub recv_errors: RecvErrorGuard,
}

/// The watchdog's last view of the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Link {
    Up,
    /// Listed but unusable (reason code); `link_down` was reported.
    Down(&'static str),
    /// Not listed since then.
    Missing(Instant),
}

/// Failed sends since the last `send_failed` warning.
#[derive(Debug, Default)]
pub(crate) struct SendErrors {
    count: u64,
    last_report: Option<Instant>,
}

/// State while started.
pub(crate) struct Running {
    pub cfg: EngineConfig,
    pub net: NetPlan,
    pub info: StartInfo,
    pub bootp: BootpHandle,
    pub tftp_files: watch::Sender<Arc<FileMap>>,
    /// Runners, the kernel download and preparations; aborted on stop.
    pub tasks: Vec<JoinHandle<()>>,
    pub gate: Gate,
    pub devices: BTreeMap<Mac, Device>,
    pub kernel_version: Option<Version>,
    /// What the kernels were (or are being) fetched for.
    pub kernel_spec: Option<VersionSpec>,
    /// Incremented per kernel fetch, so a superseded fetch is dropped.
    pub kernel_gen: u64,
    /// The version the current fetch resolved to: only the download of
    /// its `netinstall` tarball is reported as kernel progress (a
    /// superseded fetch may still be downloading another one).
    pub kernel_resolved: Option<Version>,
    pub kernels: KernelState,
    pub queue_seq: u64,
    pub ignored: HashMap<(Mac, &'static str), Instant>,
    pub next_link_check: Option<Instant>,
    pub link: Link,
    /// Index of the interface at start (`IP_BOUND_IF` binds it).
    pub link_index: u32,
    /// The interface's IPv4 addresses (at start, then as of the last link
    /// check): frames from any of them on our port are our own.
    pub local_ips: Vec<std::net::Ipv4Addr>,
    /// Consecutive checks that saw other addresses but not the server IP.
    pub foreign_ip_checks: u32,
    pub send_errors: SendErrors,
    /// `local_network_denied` was reported and no send has worked since.
    pub local_network_denied: bool,
    pub next_prune: Instant,
    /// Hello arches resolved from package contents this session
    /// (lowercase hello arch → download arch; see `prepare::Prepared`).
    pub arch_map: HashMap<String, PkgArch>,
}

impl Running {
    /// Drop expired `ignored` rate-limit entries and cooldowns.
    fn prune(&mut self, now: Instant) {
        if now < self.next_prune {
            return;
        }
        self.next_prune = now + PRUNE_INTERVAL;
        self.ignored
            .retain(|_, t| now.saturating_duration_since(*t) < IGNORED_INTERVAL);
        self.gate.prune(now);
    }
}

struct Core {
    assets: Arc<Assets>,
    events: broadcast::Sender<EngineEvent>,
    internal: mpsc::Sender<Internal>,
    epoch: u64,
    run: Option<Running>,
    /// Tasks aborted by `stop`, awaited before the next start (and before
    /// `stop` replies) so their sockets are closed.
    aborted: Vec<JoinHandle<()>>,
}

/// `kernel_version_unknown` (`mac`): neither the boot kernel the device
/// netbooted nor a served one is known, so a profile cannot be checked
/// against it.
fn kernel_unknown(mac: Mac) -> Failure {
    Failure::new("kernel_version_unknown").with("mac", mac)
}

async fn recv_opt<T>(rx: Option<&mut mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => pending().await,
    }
}

/// Receive a datagram; transient errors are returned after a short
/// backoff, so a persistent error cannot spin the loop.
async fn recv_from_opt(
    socket: Option<&UdpSocket>,
    buf: &mut [u8],
) -> io::Result<(usize, SocketAddr)> {
    let Some(socket) = socket else {
        return pending().await;
    };
    let r = socket.recv_from(buf).await;
    if matches!(&r, Err(e) if ni_net::is_transient_recv_error(e)) {
        tokio::time::sleep(RecvErrorGuard::BACKOFF).await;
    }
    r
}

async fn sleep_opt(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(t.into()).await,
        None => pending().await,
    }
}

pub(crate) async fn run(
    assets: Arc<Assets>,
    events: broadcast::Sender<EngineEvent>,
    mut cmds: mpsc::Receiver<Command>,
    (internal, mut internal_rx): (mpsc::Sender<Internal>, mpsc::Receiver<Internal>),
) {
    let mut core = Core {
        assets,
        events,
        internal,
        epoch: 0,
        run: None,
        aborted: Vec::new(),
    };
    let mut io: Option<Io> = None;
    let mut buf = vec![0u8; 2048];
    loop {
        let deadline = core.next_deadline();
        let (socket, bootp_rx, tftp_rx) = match io.as_mut() {
            Some(io) => (
                Some(&io.socket),
                Some(&mut io.bootp_rx),
                Some(&mut io.tftp_rx),
            ),
            None => (None, None, None),
        };
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(cmd) => core.command(cmd, &mut io).await,
                None => {
                    core.stop(&mut io);
                    return;
                }
            },
            Some(msg) = internal_rx.recv() => core.internal(msg, &mut io),
            Some(ev) = recv_opt(bootp_rx) => core.on_bootp(ev),
            Some(ev) = recv_opt(tftp_rx) => core.on_tftp(ev),
            r = recv_from_opt(socket, &mut buf) => core.on_datagram(r, &buf, &mut io),
            _ = sleep_opt(deadline) => {}
        }
        core.drive(&mut io).await;
    }
}

impl Core {
    fn emit(&self, e: EngineEvent) {
        let _ = self.events.send(e);
    }

    fn next_deadline(&self) -> Option<Instant> {
        let run = self.run.as_ref()?;
        run.devices
            .values()
            .filter_map(|d| d.session.as_ref()?.poll_timeout())
            .chain(run.next_link_check)
            .min()
    }

    // ------------------------------------------------------------ commands

    async fn command(&mut self, cmd: Command, io: &mut Option<Io>) {
        match cmd {
            Command::Start(cfg, reply) => {
                self.reap().await;
                let _ = reply.send(self.start(*cfg, io));
            }
            Command::Stop(reply) => {
                let r = match self.run {
                    None => Err(Failure::new("not_running")),
                    Some(_) => {
                        self.stop(io);
                        Ok(())
                    }
                };
                self.reap().await;
                let _ = reply.send(r);
            }
            Command::Flash(mac, profile, reply) => {
                let _ = reply.send(self.flash(mac, *profile));
            }
            Command::Retry(mac, reply) => {
                let _ = reply.send(self.retry(mac));
            }
            Command::Cancel(mac, reply) => {
                let _ = reply.send(self.cancel(mac));
            }
            Command::Forget(mac, reply) => {
                let _ = reply.send(self.forget(mac));
            }
            Command::AutoRepeat(on, reply) => {
                let _ = reply.send(self.set_auto_repeat(on));
            }
            Command::ActiveProfile(profile, reply) => {
                let _ = reply.send(self.set_active_profile(profile.map(|p| *p)));
            }
            Command::OnlyMac(mac, reply) => {
                let _ = reply.send(self.set_only_mac(mac));
            }
            Command::MaxParallel(n, reply) => {
                let _ = reply.send(self.set_max_parallel(n));
            }
            Command::Cooldown(d, reply) => {
                let _ = reply.send(self.set_cooldown(d));
            }
            Command::Snapshot(reply) => {
                let _ = reply.send(self.snapshot());
            }
            Command::InUse(reply) => {
                let _ = reply.send(self.versions_in_use());
            }
            #[cfg(test)]
            Command::Panic => panic!("engine task panic requested by a test"),
        }
    }

    /// Wait (bounded) until the tasks aborted by `stop` have ended, so the
    /// listeners they owned are closed and their ports can be bound again.
    async fn reap(&mut self) {
        let tasks = std::mem::take(&mut self.aborted);
        let all = async {
            for task in tasks {
                let _ = task.await;
            }
        };
        if tokio::time::timeout(REAP_TIMEOUT, all).await.is_err() {
            REAP_TIMEOUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            warn!("aborted engine tasks did not end in time");
        }
    }

    fn start(&mut self, cfg: EngineConfig, io: &mut Option<Io>) -> Result<StartInfo, Failure> {
        if self.run.is_some() {
            return Err(Failure::new("already_running"));
        }
        let epoch = self.epoch + 1;
        let (running, new_io) = start(cfg, self.internal.clone(), epoch)?;
        self.epoch = epoch;
        let info = running.info.clone();
        info!(interface = %info.interface, server_ip = %info.server_ip, "engine started");
        let link = running.link;
        self.run = Some(running);
        *io = Some(new_io);
        self.emit(EngineEvent::Started(info.clone()));
        match link {
            Link::Up => {}
            Link::Down(reason) => self.link_down(reason),
            Link::Missing(_) => self.link_down("interface_missing"),
        }
        self.fetch_kernels();
        Ok(info)
    }

    /// Fetch the boot kernels for the active profile's version (stable
    /// without a profile) in the background. Nothing is served until they
    /// are ready: the old ones (if any) are withdrawn first, so no device
    /// boots a kernel of a version it will not get. A fetch still running
    /// is superseded (its result is dropped).
    fn fetch_kernels(&mut self) {
        let epoch = self.epoch;
        let assets = self.assets.clone();
        let internal = self.internal.clone();
        let Some(run) = self.run.as_mut() else { return };
        let spec = run
            .cfg
            .profile
            .as_ref()
            .map(kernel_spec)
            .unwrap_or(VersionSpec::Channel(Channel::Stable));
        run.kernel_gen += 1;
        let generation = run.kernel_gen;
        run.kernel_resolved = None;
        run.kernel_spec = Some(spec.clone());
        if run.kernel_version.take().is_some() {
            if let Err(e) = run.bootp.set_boot_files(HashMap::new()) {
                warn!("boot file names rejected: {e}");
            }
            run.tftp_files.send_replace(Arc::new(FileMap::new()));
        }
        run.tasks.retain(|t| !t.is_finished());
        run.tasks.push(tokio::spawn(async move {
            let result = match resolve_kernels(&assets, &spec).await {
                Ok((version, fallback)) => {
                    let _ = internal
                        .send(Internal::KernelResolved {
                            epoch,
                            generation,
                            version: version.clone(),
                        })
                        .await;
                    ensure_kernels(&assets, &version)
                        .await
                        .map(|set| (version, set, fallback))
                }
                Err(f) => Err(f),
            };
            let _ = internal
                .send(Internal::Kernels {
                    epoch,
                    generation,
                    result,
                })
                .await;
        }));
        let state = KernelState::Downloading {
            spec: spec_to_string(&run.kernel_spec.clone().expect("just set")),
            done: 0,
            total: None,
        };
        self.set_kernels(state);
    }

    /// Set the kernel state; report it if it changed.
    fn set_kernels(&mut self, state: KernelState) {
        let Some(run) = self.run.as_mut() else { return };
        if run.kernels == state {
            return;
        }
        run.kernels = state.clone();
        self.emit(EngineEvent::Kernels { state });
    }

    fn stop(&mut self, io: &mut Option<Io>) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        *io = None;
        for task in run.tasks.drain(..) {
            task.abort();
            self.aborted.push(task);
        }
        for dev in run.devices.values_mut() {
            if dev.is_active() {
                if let Some(s) = dev.session.as_mut() {
                    s.cancel();
                }
                self.emit(EngineEvent::Device {
                    mac: dev.mac,
                    state: DeviceState::failed(Failure::new("cancelled")),
                });
            }
        }
        info!("engine stopped");
        self.emit(EngineEvent::Stopped);
    }

    fn running(&mut self) -> Result<&mut Running, Failure> {
        self.run.as_mut().ok_or_else(|| Failure::new("not_running"))
    }

    fn flash(&mut self, mac: Mac, profile: Profile) -> Result<(), Failure> {
        profile.validate()?;
        let run = self.running()?;
        let dev = run
            .devices
            .get_mut(&mac)
            .ok_or_else(|| Failure::new("unknown_device").with("mac", mac))?;
        if dev.is_active() {
            return Err(Failure::new("device_busy").with("mac", mac));
        }
        if dev.is("done") {
            return Err(Failure::new("device_done").with("mac", mac));
        }
        let Some(kernel) = dev.kernel.as_ref().or(run.kernel_version.as_ref()) else {
            return Err(kernel_unknown(mac));
        };
        if let VersionSpec::Exact(v) = &profile.version {
            if follows_kernels(&profile) && v != kernel {
                return Err(version_mismatch(v, kernel));
            }
        }
        dev.assigned = Some(profile.clone());
        if dev.info.is_some() {
            self.enqueue(mac, profile);
        }
        Ok(())
    }

    fn retry(&mut self, mac: Mac) -> Result<(), Failure> {
        let run = self.running()?;
        let dev = run
            .devices
            .get(&mac)
            .ok_or_else(|| Failure::new("unknown_device").with("mac", mac))?;
        if !dev.is("failed") {
            return Err(Failure::new("not_failed").with("mac", mac));
        }
        run.gate.clear_cooldown(mac);
        if dev.info.is_some() {
            self.route(mac);
        } else {
            // Failed before its hello (boot kernel transfer): forget it; its
            // next BOOTP request starts over.
            run.devices.remove(&mac);
        }
        Ok(())
    }

    fn cancel(&mut self, mac: Mac) -> Result<(), Failure> {
        let run = self.running()?;
        let dev = run
            .devices
            .get_mut(&mac)
            .ok_or_else(|| Failure::new("unknown_device").with("mac", mac))?;
        if dev.is_active() {
            // A preparation still running is dropped when it reports back
            // (the device is no longer `preparing`).
            if let Some(mut session) = dev.session.take() {
                session.cancel();
            }
            warn!(%mac, "install cancelled");
            self.set_state(mac, DeviceState::failed(Failure::new("cancelled")));
            return Ok(());
        }
        if dev.queued.is_some() {
            dev.queued = None;
            dev.using = None;
            dev.assigned = None;
            self.set_state(
                mac,
                DeviceState::Waiting {
                    reason: WaitReason::NoProfile,
                },
            );
            return Ok(());
        }
        let state = dev.state.as_ref().map_or("unknown", DeviceState::name);
        Err(Failure::new("not_cancellable")
            .with("mac", mac)
            .with("state", state))
    }

    fn forget(&mut self, mac: Mac) -> Result<(), Failure> {
        let run = self.running()?;
        let dev = run
            .devices
            .get(&mac)
            .ok_or_else(|| Failure::new("unknown_device").with("mac", mac))?;
        if dev.is_active() {
            return Err(Failure::new("device_busy").with("mac", mac));
        }
        // Forgetting would end the cooldown, and a device that is rebooting
        // into its new system could be served (and flashed) again.
        if dev.is("done") && run.gate.in_cooldown(mac, Instant::now()) {
            return Err(Failure::new("device_in_cooldown").with("mac", mac));
        }
        run.devices.remove(&mac);
        run.gate.clear_cooldown(mac);
        run.ignored.retain(|(m, _), _| *m != mac);
        info!(%mac, "device forgotten");
        self.emit(EngineEvent::Forgotten { mac });
        Ok(())
    }

    fn set_auto_repeat(&mut self, on: bool) -> Result<(), Failure> {
        let run = self.running()?;
        if on && run.cfg.profile.is_none() {
            return Err(Failure::new("no_profile"));
        }
        run.cfg.auto_repeat = on;
        if !on {
            // Queued only because of auto-repeat: back to waiting for a
            // profile. Devices given one with `flash` keep their place.
            let dequeue: Vec<Mac> = run
                .devices
                .values_mut()
                .filter(|d| d.queued.is_some() && d.assigned.is_none())
                .map(|d| {
                    d.queued = None;
                    d.using = None;
                    d.mac
                })
                .collect();
            for mac in dequeue {
                self.set_state(
                    mac,
                    DeviceState::Waiting {
                        reason: WaitReason::NoProfile,
                    },
                );
            }
            return Ok(());
        }
        let waiting: Vec<Mac> = run
            .devices
            .values()
            .filter(|d| {
                d.state
                    == Some(DeviceState::Waiting {
                        reason: WaitReason::NoProfile,
                    })
            })
            .map(|d| d.mac)
            .collect();
        for mac in waiting {
            self.route(mac);
        }
        Ok(())
    }

    fn set_active_profile(&mut self, profile: Option<Profile>) -> Result<(), Failure> {
        if let Some(p) = &profile {
            p.validate()?;
        }
        let run = self.running()?;
        let Some(profile) = profile else {
            if run.cfg.auto_repeat {
                self.set_auto_repeat(false)?;
            }
            let run = self.running()?;
            run.cfg.profile = None;
            if matches!(run.kernels, KernelState::Unavailable { .. }) {
                self.fetch_kernels(); // try again (stable, without a profile)
            }
            return Ok(());
        };
        // The same spec keeps the kernels, unless they could not be had:
        // then this is how to try again without a restart.
        let refetch = run.kernel_spec.as_ref() != Some(&kernel_spec(&profile))
            || matches!(run.kernels, KernelState::Unavailable { .. });
        run.cfg.profile = Some(profile.clone());
        // Devices queued by auto-repeat take the new profile (and keep
        // their place); ones given a profile with `flash` keep theirs.
        for dev in run.devices.values_mut() {
            if dev.queued.is_some() && dev.assigned.is_none() {
                dev.using = Some(profile.clone());
            }
        }
        if refetch {
            self.fetch_kernels();
        }
        Ok(())
    }

    fn set_only_mac(&mut self, mac: Option<Mac>) -> Result<(), Failure> {
        let run = self.running()?;
        run.cfg.only_mac = mac;
        run.gate.set_only(mac);
        self.emit(EngineEvent::OnlyMacChanged { mac });
        Ok(())
    }

    fn set_max_parallel(&mut self, n: usize) -> Result<(), Failure> {
        check_max_parallel(n)?;
        self.running()?.cfg.max_parallel = n;
        self.admit();
        Ok(())
    }

    fn set_cooldown(&mut self, cooldown: Duration) -> Result<(), Failure> {
        let run = self.running()?;
        check_cooldown(cooldown, run.cfg.min_cooldown)?;
        run.cfg.cooldown = cooldown;
        Ok(())
    }

    fn versions_in_use(&self) -> std::collections::BTreeSet<Version> {
        let Some(run) = &self.run else {
            return Default::default();
        };
        run.kernel_version
            .iter()
            .chain(&run.kernel_resolved)
            .chain(
                run.devices
                    .values()
                    .filter(|d| d.is_active())
                    .flat_map(|d| d.version.iter().chain(&d.kernel)),
            )
            .cloned()
            .collect()
    }

    fn snapshot(&self) -> Snapshot {
        match &self.run {
            None => Snapshot {
                running: false,
                started: None,
                network: None,
                kernel_version: None,
                kernels: KernelState::Idle,
                auto_repeat: false,
                profile: None,
                only_mac: None,
                max_parallel: 0,
                cooldown_secs: 0,
                link_down: None,
                local_network_denied: false,
                devices: Vec::new(),
            },
            Some(run) => Snapshot {
                running: true,
                started: Some(run.info.clone()),
                network: Some(run.net.clone()),
                kernel_version: run.kernel_version.as_ref().map(|v| v.to_string()),
                kernels: run.kernels.clone(),
                auto_repeat: run.cfg.auto_repeat,
                profile: run.cfg.profile.as_ref().map(|p| p.name.clone()),
                only_mac: run.cfg.only_mac,
                max_parallel: run.cfg.max_parallel,
                cooldown_secs: run.cfg.cooldown.as_secs(),
                link_down: match run.link {
                    Link::Up => None,
                    Link::Down(reason) => Some(reason.to_string()),
                    Link::Missing(_) => Some("interface_missing".to_string()),
                },
                local_network_denied: run.local_network_denied,
                devices: run.devices.values().filter_map(Device::view).collect(),
            },
        }
    }

    // ------------------------------------------------------------ devices

    /// Set a device's state; report it if it changed.
    fn set_state(&mut self, mac: Mac, state: DeviceState) {
        let Some(run) = self.run.as_mut() else { return };
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
        };
        if dev.state.as_ref() == Some(&state) {
            return;
        }
        debug!(%mac, state = state.name(), "device state");
        dev.state = Some(state.clone());
        dev.updated = Instant::now();
        dev.changed = SystemTime::now();
        self.emit(EngineEvent::Device { mac, state });
    }

    fn ignored(&mut self, mac: Mac, reason: &'static str, now: Instant) {
        let Some(run) = self.run.as_mut() else { return };
        let last = run.ignored.get(&(mac, reason));
        if last.is_some_and(|t| now.saturating_duration_since(*t) < IGNORED_INTERVAL) {
            return;
        }
        run.ignored.insert((mac, reason), now);
        debug!(%mac, reason, "device ignored");
        self.emit(EngineEvent::Ignored {
            mac,
            reason: reason.to_string(),
        });
    }

    fn warning(&self, f: Failure) {
        warn!(code = %f.code, "{f}");
        self.emit(EngineEvent::Warning(f));
    }

    /// A discovered (or re-armed) device: queue it with its profile, or
    /// wait for one.
    fn route(&mut self, mac: Mac) {
        let Some(run) = self.run.as_mut() else { return };
        let Some(dev) = run.devices.get(&mac) else {
            return;
        };
        let active = run
            .cfg
            .auto_repeat
            .then(|| run.cfg.profile.clone())
            .flatten();
        match dev.assigned.clone().or(active) {
            Some(profile) => self.enqueue(mac, profile),
            None => self.set_state(
                mac,
                DeviceState::Waiting {
                    reason: WaitReason::NoProfile,
                },
            ),
        }
    }

    /// Queue a device with `profile`; one already queued keeps its place.
    fn enqueue(&mut self, mac: Mac, profile: Profile) {
        let Some(run) = self.run.as_mut() else { return };
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
        };
        let seq = match dev.queued {
            Some(seq) => seq,
            None => {
                run.queue_seq += 1;
                run.queue_seq
            }
        };
        dev.using = Some(profile);
        dev.queued = Some(seq);
        let active = run.devices.values().filter(|d| d.is_active()).count();
        if active < run.cfg.max_parallel.max(1) {
            self.start_prepare(mac);
        } else {
            self.set_state(
                mac,
                DeviceState::Waiting {
                    reason: WaitReason::Concurrency,
                },
            );
        }
    }

    /// Start queued devices while slots are free, oldest first.
    fn admit(&mut self) {
        loop {
            let Some(run) = self.run.as_ref() else { return };
            let active = run.devices.values().filter(|d| d.is_active()).count();
            if active >= run.cfg.max_parallel.max(1) {
                return;
            }
            // Devices "only this MAC" excludes stay queued until it is
            // lifted.
            let next = run
                .devices
                .values()
                .filter(|d| d.is("waiting") && run.gate.selected(d.mac))
                .filter_map(|d| Some((d.queued?, d.mac)))
                .min();
            match next {
                Some((_, mac)) => self.start_prepare(mac),
                None => return,
            }
        }
    }

    fn start_prepare(&mut self, mac: Mac) {
        let epoch = self.epoch;
        let assets = self.assets.clone();
        let internal = self.internal.clone();
        let Some(run) = self.run.as_mut() else { return };
        let kernel_spec = run.kernel_spec.clone();
        let served = run.kernel_version.clone();
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
        };
        let (Some(profile), Some(info)) = (dev.using.clone(), dev.info.clone()) else {
            return;
        };
        let known = run.arch_map.get(&arch_key(&info.arch)).copied();
        let target = Target::new(info)
            .with_boot_arch(dev.boot_arch)
            .with_known(known);
        dev.queued = None;
        // The kernels it booted (those served now if it was not seen
        // netbooting, e.g. it said hello right after a restart). Neither
        // known: it cannot be checked against the profile, so no install.
        let Some(kernel_version) = dev.kernel.clone().or_else(|| served.clone()) else {
            warn!(%mac, "boot kernel version unknown");
            self.set_state(mac, DeviceState::failed(kernel_unknown(mac)));
            return;
        };
        dev.attempt += 1;
        let attempt = dev.attempt;
        // Known without resolving: the profile's spec is what the served
        // kernels were resolved from, and the device runs those.
        let version = served.filter(|v| {
            follows_kernels(&profile)
                && kernel_spec.as_ref() == Some(&profile.version)
                && kernel_version == *v
        });
        dev.version = version.clone();
        let shown = version.as_ref().map(|v| v.to_string());
        let config = Some(profile.config.name().to_string());
        let source = Some(profile.package_source.as_str().to_string());
        run.tasks.retain(|t| !t.is_finished());
        run.tasks.push(tokio::spawn(async move {
            let result = prepare(&assets, &profile, version, Some(&kernel_version), &target).await;
            let _ = internal
                .send(Internal::Prepared {
                    epoch,
                    mac,
                    attempt,
                    result,
                })
                .await;
        }));
        self.set_state(
            mac,
            DeviceState::Preparing {
                version: shown,
                config,
                source,
            },
        );
    }

    fn prepared(&mut self, mac: Mac, attempt: u64, result: Result<Prepared, Failure>) {
        let Some(run) = self.run.as_mut() else { return };
        let session_cfg = run.cfg.session.clone();
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
        };
        if dev.attempt != attempt || !dev.is("preparing") {
            return;
        }
        let mut newer = None;
        let mut resolved = None;
        let state = match result {
            Err(f) => DeviceState::failed(f),
            Ok(Prepared {
                version,
                plan,
                resolved: now_resolved,
                ..
            }) => {
                let info = dev.info.clone().expect("prepared devices have said hello");
                let shown = version.as_ref().map_or(String::new(), Version::to_string);
                if let Some(arch) = now_resolved {
                    run.arch_map.insert(arch_key(&info.arch), arch);
                    resolved = Some(
                        Failure::new("arch_resolved")
                            .with("mac", mac)
                            .with("arch", info.arch.trim())
                            .with("download_arch", arch),
                    );
                }
                // Below the device's minimum: `prepare` refused it unless
                // the profile allows it; then say so and go on.
                newer = version
                    .as_ref()
                    .filter(|v| v.below_minimum(&info.min_os))
                    .map(|v| {
                        Failure::new("min_os_newer")
                            .with("mac", mac)
                            .with("min_os", info.min_os.trim())
                            .with("version", v)
                    });
                dev.version = version.clone();
                match ServerSession::new(info, &plan, session_cfg, Instant::now()) {
                    Ok(session) => {
                        dev.session = Some(session);
                        dev.last_progress = None;
                        DeviceState::Flashing {
                            version: shown,
                            phase: "offering".into(),
                            sent: 0,
                            total: plan.total_bytes(),
                        }
                    }
                    Err(e) => DeviceState::failed(Failure::new("invalid_plan").with("detail", e)),
                }
            }
        };
        if let Some(f) = resolved {
            info!(%mac, "{f}");
            self.emit(EngineEvent::Info(f));
        }
        if let Some(f) = newer {
            self.warning(f);
        }
        self.set_state(mac, state);
        self.admit();
    }

    fn on_hello(&mut self, info: DeviceInfo, now: Instant) {
        let mac = info.mac;
        let Some(run) = self.run.as_mut() else { return };
        let verdict = run.gate.check(mac, now);
        if verdict != Verdict::Allow {
            self.ignored(mac, verdict.reason(), now);
            return;
        }
        if run.devices.get(&mac).is_some_and(|d| d.is("done")) {
            // The cooldown is over, but a new round starts only with a new
            // BOOTP request: these hellos are stale (or the device was
            // power-cycled straight into the old kernel's session).
            self.ignored(mac, "awaiting_bootp", now);
            return;
        }
        let dev = device::entry(&mut run.devices, mac, MAX_DEVICES);
        if dev.info.is_some() {
            return; // already discovered (waiting, flashing, failed, ...)
        }
        dev.info = Some(info.clone());
        // The whole hello, so an unknown model can be diagnosed from the
        // log file (the event carries only what the UI shows).
        info!(
            %mac,
            model = %info.model,
            arch = %info.arch,
            license_id = %info.license_id,
            license_key = %info.license_key,
            min_os = %info.min_os,
            extra = ?info.extra,
            hello_dst = %info.hello_dst,
            "device discovered"
        );
        self.emit(EngineEvent::Discovered {
            mac,
            info: DeviceInfoView::from(&info),
        });
        self.set_state(mac, DeviceState::Discovered);
        self.route(mac);
    }

    // ------------------------------------------------------------ inputs

    fn on_datagram(&mut self, r: io::Result<(usize, SocketAddr)>, buf: &[u8], io: &mut Option<Io>) {
        let now = Instant::now();
        let (n, from) = match r {
            Ok((n, from)) => {
                if let Some(io) = io.as_mut() {
                    io.recv_errors.on_success();
                }
                (n, from)
            }
            Err(e) => {
                let Some(guard) = io.as_mut().map(|io| &mut io.recv_errors) else {
                    return;
                };
                match guard.on_error(&e, now) {
                    RecvErrorAction::Fatal => {
                        self.emit(EngineEvent::Error(
                            Failure::new("runner_failed")
                                .with("service", "netinstall")
                                .with("detail", e),
                        ));
                        self.stop(io);
                    }
                    RecvErrorAction::Retry {
                        report: Some(count),
                    } => self.warning(
                        Failure::new("recv_error")
                            .with("service", "netinstall")
                            .with("count", count)
                            .with("detail", e),
                    ),
                    RecvErrorAction::Retry { report: None } => {}
                }
                return;
            }
        };
        let Ok(frame) = Frame::decode(&buf[..n]) else {
            return;
        };
        let Some(run) = self.run.as_mut() else { return };
        // Our own broadcasts come back from our port and an address of the
        // interface: the server IP, or another one the interface has (the
        // source address of a broadcast is not necessarily the server IP).
        // Their MAC fields are the device's, so they cannot be told apart
        // by MAC; a session would ignore them anyway (`ack` is `seq - 2`),
        // but an echoed offer must not look like a hello.
        let own_port = io
            .as_ref()
            .and_then(|io| io.socket.local_addr().ok())
            .map(|a| a.port());
        let own_ip = match from.ip() {
            IpAddr::V4(ip) => ip == run.net.server_ip || run.local_ips.contains(&ip),
            IpAddr::V6(_) => false,
        };
        if own_ip && Some(from.port()) == own_port {
            return;
        }
        if let Some(session) = run
            .devices
            .get_mut(&frame.src)
            .and_then(|d| d.session.as_mut())
        {
            session.handle_frame(&frame, now);
            return;
        }
        if DeviceInfo::is_hello(&frame) {
            if let Some(info) = DeviceInfo::parse(&frame) {
                self.on_hello(info, now);
            }
        }
    }

    fn on_bootp(&mut self, ev: BootpEvent) {
        let now = Instant::now();
        let Some(run) = self.run.as_mut() else { return };
        match ev {
            BootpEvent::Replied {
                mac,
                arch,
                ip,
                file,
                ..
            } => {
                self.sends_work();
                let Some(run) = self.run.as_mut() else { return };
                let dev = device::entry(&mut run.devices, mac, MAX_DEVICES);
                if dev.is_active() {
                    debug!(%mac, "BOOTP from a device being flashed");
                    return;
                }
                // Netbooting again: a done device starts a new round; a
                // failed one is reset (it keeps a profile assigned with
                // `flash`) and, once it says hello, is flashed again if it
                // has a profile (assigned, or the active one with
                // auto-repeat). No `retry` needed.
                if dev.is("done") {
                    *dev = Device::new(mac);
                }
                dev.boot_arch = Some(arch);
                dev.ip = Some(ip);
                // The version is in the file name it was given: that is the
                // only kernel it can fetch (see `served_kernel_name`), even
                // if the kernels were swapped since the reply was sent.
                dev.kernel = served_kernel_version(&file);
                dev.info = None;
                dev.queued = None;
                self.set_state(
                    mac,
                    DeviceState::BootpSeen {
                        boot_arch: arch.as_str().to_string(),
                        ip,
                    },
                );
            }
            BootpEvent::Ignored { mac, reason } => match reason {
                IgnoreReason::Denied(_) => {
                    let verdict = run.gate.check(mac, now);
                    if verdict != Verdict::Allow {
                        self.ignored(mac, verdict.reason(), now);
                    }
                }
                IgnoreReason::NoBootFile(_) => self.ignored(mac, "no_boot_file", now),
                IgnoreReason::PoolExhausted(_) => self.ignored(mac, "pool_exhausted", now),
                IgnoreReason::UnsupportedDevice(_) => self.ignored(mac, "unsupported_device", now),
                other => debug!(%mac, ?other, "BOOTP request ignored"),
            },
            BootpEvent::SendFailed {
                mac,
                error,
                host_unreachable: true,
            } => {
                debug!(%mac, "BOOTP reply refused: {error}");
                self.local_network_denied("bootp", Some(mac));
            }
            BootpEvent::SendFailed { mac, error, .. } => self.warning(
                Failure::new("bootp_send_failed")
                    .with("mac", mac)
                    .with("detail", error),
            ),
            BootpEvent::BroadcastFallback { to } => {
                info!(%to, "BOOTP replies go to the directed broadcast (the limited one is refused)");
            }
            BootpEvent::RecvError { error, count } => self.warning(
                Failure::new("recv_error")
                    .with("service", "bootp")
                    .with("count", count)
                    .with("detail", error),
            ),
            other => debug!(?other, "BOOTP"),
        }
    }

    /// The device a TFTP peer belongs to, by its BOOTP lease.
    fn tftp_device(&self, peer: SocketAddr) -> Option<Mac> {
        let run = self.run.as_ref()?;
        let IpAddr::V4(ip) = peer.ip() else {
            return None;
        };
        let mac = run.bootp.mac_of(ip)?;
        let dev = run.devices.get(&mac)?;
        (!dev.is_active() && dev.info.is_none()).then_some(mac)
    }

    fn on_tftp(&mut self, ev: TftpEvent) {
        match ev {
            TftpEvent::Started { peer, size, file } => {
                if let Some(mac) = self.tftp_device(peer) {
                    // What it really boots, by the file it asked for.
                    if let Some(version) = served_kernel_version(&file) {
                        if let Some(dev) = self.run.as_mut().and_then(|r| r.devices.get_mut(&mac)) {
                            dev.kernel = Some(version);
                        }
                    }
                    self.set_state(
                        mac,
                        DeviceState::KernelServing {
                            sent: 0,
                            total: size,
                        },
                    );
                }
            }
            TftpEvent::Progress {
                peer, acked, size, ..
            } => {
                self.sends_work();
                if let Some(mac) = self.tftp_device(peer) {
                    let state = DeviceState::KernelServing {
                        sent: acked,
                        total: size,
                    };
                    self.set_state(mac, state);
                }
            }
            TftpEvent::Completed { peer, size, .. } => {
                self.sends_work();
                if let Some(mac) = self.tftp_device(peer) {
                    let state = DeviceState::KernelServing {
                        sent: size,
                        total: size,
                    };
                    self.set_state(mac, state);
                }
            }
            TftpEvent::Failed {
                peer,
                error: ni_tftp::TransferError::HostUnreachable(error),
                ..
            } => {
                debug!(%peer, "TFTP send refused: {error}");
                let mac = self.tftp_device(peer);
                self.local_network_denied("tftp", mac);
            }
            TftpEvent::Failed { peer, error, .. } => {
                if let Some(mac) = self.tftp_device(peer) {
                    let f = Failure::new("kernel_transfer_failed").with("detail", error);
                    self.set_state(mac, DeviceState::failed(f));
                }
            }
            TftpEvent::Rejected { peer, file, code } => self.warning(
                Failure::new("tftp_rejected")
                    .with("peer", peer)
                    .with("file", file.unwrap_or_default())
                    .with("code", format!("{code:?}")),
            ),
            TftpEvent::RecvError { error, count } => self.warning(
                Failure::new("recv_error")
                    .with("service", "tftp")
                    .with("count", count)
                    .with("detail", error),
            ),
        }
    }

    fn internal(&mut self, msg: Internal, io: &mut Option<Io>) {
        match msg {
            Internal::Kernels {
                epoch,
                generation,
                result,
            } if epoch == self.epoch => {
                let Some(run) = self.run.as_mut() else { return };
                if generation != run.kernel_gen {
                    return; // superseded by `set_active_profile`
                }
                match result {
                    Ok((version, set, fallback)) => {
                        let (boot, files) = boot_maps(&version, &set);
                        if let Err(e) = run.bootp.set_boot_files(boot) {
                            warn!("boot file names rejected: {e}");
                        }
                        run.tftp_files.send_replace(Arc::new(files));
                        run.kernel_version = Some(version.clone());
                        if fallback {
                            self.warning(
                                Failure::new("kernels_offline_fallback").with("version", &version),
                            );
                        }
                        info!(%version, "boot kernels ready");
                        self.set_kernels(KernelState::Ready {
                            version: version.to_string(),
                            offline: fallback,
                        });
                        self.emit(EngineEvent::KernelsReady {
                            version: version.to_string(),
                        });
                    }
                    Err(f) => {
                        warn!("boot kernels unavailable: {f}");
                        self.set_kernels(KernelState::Unavailable {
                            code: f.code.clone(),
                            params: f.params.clone(),
                        });
                        self.emit(EngineEvent::Error(f));
                    }
                }
            }
            Internal::KernelResolved {
                epoch,
                generation,
                version,
            } if epoch == self.epoch => {
                if let Some(run) = self.run.as_mut().filter(|r| r.kernel_gen == generation) {
                    run.kernel_resolved = Some(version);
                }
            }
            Internal::Download { file, done, total } => {
                let Some(run) = self.run.as_ref() else { return };
                let current = run
                    .kernel_resolved
                    .as_ref()
                    .is_some_and(|v| file == format!("netinstall-{v}.tar.gz"));
                if !current {
                    return; // another (superseded) fetch's download
                }
                if let KernelState::Downloading { spec, .. } = &run.kernels {
                    let state = KernelState::Downloading {
                        spec: spec.clone(),
                        done,
                        total,
                    };
                    debug!(%file, done, "kernel download");
                    self.set_kernels(state);
                }
            }
            Internal::Prepared {
                epoch,
                mac,
                attempt,
                result,
            } if epoch == self.epoch => self.prepared(mac, attempt, result),
            Internal::RunnerExited {
                epoch,
                service,
                detail,
            } if epoch == self.epoch && self.run.is_some() => {
                self.emit(EngineEvent::Error(
                    Failure::new("runner_failed")
                        .with("service", service)
                        .with("detail", detail),
                ));
                self.stop(io);
            }
            _ => {} // from an earlier start
        }
    }

    // ------------------------------------------------------------ driving

    /// Advance sessions, send their frames, report their events, admit
    /// waiting devices and watch the interface.
    async fn drive(&mut self, io: &mut Option<Io>) {
        let now = Instant::now();
        let Some(run) = self.run.as_mut() else { return };
        let mut frames = Vec::new();
        let mut events = Vec::new();
        for dev in run.devices.values_mut() {
            let Some(session) = dev.session.as_mut() else {
                continue;
            };
            if session.poll_timeout().is_some_and(|t| t <= now) {
                session.handle_timeout(now);
            }
            while let Some(frame) = session.poll_transmit(now) {
                frames.push(frame.encode());
            }
            while let Some(ev) = session.poll_event() {
                events.push((dev.mac, ev));
            }
        }
        run.prune(now);
        let mut send_error = None;
        let mut sent = false;
        if let Some(io) = io.as_mut() {
            let socket = &io.socket;
            for frame in &frames {
                match io
                    .netinstall_dest
                    .send(|to| socket.send_to(frame, to))
                    .await
                {
                    Ok(_) => sent = true,
                    Err(e) => {
                        debug!("netinstall send failed: {e}");
                        send_error = Some(e);
                    }
                }
            }
            if io.netinstall_dest.take_switched() {
                info!(
                    to = %io.netinstall_dest.current(),
                    "Netinstall frames go to the directed broadcast (the limited one is refused)"
                );
            }
        }
        if sent {
            self.sends_work();
        }
        match send_error {
            Some(e) if ni_net::is_host_unreachable(&e) => {
                self.local_network_denied("netinstall", None)
            }
            Some(e) => self.send_failed("netinstall", &e, now),
            None => {}
        }
        for (mac, ev) in events {
            self.session_event(mac, ev, now);
        }
        self.admit();
        self.check_link(now, io);
    }

    /// A send refused with `EHOSTUNREACH`: on macOS, the app's Local
    /// Network permission is missing (or not answered yet).
    /// `local_network_denied` (`service`) is reported once per start, and
    /// again only after a send worked in between ([`Self::sends_work`]);
    /// the device it was for (if known and not being flashed) fails with
    /// the same code, so its row says why. Each refused packet is only
    /// logged at debug level.
    fn local_network_denied(&mut self, service: &'static str, mac: Option<Mac>) {
        let Some(run) = self.run.as_mut() else { return };
        if let Some(mac) = mac {
            let dev = device::entry(&mut run.devices, mac, MAX_DEVICES);
            if !dev.is_active() {
                let f = Failure::new("local_network_denied").with("mac", mac);
                self.set_state(mac, DeviceState::failed(f));
            }
        }
        let Some(run) = self.run.as_mut() else { return };
        if std::mem::replace(&mut run.local_network_denied, true) {
            return;
        }
        self.warning(Failure::new("local_network_denied").with("service", service));
    }

    /// A send worked: after `local_network_denied`, `local_network_allowed`
    /// (once).
    fn sends_work(&mut self) {
        let Some(run) = self.run.as_mut() else { return };
        if std::mem::take(&mut run.local_network_denied) {
            self.warning(Failure::new("local_network_allowed"));
        }
    }

    /// A failed send: `send_failed` (`service`, `count` failures since the
    /// last report, `detail`), at most once per [`SEND_WARNING_INTERVAL`],
    /// and not while the link is known to be down (`link_down` says it).
    /// `EHOSTUNREACH` is not one of these: see
    /// [`Self::local_network_denied`].
    fn send_failed(&mut self, service: &str, e: &io::Error, now: Instant) {
        let Some(run) = self.run.as_mut() else { return };
        let errors = &mut run.send_errors;
        errors.count += 1;
        if run.link != Link::Up
            || errors
                .last_report
                .is_some_and(|t| now.saturating_duration_since(t) < SEND_WARNING_INTERVAL)
        {
            return;
        }
        let count = std::mem::take(&mut errors.count);
        errors.last_report = Some(now);
        self.warning(
            Failure::new("send_failed")
                .with("service", service)
                .with("count", count)
                .with("detail", e),
        );
    }

    fn session_event(&mut self, mac: Mac, ev: ni_proto::Event, now: Instant) {
        let Some(run) = self.run.as_mut() else { return };
        let cooldown = run.cfg.cooldown;
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
        };
        let (version, phase, sent, total) = match &dev.state {
            Some(DeviceState::Flashing {
                version,
                phase,
                sent,
                total,
            }) => (version.clone(), phase.clone(), *sent, *total),
            _ => (String::new(), String::new(), 0, 0),
        };
        match ev {
            ni_proto::Event::PhaseChanged(p) => {
                dev.last_progress = Some(now);
                let phase = p.name().to_string();
                let state = DeviceState::Flashing {
                    version,
                    phase,
                    sent,
                    total,
                };
                self.set_state(mac, state);
            }
            ni_proto::Event::Progress {
                bytes_sent,
                bytes_total,
                ..
            } => {
                let state = DeviceState::Flashing {
                    version,
                    phase,
                    sent: bytes_sent,
                    total: bytes_total,
                };
                let due = dev
                    .last_progress
                    .is_none_or(|t| now.saturating_duration_since(t) >= PROGRESS_INTERVAL);
                if due || bytes_sent == bytes_total {
                    dev.last_progress = Some(now);
                    self.set_state(mac, state);
                } else {
                    dev.state = Some(state);
                }
            }
            ni_proto::Event::FinishUnconfirmed => {
                // `TERM` was sent anyway; `Done` follows.
                warn!(%mac, "device never confirmed the end of the upload");
                self.warning(Failure::new("finish_unconfirmed").with("mac", mac));
            }
            ni_proto::Event::Done => {
                dev.session = None;
                // `cooldown` is at most `MAX_COOLDOWN`; never panic anyway.
                let until = now
                    .checked_add(cooldown)
                    .or_else(|| now.checked_add(crate::config::MAX_COOLDOWN))
                    .unwrap_or(now);
                run.gate.start_cooldown(mac, until);
                info!(%mac, "device flashed");
                self.set_state(mac, DeviceState::Done);
            }
            ni_proto::Event::Failed(reason) => {
                dev.session = None;
                let f = match reason {
                    FailReason::Timeout { phase } => {
                        Failure::new("flash_timeout").with("phase", phase)
                    }
                    FailReason::Rejected { phase } => {
                        Failure::new("flash_rejected").with("phase", phase)
                    }
                    FailReason::EndedEarly { remaining } => {
                        Failure::new("flash_ended_early").with("remaining", remaining)
                    }
                    FailReason::Cancelled => Failure::new("cancelled"),
                };
                warn!(%mac, "flashing failed: {f}");
                self.set_state(mac, DeviceState::failed(f));
            }
        }
    }

    fn link_down(&self, reason: &str) {
        let Some(run) = self.run.as_ref() else { return };
        self.warning(
            Failure::new("link_down")
                .with("interface", &run.cfg.interface)
                .with("reason", reason),
        );
    }

    /// The link watchdog (every `link_check`). A lost carrier or server IP
    /// is reported once (`link_down`) and the engine keeps running:
    /// sessions in flight fail by their own timeouts, the sockets work
    /// again when the link is back (`link_up`; see `start.rs` for why no
    /// rebind is needed). Only an interface that stays missing from the
    /// list for `interface_grace`, or comes back under another index, is
    /// `interface_gone`, and the engine stops; so is an interface that has
    /// link and other IPv4 addresses (a 169.254 one included) but not the
    /// server IP for [`FOREIGN_IP_CHECKS`] checks in a row:
    /// `server_ip_not_on_interface` (`interface`, `ip`, `addresses`).
    fn check_link(&mut self, now: Instant, io: &mut Option<Io>) {
        let Some(run) = self.run.as_mut() else { return };
        let (Some(at), Some(every)) = (run.next_link_check, run.cfg.link_check) else {
            return;
        };
        if now < at {
            return;
        }
        run.next_link_check = now.checked_add(every);
        let ifaces = match run.cfg.interfaces.list() {
            Ok(list) => list,
            Err(e) => {
                debug!("interface list failed: {e}");
                return;
            }
        };
        let name = run.cfg.interface.clone();
        let server_ip = run.net.server_ip;
        let status = link_status(&ifaces, &name, server_ip);
        let found = ifaces.iter().find(|i| i.name == name);
        if let Some(i) = found {
            run.local_ips = i.ipv4.iter().map(|(a, _)| *a).collect();
        }
        let index = found.map(|i| i.index);
        // Link, addresses, but not ours: a lost carrier looks different
        // (no address at all), so this one will not heal by itself.
        let foreign: Vec<String> = found
            .filter(|i| i.is_up && i.is_running && status == LinkStatus::Down("no_ip"))
            .map(|i| {
                i.ipv4
                    .iter()
                    .map(|(a, m)| format!("{a}/{}", u32::from(*m).leading_ones()))
                    .collect()
            })
            .unwrap_or_default();
        run.foreign_ip_checks = if foreign.is_empty() {
            0
        } else {
            run.foreign_ip_checks + 1
        };
        let foreign_fatal = (run.foreign_ip_checks >= FOREIGN_IP_CHECKS).then(|| {
            Failure::new("server_ip_not_on_interface")
                .with("interface", &name)
                .with("ip", server_ip)
                .with("addresses", foreign.join(","))
        });
        let prev = run.link;
        let index_changed =
            index.is_some_and(|i| run.link_index != 0 && i != 0 && i != run.link_index);
        let gone = |reason: &str| {
            Failure::new("interface_gone")
                .with("interface", &name)
                .with("ip", server_ip)
                .with("reason", reason)
        };
        let (link, fatal) = match status {
            LinkStatus::Missing => {
                let since = match prev {
                    Link::Missing(since) => since,
                    _ => now,
                };
                let over = now.saturating_duration_since(since) >= run.cfg.interface_grace;
                (Link::Missing(since), over.then(|| gone("missing")))
            }
            _ if index_changed => (prev, Some(gone("index_changed"))),
            LinkStatus::Down(reason) => (Link::Down(reason), foreign_fatal),
            LinkStatus::Up => (Link::Up, None),
        };
        run.link = link;
        match (prev, link) {
            (Link::Up, Link::Down(reason)) => self.link_down(reason),
            (Link::Up, Link::Missing(_)) => self.link_down("interface_missing"),
            (Link::Down(_) | Link::Missing(_), Link::Up) => {
                info!(interface = %name, "link up");
                self.warning(Failure::new("link_up").with("interface", &name));
            }
            _ => {}
        }
        if let Some(f) = fatal {
            warn!("{f}");
            self.emit(EngineEvent::Error(f));
            self.stop(io);
        }
    }
}

/// The key of a hello arch in `Running::arch_map`.
fn arch_key(hello_arch: &str) -> String {
    hello_arch.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InterfaceSource, Ports};
    use ni_assets::HttpConfig;
    use std::net::Ipv4Addr;

    const DEV: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x42]);

    /// A core started on `lo0` (with an extra address `alias`), its I/O and
    /// its Netinstall port.
    fn started_core(alias: Ipv4Addr) -> (Core, Option<Io>, ni_assets::testutil::TempDir) {
        let dir = ni_assets::testutil::tempdir("core-own-frames");
        // Nothing listens on port 9: kernel downloads fail at once.
        let http = HttpConfig {
            download_base: "http://127.0.0.1:9/dl/".into(),
            upgrade_base: "http://127.0.0.1:9/up".into(),
            ..HttpConfig::default()
        };
        let assets = Arc::new(Assets::new(dir.join("cache"), http).unwrap());
        let (events, _) = broadcast::channel(64);
        let (internal, _) = mpsc::channel(64);
        let mut core = Core {
            assets,
            events,
            internal,
            epoch: 0,
            run: None,
            aborted: Vec::new(),
        };
        let mut lo0 = ni_net::list_interfaces()
            .unwrap()
            .into_iter()
            .find(|i| i.name == "lo0")
            .expect("lo0");
        lo0.ipv4.push((alias, Ipv4Addr::new(255, 0, 0, 0)));
        let mut cfg = EngineConfig::new("lo0");
        cfg.ports = Ports {
            bootp: 0,
            tftp: 0,
            netinstall: 0,
            ..Ports::default()
        };
        cfg.link_check = None;
        cfg.interfaces = InterfaceSource::new(move || Ok(vec![lo0.clone()]));
        let mut io = None;
        core.start(cfg, &mut io).unwrap();
        (core, io, dir)
    }

    fn hello() -> Vec<u8> {
        Frame::new(
            DEV,
            Mac::BROADCAST,
            1,
            0,
            &b"DSCV\n\n\nhEX\nmmips\n7.1\n0\n0\n"[..],
        )
        .encode()
    }

    #[tokio::test]
    async fn own_frames_from_any_interface_address_are_dropped() {
        let alias = Ipv4Addr::new(10, 9, 9, 9);
        let (mut core, mut io, _dir) = started_core(alias);
        let port = io.as_ref().unwrap().socket.local_addr().unwrap().port();
        let server_ip = core.run.as_ref().unwrap().net.server_ip;
        let frame = hello();
        let feed = |core: &mut Core, io: &mut Option<Io>, from: SocketAddr| {
            core.on_datagram(Ok((frame.len(), from)), &frame, io);
            core.run.as_ref().unwrap().devices.contains_key(&DEV)
        };
        // Looped back from our port, from the server IP or another address
        // of the interface: ours, dropped.
        assert!(!feed(
            &mut core,
            &mut io,
            SocketAddr::from((server_ip, port))
        ));
        assert!(!feed(&mut core, &mut io, SocketAddr::from((alias, port))));
        // The same frame from a device (any other port) is a hello.
        assert!(feed(
            &mut core,
            &mut io,
            SocketAddr::from((alias, port.wrapping_add(1)))
        ));
        core.stop(&mut io);
    }

    /// The events sent so far.
    fn drain(rx: &mut broadcast::Receiver<EngineEvent>) -> Vec<EngineEvent> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn warnings(events: &[EngineEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Warning(f) => Some(f.code.clone()),
                _ => None,
            })
            .collect()
    }

    fn unreachable() -> BootpEvent {
        BootpEvent::SendFailed {
            mac: DEV,
            error: "No route to host (os error 65)".into(),
            host_unreachable: true,
        }
    }

    fn replied() -> BootpEvent {
        BootpEvent::Replied {
            mac: DEV,
            arch: ni_bootp::BootArch::MMips,
            ip: Ipv4Addr::new(10, 9, 9, 20),
            kind: ni_bootp::ReplyKind::Offer,
            file: "x".into(),
        }
    }

    #[tokio::test]
    async fn host_unreachable_sends_are_one_local_network_warning() {
        let (mut core, mut io, _dir) = started_core(Ipv4Addr::new(10, 9, 9, 8));
        let mut rx = core.events.subscribe();
        for _ in 0..50 {
            core.on_bootp(unreachable());
        }
        let events = drain(&mut rx);
        assert_eq!(warnings(&events), ["local_network_denied"]);
        let EngineEvent::Warning(w) = events
            .iter()
            .find(|e| matches!(e, EngineEvent::Warning(_)))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(w.param("service"), Some("bootp"));
        // The device's row says it too (once).
        let failed: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Device {
                    mac,
                    state: DeviceState::Failed { code, .. },
                } if *mac == DEV => Some(code.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(failed, ["local_network_denied"]);
        assert!(core.snapshot().local_network_denied);

        // A reply that went out: allowed again (said once), and a new
        // refusal is reported again.
        core.on_bootp(replied());
        core.on_bootp(replied());
        assert_eq!(warnings(&drain(&mut rx)), ["local_network_allowed"]);
        assert!(!core.snapshot().local_network_denied);
        core.on_bootp(unreachable());
        core.on_bootp(unreachable());
        assert_eq!(warnings(&drain(&mut rx)), ["local_network_denied"]);

        // Other send errors keep their own code.
        core.on_bootp(BootpEvent::SendFailed {
            mac: DEV,
            error: "Network is down".into(),
            host_unreachable: false,
        });
        assert_eq!(warnings(&drain(&mut rx)), ["bootp_send_failed"]);

        // A new start reports it again.
        let cfg = core.run.as_ref().unwrap().cfg.clone();
        core.stop(&mut io);
        core.reap().await;
        drain(&mut rx);
        core.start(cfg, &mut io).unwrap();
        core.on_bootp(unreachable());
        assert!(warnings(&drain(&mut rx)).contains(&"local_network_denied".to_string()));
        core.stop(&mut io);
    }

    #[tokio::test]
    async fn a_tftp_transfer_refused_as_unreachable_is_local_network_denied() {
        let (mut core, mut io, _dir) = started_core(Ipv4Addr::new(10, 9, 9, 7));
        let mut rx = core.events.subscribe();
        core.on_tftp(TftpEvent::Failed {
            peer: SocketAddr::from((Ipv4Addr::new(10, 9, 9, 20), 1234)),
            file: "x".into(),
            error: ni_tftp::TransferError::HostUnreachable("No route to host".into()),
        });
        assert_eq!(warnings(&drain(&mut rx)), ["local_network_denied"]);
        assert!(core.snapshot().local_network_denied);
        core.stop(&mut io);
    }
}
