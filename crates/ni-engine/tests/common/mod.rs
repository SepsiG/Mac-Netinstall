//! Test rig: an engine on `lo0` with unprivileged ports, a fake MikroTik
//! mirror over HTTP (synthetic kernels and packages) and simulated
//! RouterBOARDs ([`ni_sim::lan::SimLan`]).
#![allow(dead_code)] // each test file uses a different part

use ni_assets::testutil::{mirror, synthetic_kernels, tempdir, TempDir, TestServer};
use ni_assets::{Channel, HttpConfig, VersionSpec};
use ni_bootp::BootArch;
use ni_engine::{EngineConfig, EngineEvent, EngineHandle, Ports, Profile, StartInfo};
use ni_proto::{Mac, SessionConfig};
use ni_sim::lan::{tftp_get, SimLan};
use ni_sim::{DeviceSim, SimConfig};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

pub const SERVER_MAC: Mac = Mac([0x02, 0, 0, 0, 0, 0x01]);
pub const A: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x0a]);
pub const B: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x0b]);
pub const WAIT: Duration = Duration::from_secs(10);

pub struct Rig {
    pub engine: EngineHandle,
    pub events: broadcast::Receiver<EngineEvent>,
    pub lan: SimLan,
    pub started: StartInfo,
    /// Bytes of the mirror's `routeros` and `wireless` packages.
    pub main: Vec<u8>,
    pub wireless: Vec<u8>,
    pub srv: TestServer,
    pub cache: TempDir,
}

/// Fast session timings, so failures show up in well under a second.
pub fn fast_session() -> SessionConfig {
    SessionConfig {
        reply_timeout: Duration::from_millis(150),
        format_timeout: Duration::from_secs(2),
        optional_timeout: Duration::from_millis(150),
        max_retransmits: 2,
        chunk_delay: Duration::from_millis(1),
    }
}

/// The profile every rig starts with: stable (= 7.25.1) plus `wireless`.
pub fn profile() -> Profile {
    let mut p = Profile::new("test", VersionSpec::Channel(Channel::Stable));
    p.extra_packages = vec!["wireless".into()];
    p
}

/// Engine settings for `lo0`: ephemeral ports, replies to `lan`.
pub fn config(lan: &SimLan) -> EngineConfig {
    let mut cfg = EngineConfig::new("lo0");
    cfg.server_mac = Some(SERVER_MAC);
    cfg.ports = Ports {
        bootp: 0,
        tftp: 0,
        netinstall: 0,
        bootp_reply_to: lan.bootp_addr(),
        netinstall_dest: lan.netinstall_addr(),
        ..Ports::default()
    };
    cfg.profile = Some(profile());
    cfg.auto_repeat = true;
    cfg.session = fast_session();
    cfg
}

/// Start an engine (after `tweak`) and wait until its kernels are served.
pub async fn rig(tweak: impl FnOnce(&mut EngineConfig)) -> Rig {
    let srv = TestServer::start().await;
    let (_, main, wireless) = mirror(&srv);
    let cache = tempdir("engine");
    let http = HttpConfig {
        download_base: format!("{}/dl/", srv.base),
        upgrade_base: format!("{}/up", srv.base),
        ..HttpConfig::default()
    };
    let engine = EngineHandle::spawn(cache.join("cache"), http).unwrap();
    let mut events = engine.subscribe();
    let lan = SimLan::bind().await.unwrap();
    let mut cfg = config(&lan);
    tweak(&mut cfg);
    let started = engine.start(cfg).await.unwrap();
    lan.connect(started.netinstall).await;
    next(&mut events, |e| {
        matches!(e, EngineEvent::KernelsReady { .. })
    })
    .await;
    Rig {
        engine,
        events,
        lan,
        started,
        main,
        wireless,
        srv,
        cache,
    }
}

/// The next event matching `pred` (others are skipped); panics after
/// [`WAIT`].
pub async fn next(
    events: &mut broadcast::Receiver<EngineEvent>,
    pred: impl Fn(&EngineEvent) -> bool,
) -> EngineEvent {
    let end = tokio::time::Instant::now() + WAIT;
    loop {
        match tokio::time::timeout_at(end, events.recv()).await {
            Ok(Ok(e)) if pred(&e) => return e,
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("event stream: {e}"),
            Err(_) => panic!("no matching event within {WAIT:?}"),
        }
    }
}

/// Wait for `mac` to enter the state named `state`.
pub async fn state(
    events: &mut broadcast::Receiver<EngineEvent>,
    mac: Mac,
    state: &str,
) -> EngineEvent {
    next(
        events,
        |e| matches!(e, EngineEvent::Device { mac: m, state: s } if *m == mac && s.name() == state),
    )
    .await
}

/// Collect events until `mac` is done or failed; returns them all.
pub async fn until_finished(
    events: &mut broadcast::Receiver<EngineEvent>,
    mac: Mac,
) -> Vec<EngineEvent> {
    let mut seen = Vec::new();
    loop {
        let e = next(events, |_| true).await;
        let end = matches!(&e, EngineEvent::Device { mac: m, state } if *m == mac && matches!(state.name(), "done" | "failed"));
        seen.push(e);
        if end {
            return seen;
        }
    }
}

/// The state names `mac` went through, in order.
pub fn states(events: &[EngineEvent], mac: Mac) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Device { mac: m, state } if *m == mac => Some(state.name()),
            _ => None,
        })
        .collect()
}

pub fn sim_config(mac: Mac) -> SimConfig {
    let mut cfg = SimConfig::new(mac, "RB5009UG+S+", "arm64");
    cfg.hello_interval = Duration::from_millis(100);
    cfg.resend_interval = Duration::from_millis(100);
    cfg
}

/// Boot a simulated arm64 board like RouterBOOT does: BOOTP, then TFTP of
/// the boot file (checked against the synthetic kernel). Returns the
/// device simulator, ready for [`SimLan::add`].
pub async fn boot(rig: &Rig, mac: Mac, sim: SimConfig) -> DeviceSim {
    let reply = rig
        .lan
        .boot(mac, "ARM64__boot", rig.started.bootp, WAIT)
        .await
        .expect("BOOTP reply");
    assert_eq!(reply.file, "kernel-arm64.elf");
    assert_eq!(reply.server, rig.started.server_ip);
    let tftp = SocketAddr::from((reply.server, rig.started.tftp.port()));
    let kernel = tftp_get(tftp, &reply.file).await.unwrap();
    let expected = synthetic_kernels()
        .into_iter()
        .find(|(arch, _)| *arch == BootArch::Arm64)
        .unwrap()
        .1;
    assert_eq!(kernel, expected, "TFTP served the arm64 kernel");
    DeviceSim::new(sim, Instant::now())
}
