//! The interface watchdog with a fake interface list: a lost carrier (or
//! server IP) is a `link_down` warning and the engine keeps running; an
//! interface that disappears for longer than the grace period is
//! `interface_gone` and the engine stops.

mod common;

use common::*;
use ni_engine::{EngineEvent, Failure};
use std::net::Ipv4Addr;
use std::time::Duration;
use tokio::sync::broadcast;

const CHECK: Duration = Duration::from_millis(40);

fn warning(code: &str) -> impl Fn(&EngineEvent) -> bool + '_ {
    move |e| matches!(e, EngineEvent::Warning(f) if f.code == code)
}

/// All events within `d` (the stream must stay open).
async fn collect_for(
    events: &mut broadcast::Receiver<EngineEvent>,
    d: Duration,
) -> Vec<EngineEvent> {
    let end = tokio::time::Instant::now() + d;
    let mut out = Vec::new();
    while let Ok(Ok(e)) = tokio::time::timeout_at(end, events.recv()).await {
        out.push(e);
    }
    out
}

fn no_carrier(mut i: ni_net::Interface) -> ni_net::Interface {
    i.ipv4.clear();
    i.is_running = false;
    i
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_link_is_a_warning_and_the_engine_keeps_running() {
    let (source, iface) = fake_interfaces(Some(real_lo0()));
    let mut rig = rig(|cfg| {
        cfg.interfaces = source;
        cfg.link_check = Some(CHECK);
    })
    .await;
    // Cable pulled: macOS drops the manual IPv4 with the carrier.
    let up = iface.lock().unwrap().clone().unwrap();
    *iface.lock().unwrap() = Some(no_carrier(up.clone()));
    let e = next(&mut rig.events, warning("link_down")).await;
    assert_eq!(
        e,
        EngineEvent::Warning(
            Failure::new("link_down")
                .with("interface", "lo0")
                .with("reason", "no_ip")
        )
    );
    // Reported once, nothing stops.
    let later = collect_for(&mut rig.events, CHECK * 8).await;
    assert!(
        later.iter().all(
            |e| !matches!(e, EngineEvent::Warning(f) if f.code == "link_down")
                && !matches!(e, EngineEvent::Error(_) | EngineEvent::Stopped)
        ),
        "{later:?}"
    );
    assert!(rig.engine.snapshot().await.unwrap().running);
    // Plugged in again.
    *iface.lock().unwrap() = Some(up);
    let e = next(&mut rig.events, warning("link_up")).await;
    assert_eq!(
        e,
        EngineEvent::Warning(Failure::new("link_up").with("interface", "lo0"))
    );
    // And devices are still served.
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interface_that_disappears_stops_the_engine_after_the_grace_period() {
    let (source, iface) = fake_interfaces(Some(real_lo0()));
    let mut rig = rig(|cfg| {
        cfg.interfaces = source;
        cfg.link_check = Some(CHECK);
        cfg.interface_grace = Duration::from_millis(400);
    })
    .await;
    // Unplugged and back within the grace period: just a flap.
    let lo0 = iface.lock().unwrap().take().unwrap();
    let e = next(&mut rig.events, warning("link_down")).await;
    assert_eq!(
        e,
        EngineEvent::Warning(
            Failure::new("link_down")
                .with("interface", "lo0")
                .with("reason", "interface_missing")
        )
    );
    *iface.lock().unwrap() = Some(lo0);
    next(&mut rig.events, warning("link_up")).await;
    // Unplugged for good.
    let start = std::time::Instant::now();
    iface.lock().unwrap().take();
    next(&mut rig.events, warning("link_down")).await;
    let e = next(&mut rig.events, |e| matches!(e, EngineEvent::Error(_))).await;
    assert!(start.elapsed() >= Duration::from_millis(400));
    let EngineEvent::Error(f) = e else {
        unreachable!()
    };
    assert_eq!(f.code, "interface_gone");
    assert_eq!(f.param("interface"), Some("lo0"));
    next(&mut rig.events, |e| *e == EngineEvent::Stopped).await;
    assert!(!rig.engine.snapshot().await.unwrap().running);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_without_link_uses_the_configured_server_ip() {
    let (source, iface) = fake_interfaces(Some(no_carrier(real_lo0())));
    let srv = ni_assets::testutil::TestServer::start().await;
    ni_assets::testutil::mirror(&srv);
    let cache = ni_assets::testutil::tempdir("engine-nolink");
    let engine = ni_engine::EngineHandle::spawn(cache.join("cache"), http(&srv)).unwrap();
    let mut events = engine.subscribe();
    let lan = ni_sim::lan::SimLan::bind().await.unwrap();
    let mut cfg = config(&lan);
    cfg.interfaces = source;
    cfg.link_check = Some(CHECK);
    // No server IP configured and none on the interface: nothing to serve.
    assert_eq!(engine.start(cfg.clone()).await.unwrap_err().code, "no_ipv4");
    cfg.server_ip = Some(Ipv4Addr::LOCALHOST);
    cfg.netmask = Some(Ipv4Addr::new(255, 0, 0, 0));
    let started = engine.start(cfg).await.unwrap();
    assert_eq!(started.server_ip, Ipv4Addr::LOCALHOST);
    let snap = engine.snapshot().await.unwrap();
    let net = snap.network.unwrap();
    assert_eq!(net.netmask, Ipv4Addr::new(255, 0, 0, 0));
    let e = next(&mut events, warning("link_down")).await;
    assert_eq!(
        e,
        EngineEvent::Warning(
            Failure::new("link_down")
                .with("interface", "lo0")
                .with("reason", "no_ip")
        )
    );
    // The link comes up: the same sockets serve devices.
    *iface.lock().unwrap() = Some(real_lo0());
    next(&mut events, warning("link_up")).await;
    lan.connect(started.netinstall).await;
    let reply = lan
        .boot(A, "ARM64__boot", started.bootp, WAIT)
        .await
        .expect("BOOTP reply");
    assert_eq!(reply.server, Ipv4Addr::LOCALHOST);
    engine.stop().await.unwrap();
}
