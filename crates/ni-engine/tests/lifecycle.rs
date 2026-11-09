//! Several devices, the concurrency limit, cooldown, failures and retry,
//! and start/stop errors.

mod common;

use common::*;
use ni_engine::{DeviceState, EngineEvent, EngineHandle, WaitReason};
use ni_net::{bind_udp, BindOptions};
use ni_sim::AfterFiles;
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
async fn after_the_cooldown_a_device_is_flashed_again_once_it_netboots() {
    let mut rig = rig(|cfg| {
        cfg.min_cooldown = Duration::ZERO;
        cfg.cooldown = Duration::from_millis(300);
    })
    .await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    // Hellos alone (no new BOOTP) are stale: ignored, not flashed.
    rig.lan.reset(A).await;
    let e = next(&mut rig.events, |e| {
        matches!(e, EngineEvent::Ignored { .. })
    })
    .await;
    assert_eq!(
        e,
        EngineEvent::Ignored {
            mac: A,
            reason: "awaiting_bootp".into()
        }
    );
    assert_eq!(
        rig.engine.snapshot().await.unwrap().devices[0].state,
        DeviceState::Done
    );
    // It netboots again: a new round.
    boot(&rig, A, sim_config(A)).await;
    rig.lan.reset(A).await;
    state(&mut rig.events, A, "discovered").await;
    state(&mut rig.events, A, "done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cooldown_below_the_minimum_is_rejected() {
    let rig = rig(|_| {}).await;
    rig.engine.stop().await.unwrap();
    let mut cfg = config(&rig.lan);
    cfg.cooldown = Duration::from_secs(29);
    let err = rig.engine.start(cfg).await.unwrap_err();
    assert_eq!(err.code, "invalid_cooldown");
    assert_eq!(err.param("min_secs"), Some("30"));
    let mut cfg = config(&rig.lan);
    cfg.cooldown = Duration::from_secs(30);
    rig.engine.start(cfg).await.unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_install_is_flash_rejected() {
    let mut rig = rig(|_| {}).await;
    let mut refusing = sim_config(A);
    refusing.reject_offer = true;
    let sim = boot(&rig, A, refusing).await;
    rig.lan.add(sim).await;
    let e = state(&mut rig.events, A, "failed").await;
    let EngineEvent::Device {
        state: DeviceState::Failed { code, params },
        ..
    } = e
    else {
        unreachable!()
    };
    assert_eq!(code, "flash_rejected");
    assert_eq!(params["phase"], "offering");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_that_stops_before_the_last_file_is_flash_ended_early() {
    let mut rig = rig(|_| {}).await;
    let mut early = sim_config(A);
    early.after_files = AfterFiles::EarlyWtrm;
    let sim = boot(&rig, A, early).await;
    rig.lan.add(sim).await;
    let e = state(&mut rig.events, A, "failed").await;
    let EngineEvent::Device {
        state: DeviceState::Failed { code, params },
        ..
    } = e
    else {
        unreachable!()
    };
    assert_eq!(code, "flash_ended_early");
    assert_eq!(params["remaining"], "1", "wireless was not sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_that_never_says_wtrm_is_done_with_a_warning() {
    let mut rig = rig(|_| {}).await;
    let mut never = sim_config(A);
    never.after_files = AfterFiles::NeverWtrm;
    let sim = boot(&rig, A, never).await;
    rig.lan.add(sim).await;
    let w = next(
        &mut rig.events,
        |e| matches!(e, EngineEvent::Warning(f) if f.code == "finish_unconfirmed"),
    )
    .await;
    let EngineEvent::Warning(f) = w else {
        unreachable!()
    };
    assert_eq!(f.param("mac"), Some("48:A9:8A:00:00:0A"));
    state(&mut rig.events, A, "done").await;
    let report = rig.lan.wait_terminated(A, WAIT).await;
    assert!(report.is_some(), "TERM reached the device");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_min_os_is_only_a_warning() {
    let mut rig = rig(|_| {}).await;
    let mut newer = sim_config(A);
    newer.info.min_os = "7.99".into();
    let sim = boot(&rig, A, newer).await;
    rig.lan.add(sim).await;
    let w = next(
        &mut rig.events,
        |e| matches!(e, EngineEvent::Warning(f) if f.code == "min_os_newer"),
    )
    .await;
    let EngineEvent::Warning(f) = w else {
        unreachable!()
    };
    assert_eq!(f.param("min_os"), Some("7.99"));
    assert_eq!(f.param("version"), Some("7.25.1"));
    assert_eq!(f.param("mac"), Some("48:A9:8A:00:00:0A"));
    state(&mut rig.events, A, "done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_device_that_netboots_again_is_reflashed() {
    let mut rig = rig(|_| {}).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.set_mute(A, true).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "failed").await;
    // Power-cycled into Etherboot: BOOTP resets it, auto-repeat flashes it.
    rig.lan.set_mute(A, false).await;
    boot(&rig, A, sim_config(A)).await;
    state(&mut rig.events, A, "bootp_seen").await;
    rig.lan.reset(A).await;
    state(&mut rig.events, A, "done").await;
    let report = rig.lan.wait_terminated(A, WAIT).await.unwrap();
    assert_eq!(report.received.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_limit_one_a_failing_first_device_frees_the_slot() {
    let mut rig = rig(|cfg| cfg.max_parallel = 1).await;
    let a = boot(&rig, A, sim_config(A)).await;
    let b = boot(&rig, B, sim_config(B)).await;
    rig.lan.set_mute(A, true).await;
    rig.lan.add(a).await;
    state(&mut rig.events, A, "flashing").await;
    rig.lan.add(b).await;
    let mut seen = Vec::new();
    loop {
        let e = next(&mut rig.events, |_| true).await;
        let end =
            matches!(&e, EngineEvent::Device { mac, state } if *mac == B && state.name() == "done");
        seen.push(e);
        if end {
            break;
        }
    }
    assert!(seen.contains(&EngineEvent::Device {
        mac: B,
        state: DeviceState::Waiting {
            reason: WaitReason::Concurrency
        }
    }));
    let a_failed = states(&seen, A).iter().position(|s| *s == "failed");
    let b_prep = states(&seen, B).iter().position(|s| *s == "preparing");
    assert!(a_failed.is_some() && b_prep.is_some(), "{seen:?}");
    let pos = |mac, name: &str| {
        seen.iter()
            .position(|e| matches!(e, EngineEvent::Device { mac: m, state } if *m == mac && state.name() == name))
            .unwrap()
    };
    assert!(pos(A, "failed") < pos(B, "preparing"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_positions_and_auto_repeat_off() {
    let mut rig = rig(|cfg| cfg.max_parallel = 1).await;
    let mut slow = sim_config(A);
    slow.format_delay = Duration::from_millis(1500);
    let a = boot(&rig, A, slow).await;
    let b = boot(&rig, B, sim_config(B)).await;
    let c = boot(&rig, C, sim_config(C)).await;
    rig.lan.add(a).await;
    state(&mut rig.events, A, "flashing").await;
    rig.lan.add(b).await;
    state(&mut rig.events, B, "waiting").await;
    rig.lan.add(c).await;
    state(&mut rig.events, C, "waiting").await;
    // B is first in line; an explicit flash keeps its place.
    let mut p = profile();
    p.name = "explicit".into();
    rig.engine.flash(B, p).await.unwrap();
    // Without auto-repeat, C (waiting only because of it) leaves the queue.
    rig.engine.set_auto_repeat(false).await.unwrap();
    let e = state(&mut rig.events, C, "waiting").await;
    assert_eq!(
        e,
        EngineEvent::Device {
            mac: C,
            state: DeviceState::Waiting {
                reason: WaitReason::NoProfile
            }
        }
    );
    state(&mut rig.events, A, "done").await;
    state(&mut rig.events, B, "done").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snap = rig.engine.snapshot().await.unwrap();
    let c_view = snap.devices.iter().find(|d| d.mac == C).unwrap();
    assert_eq!(
        c_view.state,
        DeviceState::Waiting {
            reason: WaitReason::NoProfile
        }
    );
    let b_view = snap.devices.iter().find(|d| d.mac == B).unwrap();
    assert_eq!(b_view.profile.as_deref(), Some("explicit"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_flash_keeps_the_queue_position() {
    let mut rig = rig(|cfg| cfg.max_parallel = 1).await;
    let mut slow = sim_config(A);
    slow.format_delay = Duration::from_millis(800);
    let a = boot(&rig, A, slow).await;
    let b = boot(&rig, B, sim_config(B)).await;
    let c = boot(&rig, C, sim_config(C)).await;
    rig.lan.add(a).await;
    state(&mut rig.events, A, "flashing").await;
    rig.lan.add(b).await;
    state(&mut rig.events, B, "waiting").await;
    rig.lan.add(c).await;
    state(&mut rig.events, C, "waiting").await;
    // Re-flashing B (already queued) must not move it behind C.
    rig.engine.flash(B, profile()).await.unwrap();
    let e = next(
        &mut rig.events,
        |e| matches!(e, EngineEvent::Device { state, .. } if state.name() == "preparing"),
    )
    .await;
    assert!(
        matches!(e, EngineEvent::Device { mac, .. } if mac == B),
        "{e:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_and_start_on_fixed_ports_never_finds_them_busy() {
    let mut rig = rig(|_| {}).await;
    rig.engine.stop().await.unwrap();
    // Three distinct free ports: bound together (one at a time, a port
    // dropped could be handed out again at once), released together right
    // before the first start.
    let probes: Vec<std::net::UdpSocket> = (0..3)
        .map(|_| std::net::UdpSocket::bind("0.0.0.0:0").unwrap())
        .collect();
    let ports: Vec<u16> = probes
        .iter()
        .map(|s| s.local_addr().unwrap().port())
        .collect();
    let mut cfg = config(&rig.lan);
    (cfg.ports.bootp, cfg.ports.tftp, cfg.ports.netinstall) = (ports[0], ports[1], ports[2]);
    let slow_reaps = ni_engine::engine::reap_timeouts();
    drop(probes);
    for round in 0..20 {
        let started = rig
            .engine
            .start(cfg.clone())
            .await
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert_eq!(started.tftp.port(), cfg.ports.tftp);
        rig.engine.stop().await.unwrap();
    }
    next(&mut rig.events, |e| *e == EngineEvent::Stopped).await;
    assert_eq!(
        ni_engine::engine::reap_timeouts(),
        slow_reaps,
        "aborted engine tasks did not end in time"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn netinstall_send_failures_are_reported() {
    // A destination lo0 cannot reach: every Netinstall frame fails to send.
    // (Not the limited broadcast: that now falls back to lo0's directed
    // broadcast, which works; see `ni_net::BroadcastTarget`.)
    let mut rig = rig(|cfg| {
        cfg.ports.netinstall_dest = "240.0.0.1:5000".parse().unwrap();
    })
    .await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    let e = next(
        &mut rig.events,
        |e| matches!(e, EngineEvent::Warning(f) if f.code == "send_failed"),
    )
    .await;
    let EngineEvent::Warning(f) = e else {
        unreachable!()
    };
    assert_eq!(f.param("service"), Some("netinstall"));
    assert!(f.param("detail").is_some());
    assert!(f.param("count").is_some());
    // Rate-limited: retransmits within the next ~second add no second one.
    let end = tokio::time::Instant::now() + Duration::from_millis(500);
    while let Ok(Ok(e)) = tokio::time::timeout_at(end, rig.events.recv()).await {
        assert!(
            !matches!(&e, EngineEvent::Warning(f) if f.code == "send_failed"),
            "{e:?}"
        );
    }
}
