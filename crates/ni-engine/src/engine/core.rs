//! The engine loop and its state machine.

use super::device::Device;
use super::start::start;
use super::{Command, Snapshot};
use crate::config::{interface_usable, EngineConfig, NetPlan};
use crate::event::{DeviceInfoView, DeviceState, EngineEvent, Failure, StartInfo, WaitReason};
use crate::gate::{Gate, Verdict};
use crate::prepare::{boot_maps, prepare};
use crate::profile::Profile;
use ni_assets::{Assets, KernelSet, Version};
use ni_bootp::{BootpEvent, BootpHandle, IgnoreReason};
use ni_net::{list_interfaces, RecvErrorAction, RecvErrorGuard};
use ni_proto::{DeviceInfo, FailReason, Frame, InstallPlan, Mac, ServerSession};
use ni_tftp::{FileMap, TftpEvent};
use std::collections::{BTreeMap, HashMap};
use std::future::pending;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Minimum time between two throttled progress states of one device.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// Minimum time between two `ignored` events per device and reason.
const IGNORED_INTERVAL: Duration = Duration::from_secs(10);

/// Messages from tasks the engine spawned.
pub(crate) enum Internal {
    Kernels {
        epoch: u64,
        result: Result<(Version, KernelSet, bool), Failure>,
    },
    Prepared {
        epoch: u64,
        mac: Mac,
        attempt: u64,
        result: Result<InstallPlan, Failure>,
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
    pub bootp_rx: mpsc::Receiver<BootpEvent>,
    pub tftp_rx: mpsc::Receiver<TftpEvent>,
    pub recv_errors: RecvErrorGuard,
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
    pub queue_seq: u64,
    pub ignored: HashMap<(Mac, &'static str), Instant>,
    pub next_link_check: Option<Instant>,
}

struct Core {
    assets: Arc<Assets>,
    events: broadcast::Sender<EngineEvent>,
    internal: mpsc::Sender<Internal>,
    epoch: u64,
    run: Option<Running>,
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
) {
    let (internal, mut internal_rx) = mpsc::channel(64);
    let mut core = Core {
        assets,
        events,
        internal,
        epoch: 0,
        run: None,
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
                Some(cmd) => core.command(cmd, &mut io),
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

    fn command(&mut self, cmd: Command, io: &mut Option<Io>) {
        match cmd {
            Command::Start(cfg, reply) => {
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
                let _ = reply.send(r);
            }
            Command::Flash(mac, profile, reply) => {
                let _ = reply.send(self.flash(mac, *profile));
            }
            Command::Retry(mac, reply) => {
                let _ = reply.send(self.retry(mac));
            }
            Command::AutoRepeat(on, reply) => {
                let _ = reply.send(self.set_auto_repeat(on));
            }
            Command::Snapshot(reply) => {
                let _ = reply.send(self.snapshot());
            }
        }
    }

    fn start(&mut self, cfg: EngineConfig, io: &mut Option<Io>) -> Result<StartInfo, Failure> {
        if self.run.is_some() {
            return Err(Failure::new("already_running"));
        }
        let epoch = self.epoch + 1;
        let (running, new_io) = start(cfg, self.assets.clone(), self.internal.clone(), epoch)?;
        self.epoch = epoch;
        let info = running.info.clone();
        info!(interface = %info.interface, server_ip = %info.server_ip, "engine started");
        self.run = Some(running);
        *io = Some(new_io);
        self.emit(EngineEvent::Started(info.clone()));
        Ok(info)
    }

    fn stop(&mut self, io: &mut Option<Io>) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        *io = None;
        for task in run.tasks.drain(..) {
            task.abort();
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

    fn set_auto_repeat(&mut self, on: bool) -> Result<(), Failure> {
        let run = self.running()?;
        if on && run.cfg.profile.is_none() {
            return Err(Failure::new("no_profile"));
        }
        run.cfg.auto_repeat = on;
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

    fn snapshot(&self) -> Snapshot {
        match &self.run {
            None => Snapshot {
                running: false,
                started: None,
                network: None,
                kernel_version: None,
                auto_repeat: false,
                profile: None,
                devices: Vec::new(),
            },
            Some(run) => Snapshot {
                running: true,
                started: Some(run.info.clone()),
                network: Some(run.net.clone()),
                kernel_version: run.kernel_version.as_ref().map(|v| v.to_string()),
                auto_repeat: run.cfg.auto_repeat,
                profile: run.cfg.profile.as_ref().map(|p| p.name.clone()),
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

    fn enqueue(&mut self, mac: Mac, profile: Profile) {
        let Some(run) = self.run.as_mut() else { return };
        let seq = run.queue_seq;
        run.queue_seq += 1;
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
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
            let next = run
                .devices
                .values()
                .filter(|d| d.is("waiting"))
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
        let active_spec = run.cfg.profile.as_ref().map(|p| p.version.clone());
        let kernel_version = run.kernel_version.clone();
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
        };
        let (Some(profile), Some(info)) = (dev.using.clone(), dev.info.clone()) else {
            return;
        };
        dev.queued = None;
        dev.attempt += 1;
        let attempt = dev.attempt;
        let version = kernel_version.filter(|_| active_spec.as_ref() == Some(&profile.version));
        run.tasks.retain(|t| !t.is_finished());
        run.tasks.push(tokio::spawn(async move {
            let result = prepare(&assets, &profile, version, &info.arch).await;
            let _ = internal
                .send(Internal::Prepared {
                    epoch,
                    mac,
                    attempt,
                    result,
                })
                .await;
        }));
        self.set_state(mac, DeviceState::Preparing);
    }

    fn prepared(&mut self, mac: Mac, attempt: u64, result: Result<InstallPlan, Failure>) {
        let Some(run) = self.run.as_mut() else { return };
        let session_cfg = run.cfg.session.clone();
        let server_mac = run.net.server_mac;
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
        };
        if dev.attempt != attempt || !dev.is("preparing") {
            return;
        }
        let state = match result {
            Err(f) => DeviceState::failed(f),
            Ok(plan) => {
                let info = dev.info.clone().expect("prepared devices have said hello");
                match ServerSession::new(server_mac, info, &plan, session_cfg, Instant::now()) {
                    Ok(session) => {
                        dev.session = Some(session);
                        dev.last_progress = None;
                        DeviceState::Flashing {
                            phase: "offering".into(),
                            sent: 0,
                            total: plan.total_bytes(),
                        }
                    }
                    Err(e) => DeviceState::failed(Failure::new("invalid_plan").with("detail", e)),
                }
            }
        };
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
        let dev = run.devices.entry(mac).or_insert_with(|| Device::new(mac));
        if dev.is("done") {
            // The cooldown is over: a new round for this device.
            *dev = Device::new(mac);
        }
        if dev.info.is_some() {
            return; // already discovered (waiting, flashing, failed, ...)
        }
        dev.info = Some(info.clone());
        info!(%mac, model = %info.model, arch = %info.arch, "device discovered");
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
        let n = match r {
            Ok((n, _)) => {
                if let Some(io) = io.as_mut() {
                    io.recv_errors.on_success();
                }
                n
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
        if frame.src == run.net.server_mac {
            return; // our own broadcast
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
            BootpEvent::Replied { mac, arch, ip, .. } => {
                let dev = run.devices.entry(mac).or_insert_with(|| Device::new(mac));
                if dev.is_active() {
                    debug!(%mac, "BOOTP from a device being flashed");
                    return;
                }
                if dev.is("done") {
                    *dev = Device::new(mac);
                }
                dev.boot_arch = Some(arch);
                dev.ip = Some(ip);
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
            BootpEvent::SendFailed { mac, error } => self.warning(
                Failure::new("bootp_send_failed")
                    .with("mac", mac)
                    .with("detail", error),
            ),
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
            TftpEvent::Started { peer, size, .. } => {
                if let Some(mac) = self.tftp_device(peer) {
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
                if let Some(mac) = self.tftp_device(peer) {
                    let state = DeviceState::KernelServing {
                        sent: acked,
                        total: size,
                    };
                    self.set_state(mac, state);
                }
            }
            TftpEvent::Completed { peer, size, .. } => {
                if let Some(mac) = self.tftp_device(peer) {
                    let state = DeviceState::KernelServing {
                        sent: size,
                        total: size,
                    };
                    self.set_state(mac, state);
                }
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
            Internal::Kernels { epoch, result } if epoch == self.epoch => {
                let Some(run) = self.run.as_mut() else { return };
                match result {
                    Ok((version, set, fallback)) => {
                        let (boot, files) = boot_maps(&set);
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
                        self.emit(EngineEvent::KernelsReady {
                            version: version.to_string(),
                        });
                    }
                    Err(f) => {
                        warn!("boot kernels unavailable: {f}");
                        self.emit(EngineEvent::Error(f));
                    }
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
        let dest = run.cfg.ports.netinstall_dest;
        if let Some(io) = io.as_ref() {
            for frame in &frames {
                if let Err(e) = io.socket.send_to(frame, dest).await {
                    debug!("netinstall send failed: {e}");
                }
            }
        }
        for (mac, ev) in events {
            self.session_event(mac, ev, now);
        }
        self.admit();
        self.check_link(now, io);
    }

    fn session_event(&mut self, mac: Mac, ev: ni_proto::Event, now: Instant) {
        let Some(run) = self.run.as_mut() else { return };
        let cooldown = run.cfg.cooldown;
        let Some(dev) = run.devices.get_mut(&mac) else {
            return;
        };
        let (phase, sent, total) = match &dev.state {
            Some(DeviceState::Flashing { phase, sent, total }) => (phase.clone(), *sent, *total),
            _ => (String::new(), 0, 0),
        };
        match ev {
            ni_proto::Event::PhaseChanged(p) => {
                dev.last_progress = Some(now);
                let phase = p.name().to_string();
                self.set_state(mac, DeviceState::Flashing { phase, sent, total });
            }
            ni_proto::Event::Progress {
                bytes_sent,
                bytes_total,
                ..
            } => {
                let state = DeviceState::Flashing {
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
            ni_proto::Event::Done => {
                dev.session = None;
                run.gate.start_cooldown(mac, now + cooldown);
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
                    FailReason::Cancelled => Failure::new("cancelled"),
                };
                warn!(%mac, "flashing failed: {f}");
                self.set_state(mac, DeviceState::failed(f));
            }
        }
    }

    fn check_link(&mut self, now: Instant, io: &mut Option<Io>) {
        let Some(run) = self.run.as_mut() else { return };
        let (Some(at), Some(every)) = (run.next_link_check, run.cfg.link_check) else {
            return;
        };
        if now < at {
            return;
        }
        run.next_link_check = Some(now + every);
        let ifaces = list_interfaces().unwrap_or_default();
        if !interface_usable(&ifaces, &run.cfg.interface, run.net.server_ip) {
            let f = Failure::new("interface_gone")
                .with("interface", &run.cfg.interface)
                .with("ip", run.net.server_ip);
            warn!("{f}");
            self.emit(EngineEvent::Error(f));
            self.stop(io);
        }
    }
}
