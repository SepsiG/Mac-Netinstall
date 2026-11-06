//! Several devices, the concurrency limit, cooldown, failures and retry,
//! and start/stop errors.

mod common;

use common::*;
use ni_engine::{DeviceState, EngineEvent, EngineHandle, WaitReason};
use ni_net::{bind_udp, BindOptions};
use std::net::Ipv4Addr;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_limit_one_the_second_device_waits_for_the_first() {
    let mut rig = rig(|cfg| cfg.max_parallel = 1).await;
    // A takes a while to "format", so B is discovered while A is busy.
    let mut slow = sim_config(A);
    slow.format_delay = Duration::from_millis(400);
    let a = boot(&rig, A, slow).await;
    let b = boot(&rig, B, sim_config(B)).await;
    rig.lan.add(a).await;
    state(&mut rig.events, A, "flashing").await;
    rig.lan.add(b).await;
    let mut events = Vec::new();
    let mut finished = 0;
    while finished < 2 {
        let e = next(&mut rig.events, |_| true).await;
        if matches!(&e, EngineEvent::Device { state, .. } if state.name() == "done") {
            finished += 1;
        }
        events.push(e);
    }
    let pos = |mac, name: &str| {
        events
            .iter()
            .position(|e| matches!(e, EngineEvent::Device { mac: m, state } if *m == mac && state.name() == name))
            .unwrap_or_else(|| panic!("{mac} never was {name}"))
    };
    assert!(events.contains(&EngineEvent::Device {
        mac: B,
        state: DeviceState::Waiting {
            reason: WaitReason::Concurrency
        }
    }));
    assert!(
        pos(A, "done") < pos(B, "preparing"),
        "B starts after A is done"
    );
    for mac in [A, B] {
        let report = rig.lan.wait_terminated(mac, WAIT).await.unwrap();
        assert_eq!(report.received.len(), 2, "{mac}");
        assert!(report.violations.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flashed_device_is_ignored_during_the_cooldown() {
    let mut rig = rig(|_| {}).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
    rig.lan.wait_terminated(A, WAIT).await.unwrap();

    // It comes back (netboots again): no boot file for it...
    let reply = rig
        .lan
        .boot(
            A,
            "ARM64__boot",
            rig.started.bootp,
            Duration::from_millis(600),
        )
        .await;
    assert_eq!(reply, None);
    let e = next(&mut rig.events, |e| {
        matches!(e, EngineEvent::Ignored { .. })
    })
    .await;
    assert_eq!(
        e,
        EngineEvent::Ignored {
            mac: A,
            reason: "cooldown".into()
        }
    );
    // ...and its hellos are not answered.
    rig.lan.reset(A).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let report = rig.lan.report(A).await.unwrap();
    assert_eq!(report.frames_to_device, 0);
    assert!(!report.terminated);
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!(snap.devices[0].state, DeviceState::Done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_the_cooldown_a_device_is_flashed_again() {
    let mut rig = rig(|cfg| cfg.cooldown = Duration::from_millis(300)).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    rig.lan.reset(A).await;
    state(&mut rig.events, A, "discovered").await;
    state(&mut rig.events, A, "done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_mac_ignores_other_devices() {
    let mut rig = rig(|cfg| cfg.only_mac = Some(A)).await;
    let reply = rig
        .lan
        .boot(
            B,
            "ARM64__boot",
            rig.started.bootp,
            Duration::from_millis(600),
        )
        .await;
    assert_eq!(reply, None);
    let e = next(&mut rig.events, |e| {
        matches!(e, EngineEvent::Ignored { .. })
    })
    .await;
    assert_eq!(
        e,
        EngineEvent::Ignored {
            mac: B,
            reason: "not_selected".into()
        }
    );
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_device_can_be_retried() {
    let mut rig = rig(|_| {}).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.set_mute(A, true).await;
    rig.lan.add(sim).await;
    let e = state(&mut rig.events, A, "failed").await;
    let EngineEvent::Device {
        state: DeviceState::Failed { code, params },
        ..
    } = e
    else {
        unreachable!()
    };
    assert_eq!(code, "flash_timeout");
    assert_eq!(params["phase"], "offering");

    // Still failed while it keeps saying hello.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(rig.engine.snapshot().await.unwrap().devices[0].state.name() == "failed");

    rig.lan.set_mute(A, false).await;
    rig.lan.reset(A).await;
    rig.engine.retry(A).await.unwrap();
    state(&mut rig.events, A, "done").await;
    let report = rig.lan.wait_terminated(A, WAIT).await.unwrap();
    assert_eq!(report.received.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_reports_a_busy_port_and_can_restart() {
    let held = bind_udp(&BindOptions::new(0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let mut rig = rig(|_| {}).await;
    assert_eq!(
        rig.engine.start(config(&rig.lan)).await.unwrap_err().code,
        "already_running"
    );
    rig.engine.stop().await.unwrap();
    next(&mut rig.events, |e| *e == EngineEvent::Stopped).await;
    assert_eq!(rig.engine.stop().await.unwrap_err().code, "not_running");
    assert_eq!(rig.engine.retry(A).await.unwrap_err().code, "not_running");
    assert!(!rig.engine.snapshot().await.unwrap().running);

    let mut cfg = config(&rig.lan);
    cfg.ports.tftp = port;
    let err = rig.engine.start(cfg).await.unwrap_err();
    assert_eq!(err.code, "port_in_use");
    assert_eq!(err.param("service"), Some("tftp"));
    drop(held);

    // Nothing was left bound by the failed start: a normal start works.
    let started = rig.engine.start(config(&rig.lan)).await.unwrap();
    assert_eq!(started.server_ip, Ipv4Addr::LOCALHOST);
    next(&mut rig.events, |e| {
        matches!(e, EngineEvent::KernelsReady { .. })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_cancels_a_running_install() {
    let mut rig = rig(|_| {}).await;
    let mut slow = sim_config(A);
    slow.format_delay = Duration::from_secs(5);
    let sim = boot(&rig, A, slow).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "flashing").await;
    rig.engine.stop().await.unwrap();
    let e = state(&mut rig.events, A, "failed").await;
    assert!(
        matches!(e, EngineEvent::Device { state: DeviceState::Failed { code, .. }, .. } if code == "cancelled")
    );
    next(&mut rig.events, |e| *e == EngineEvent::Stopped).await;
}

#[tokio::test]
async fn unknown_interface_is_reported_by_start() {
    let dir = ni_assets::testutil::tempdir("engine-errors");
    let engine = EngineHandle::spawn(dir.join("cache"), Default::default()).unwrap();
    let err = engine
        .start(ni_engine::EngineConfig::new("nosuch0"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "unknown_interface");
    assert_eq!(err.param("interface"), Some("nosuch0"));
}
