//! Runtime control for the GUI: kernel state, snapshot details, and
//! changing settings while running.

mod common;

use common::*;
use ni_assets::testutil::{mirror_version, netinstall_tarball, Reply};
use ni_assets::{Version, VersionSpec};
use ni_engine::{DeviceState, EngineEvent, EngineHandle, KernelState, Profile, WaitReason};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// The kernel states reported until (and including) the first one that is
/// not `downloading`.
async fn kernel_states(
    events: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
) -> Vec<KernelState> {
    let mut out = Vec::new();
    loop {
        let e = next(events, |e| matches!(e, EngineEvent::Kernels { .. })).await;
        let EngineEvent::Kernels { state } = e else {
            unreachable!()
        };
        let done = state.name() != "downloading";
        out.push(state);
        if done {
            return out;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_state_goes_from_downloading_to_ready() {
    let srv = ni_assets::testutil::TestServer::start().await;
    ni_assets::testutil::mirror(&srv);
    let cache = ni_assets::testutil::tempdir("control-kernels");
    let engine = EngineHandle::spawn(cache.join("cache"), http(&srv)).unwrap();
    assert_eq!(engine.snapshot().await.unwrap().kernels, KernelState::Idle);
    let mut events = engine.subscribe();
    let lan = ni_sim::lan::SimLan::bind().await.unwrap();
    engine.start(config(&lan)).await.unwrap();
    let states = kernel_states(&mut events).await;
    assert_eq!(
        states.first(),
        Some(&KernelState::Downloading {
            spec: "stable".into(),
            done: 0,
            total: None
        })
    );
    assert_eq!(
        states.last(),
        Some(&KernelState::Ready {
            version: "7.25.1".into(),
            offline: false
        })
    );
    let snap = engine.snapshot().await.unwrap();
    assert_eq!(snap.kernels, *states.last().unwrap());
    assert_eq!(snap.kernel_version.as_deref(), Some("7.25.1"));
    assert_eq!((snap.max_parallel, snap.cooldown_secs), (1, 300));
    assert_eq!((snap.only_mac, snap.link_down), (None, None));
    engine.stop().await.unwrap();
    assert_eq!(engine.snapshot().await.unwrap().kernels, KernelState::Idle);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_kernels_the_snapshot_says_unavailable() {
    let cache = ni_assets::testutil::tempdir("control-nokernels");
    let engine = EngineHandle::spawn(cache.join("cache"), offline_http()).unwrap();
    let mut events = engine.subscribe();
    let lan = ni_sim::lan::SimLan::bind().await.unwrap();
    engine.start(config(&lan)).await.unwrap();
    let states = kernel_states(&mut events).await;
    let last = states.last().unwrap();
    assert_eq!(last.name(), "unavailable");
    let KernelState::Unavailable { code, params } = last else {
        unreachable!()
    };
    assert_eq!(code, "kernels_unavailable");
    assert_eq!(params.get("cause").map(String::as_str), Some("network"));
    let snap = engine.snapshot().await.unwrap();
    assert!(snap.running, "running, but serving no kernels");
    assert_eq!(snap.kernel_version, None);
    assert_eq!(snap.kernels, *last);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn devices_carry_attempts_and_timestamps() {
    let before = now_ms();
    let mut rig = rig(|_| {}).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
    let snap = rig.engine.snapshot().await.unwrap();
    let dev = &snap.devices[0];
    assert_eq!(dev.attempt, 1);
    assert!(dev.first_seen_ms >= before, "first seen during the test");
    assert!(dev.updated_ms >= dev.first_seen_ms);
    assert!(dev.updated_ms <= now_ms());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raising_the_limit_starts_a_waiting_device_at_once() {
    let mut rig = rig(|cfg| cfg.max_parallel = 1).await;
    let mut slow = sim_config(A);
    slow.format_delay = Duration::from_millis(1500);
    let a = boot(&rig, A, slow).await;
    let b = boot(&rig, B, sim_config(B)).await;
    rig.lan.add(a).await;
    state(&mut rig.events, A, "flashing").await;
    rig.lan.add(b).await;
    state(&mut rig.events, B, "waiting").await;
    assert_eq!(
        rig.engine.set_max_parallel(0).await.unwrap_err().code,
        "invalid_max_parallel"
    );
    rig.engine.set_max_parallel(2).await.unwrap();
    state(&mut rig.events, B, "preparing").await;
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!(snap.max_parallel, 2);
    let a_state = snap.devices.iter().find(|d| d.mac == A).unwrap();
    assert_eq!(a_state.state.name(), "flashing", "B started while A runs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cooldown_can_change_but_not_below_the_minimum() {
    let rig = rig(|_| {}).await;
    let err = rig
        .engine
        .set_cooldown(Duration::from_secs(29))
        .await
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("min_secs")),
        ("invalid_cooldown", Some("30"))
    );
    rig.engine
        .set_cooldown(Duration::from_secs(90))
        .await
        .unwrap();
    assert_eq!(rig.engine.snapshot().await.unwrap().cooldown_secs, 90);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_mac_can_change_while_running() {
    let mut rig = rig(|_| {}).await;
    rig.engine.set_only_mac(Some(B)).await.unwrap();
    let e = next(&mut rig.events, |e| {
        matches!(e, EngineEvent::OnlyMacChanged { .. })
    })
    .await;
    assert_eq!(e, EngineEvent::OnlyMacChanged { mac: Some(B) });
    assert_eq!(rig.engine.snapshot().await.unwrap().only_mac, Some(B));
    let reply = rig
        .lan
        .boot(
            A,
            "ARM64__boot",
            rig.started.bootp,
            Duration::from_millis(500),
        )
        .await;
    assert_eq!(reply, None, "A is not selected: no boot file");
    let e = next(&mut rig.events, |e| {
        matches!(e, EngineEvent::Ignored { .. })
    })
    .await;
    assert_eq!(
        e,
        EngineEvent::Ignored {
            mac: A,
            reason: "not_selected".into()
        }
    );
    rig.engine.set_only_mac(None).await.unwrap();
    let e = next(&mut rig.events, |e| {
        matches!(e, EngineEvent::OnlyMacChanged { .. })
    })
    .await;
    assert_eq!(e, EngineEvent::OnlyMacChanged { mac: None });
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
}

#[tokio::test]
async fn runtime_settings_need_a_running_engine() {
    let cache = ni_assets::testutil::tempdir("control-idle");
    let engine = EngineHandle::spawn(cache.join("cache"), offline_http()).unwrap();
    assert_eq!(
        engine.set_only_mac(None).await.unwrap_err().code,
        "not_running"
    );
    assert_eq!(
        engine.set_max_parallel(2).await.unwrap_err().code,
        "not_running"
    );
    let err = engine
        .set_cooldown(Duration::from_secs(60))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not_running");
}

/// A profile installing exactly `version` (plus `wireless`).
fn exact(name: &str, version: &str) -> Profile {
    let mut p = Profile::new(name, VersionSpec::Exact(Version::parse(version).unwrap()));
    p.extra_packages = vec!["wireless".into()];
    p
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_active_version_withdraws_and_refetches_the_kernels() {
    let mut rig = rig(|_| {}).await;
    // 7.24.6 exists too; its tarball arrives slowly, so the switch is
    // observable.
    mirror_version(&rig.srv, "7.24.6");
    rig.srv.route(
        "/dl/7.24.6/netinstall-7.24.6.tar.gz",
        Reply::Slow {
            body: netinstall_tarball(),
            chunk: 64 * 1024,
            delay: Duration::from_millis(150),
        },
    );
    rig.engine
        .set_active_profile(Some(exact("old", "7.24.6")))
        .await
        .unwrap();
    let e = next(&mut rig.events, |e| {
        matches!(e, EngineEvent::Kernels { .. })
    })
    .await;
    assert_eq!(
        e,
        EngineEvent::Kernels {
            state: KernelState::Downloading {
                spec: "7.24.6".into(),
                done: 0,
                total: None
            }
        }
    );
    // Running, but no kernels: a netbooting device gets no boot file.
    let snap = rig.engine.snapshot().await.unwrap();
    assert!(snap.running);
    assert_eq!(snap.kernel_version, None);
    assert_eq!(snap.kernels.name(), "downloading");
    assert_eq!(snap.profile.as_deref(), Some("old"));
    let reply = rig
        .lan
        .boot(
            A,
            "ARM64__boot",
            rig.started.bootp,
            Duration::from_millis(300),
        )
        .await;
    assert_eq!(reply, None);
    let states = kernel_states(&mut rig.events).await;
    assert!(
        states
            .iter()
            .any(|s| matches!(s, KernelState::Downloading { done, .. } if *done > 0)),
        "download progress is reported: {states:?}"
    );
    assert_eq!(
        states.last(),
        Some(&KernelState::Ready {
            version: "7.24.6".into(),
            offline: false
        })
    );
    // Now A boots and gets 7.24.6.
    let sim = boot_version(&rig, A, sim_config(A), "7.24.6").await;
    rig.lan.add(sim).await;
    let e = state(&mut rig.events, A, "flashing").await;
    let EngineEvent::Device {
        state: DeviceState::Flashing { version, .. },
        ..
    } = e
    else {
        unreachable!()
    };
    assert_eq!(version, "7.24.6");
    state(&mut rig.events, A, "done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_keeps_the_kernel_version_it_booted() {
    let mut rig = rig(|cfg| cfg.auto_repeat = false).await;
    mirror_version(&rig.srv, "7.24.6");
    let sim = boot(&rig, A, sim_config(A)).await; // boots 7.25.1 kernels
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "waiting").await;
    rig.engine
        .set_active_profile(Some(exact("old", "7.24.6")))
        .await
        .unwrap();
    next(&mut rig.events, |e| {
        matches!(e, EngineEvent::KernelsReady { .. })
    })
    .await;
    let err = rig
        .engine
        .flash(A, exact("old", "7.24.6"))
        .await
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("kernel_version")),
        ("profile_version_mismatch", Some("7.25.1"))
    );
    rig.engine.flash(A, exact("new", "7.25.1")).await.unwrap();
    state(&mut rig.events, A, "done").await;
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!(snap.devices[0].version.as_deref(), Some("7.25.1"));
    assert_eq!(snap.kernel_version.as_deref(), Some("7.24.6"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_version_spec_keeps_the_kernels_and_none_stops_auto_repeat() {
    let mut rig = rig(|_| {}).await;
    let mut other = profile();
    other.name = "other".into();
    other.extra_packages.clear();
    rig.engine.set_active_profile(Some(other)).await.unwrap();
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!(snap.profile.as_deref(), Some("other"));
    assert_eq!(
        snap.kernel_version.as_deref(),
        Some("7.25.1"),
        "not withdrawn"
    );
    rig.engine.set_active_profile(None).await.unwrap();
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!((snap.profile, snap.auto_repeat), (None, false));
    assert_eq!(snap.kernels.name(), "ready");
    let mut bad = profile();
    bad.name = String::new();
    let err = rig.engine.set_active_profile(Some(bad)).await.unwrap_err();
    assert_eq!(err.code, "invalid_name");
    // No `kernels` event was sent for any of this.
    while let Ok(e) = rig.events.try_recv() {
        assert!(!matches!(e, EngineEvent::Kernels { .. }), "{e:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_flashing_device_frees_its_slot() {
    let mut rig = rig(|cfg| cfg.max_parallel = 1).await;
    let mut slow = sim_config(A);
    slow.format_delay = Duration::from_secs(3);
    let a = boot(&rig, A, slow).await;
    let b = boot(&rig, B, sim_config(B)).await;
    rig.lan.add(a).await;
    state(&mut rig.events, A, "flashing").await;
    rig.lan.add(b).await;
    state(&mut rig.events, B, "waiting").await;
    assert_eq!(rig.engine.forget(A).await.unwrap_err().code, "device_busy");
    rig.engine.cancel(A).await.unwrap();
    let e = state(&mut rig.events, A, "failed").await;
    assert_eq!(
        e,
        EngineEvent::Device {
            mac: A,
            state: DeviceState::failed(ni_engine::Failure::new("cancelled"))
        }
    );
    state(&mut rig.events, B, "preparing").await;
    state(&mut rig.events, B, "done").await;
    let err = rig.engine.cancel(B).await.unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("state")),
        ("not_cancellable", Some("done"))
    );
    let err = rig.engine.cancel(C).await.unwrap_err();
    assert_eq!(err.code, "unknown_device");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_queued_device_takes_it_out_of_the_queue() {
    let mut rig = rig(|cfg| cfg.max_parallel = 1).await;
    let mut slow = sim_config(A);
    slow.format_delay = Duration::from_millis(800);
    let a = boot(&rig, A, slow).await;
    let b = boot(&rig, B, sim_config(B)).await;
    rig.lan.add(a).await;
    state(&mut rig.events, A, "flashing").await;
    rig.lan.add(b).await;
    state(&mut rig.events, B, "waiting").await;
    rig.engine.cancel(B).await.unwrap();
    let e = next(
        &mut rig.events,
        |e| matches!(e, EngineEvent::Device { mac, .. } if *mac == B),
    )
    .await;
    assert_eq!(
        e,
        EngineEvent::Device {
            mac: B,
            state: DeviceState::Waiting {
                reason: WaitReason::NoProfile
            }
        }
    );
    state(&mut rig.events, A, "done").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let snap = rig.engine.snapshot().await.unwrap();
    let b = snap.devices.iter().find(|d| d.mac == B).unwrap();
    assert_eq!(b.state.name(), "waiting", "not started when the slot freed");
    assert_eq!((b.attempt, b.profile.as_deref()), (0, None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forgotten_device_is_reported_and_new_again() {
    let mut rig = rig(|_| {}).await;
    let reply = rig
        .lan
        .boot(A, "ARM64__boot", rig.started.bootp, WAIT)
        .await;
    assert!(reply.is_some());
    state(&mut rig.events, A, "bootp_seen").await;
    rig.engine.forget(A).await.unwrap();
    let e = next(&mut rig.events, |e| {
        matches!(e, EngineEvent::Forgotten { .. })
    })
    .await;
    assert_eq!(e, EngineEvent::Forgotten { mac: A });
    assert!(rig.engine.snapshot().await.unwrap().devices.is_empty());
    assert_eq!(
        rig.engine.forget(A).await.unwrap_err().code,
        "unknown_device"
    );
    // Netbooting again, it starts over as a new device.
    let reply = rig
        .lan
        .boot(A, "ARM64__boot", rig.started.bootp, WAIT)
        .await;
    assert!(reply.is_some());
    state(&mut rig.events, A, "bootp_seen").await;
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!(snap.devices[0].attempt, 0, "a new device");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_done_device_cannot_be_forgotten_during_its_cooldown() {
    let mut rig = rig(|cfg| {
        cfg.min_cooldown = Duration::from_millis(100);
        cfg.cooldown = Duration::from_millis(800);
    })
    .await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
    let err = rig.engine.forget(A).await.unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("mac")),
        ("device_in_cooldown", Some(A.to_string().as_str()))
    );
    assert_eq!(rig.engine.snapshot().await.unwrap().devices.len(), 1);
    tokio::time::sleep(Duration::from_millis(900)).await;
    rig.engine.forget(A).await.unwrap();
    next(
        &mut rig.events,
        |e| matches!(e, EngineEvent::Forgotten { mac } if *mac == A),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_mac_holds_back_queued_devices_of_other_macs() {
    let mut rig = rig(|cfg| cfg.max_parallel = 1).await;
    let mut slow = sim_config(A);
    slow.format_delay = Duration::from_millis(800);
    let a = boot(&rig, A, slow).await;
    let b = boot(&rig, B, sim_config(B)).await;
    rig.lan.add(a).await;
    state(&mut rig.events, A, "flashing").await;
    rig.lan.add(b).await;
    state(&mut rig.events, B, "waiting").await;
    rig.engine.set_only_mac(Some(A)).await.unwrap();
    state(&mut rig.events, A, "done").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let snap = rig.engine.snapshot().await.unwrap();
    let b_now = snap.devices.iter().find(|d| d.mac == B).unwrap();
    assert_eq!(b_now.state.name(), "waiting", "B is not admitted");
    assert_eq!(b_now.attempt, 0);
    rig.engine.set_only_mac(None).await.unwrap();
    state(&mut rig.events, B, "preparing").await;
    state(&mut rig.events, B, "done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_kernels_are_fetched_again_for_the_same_spec() {
    let srv = ni_assets::testutil::TestServer::start().await;
    ni_assets::testutil::mirror(&srv);
    let tarball = "/dl/7.25.1/netinstall-7.25.1.tar.gz";
    srv.route(tarball, Reply::Status(503));
    let cache = ni_assets::testutil::tempdir("control-refetch");
    let engine = EngineHandle::spawn(cache.join("cache"), http(&srv)).unwrap();
    let mut events = engine.subscribe();
    let lan = ni_sim::lan::SimLan::bind().await.unwrap();
    engine.start(config(&lan)).await.unwrap();
    let states = kernel_states(&mut events).await;
    assert_eq!(states.last().unwrap().name(), "unavailable");
    // Without a profile too (stable); the mirror still fails.
    engine.set_active_profile(None).await.unwrap();
    let states = kernel_states(&mut events).await;
    assert_eq!(states.first().unwrap().name(), "downloading");
    assert_eq!(states.last().unwrap().name(), "unavailable");
    srv.route(tarball, Reply::Body(netinstall_tarball()));
    engine.set_active_profile(Some(profile())).await.unwrap();
    let states = kernel_states(&mut events).await;
    assert_eq!(
        states.last(),
        Some(&KernelState::Ready {
            version: "7.25.1".into(),
            offline: false
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_of_a_superseded_kernel_fetch_is_not_reported() {
    let mut rig = rig(|_| {}).await;
    mirror_version(&rig.srv, "7.24.6");
    mirror_version(&rig.srv, "7.23.2");
    rig.srv.route(
        "/dl/7.24.6/netinstall-7.24.6.tar.gz",
        // Big and slow: still downloading long after it is superseded
        // (it would fail its checksum in the end; nobody waits for that).
        Reply::Slow {
            body: vec![0; 4 << 20],
            chunk: 64 * 1024,
            delay: Duration::from_millis(40),
        },
    );
    // The new fetch never gets past its first byte.
    rig.srv
        .route("/dl/7.23.2/netinstall-7.23.2.tar.gz", Reply::Stall);
    rig.engine
        .set_active_profile(Some(exact("old", "7.24.6")))
        .await
        .unwrap();
    next(&mut rig.events, |e| {
        matches!(e, EngineEvent::Kernels { state: KernelState::Downloading { done, .. } } if *done > 0)
    })
    .await;
    rig.engine
        .set_active_profile(Some(exact("older", "7.23.2")))
        .await
        .unwrap();
    let end = tokio::time::Instant::now() + Duration::from_millis(1500);
    let mut seen = Vec::new();
    while let Ok(Ok(e)) = tokio::time::timeout_at(end, rig.events.recv()).await {
        if let EngineEvent::Kernels { state } = e {
            seen.push(state);
        }
    }
    assert!(!seen.is_empty(), "the new fetch is reported");
    for s in &seen {
        match s {
            KernelState::Downloading { spec, done, .. } => {
                assert_eq!(spec, "7.23.2", "{seen:?}");
                assert_eq!(*done, 0, "no progress of the old fetch: {seen:?}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let snap = rig.engine.snapshot().await.unwrap();
    assert!(
        matches!(&snap.kernels, KernelState::Downloading { spec, done: 0, .. } if spec == "7.23.2"),
        "{:?}",
        snap.kernels
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_files_name_their_version_so_a_swap_cannot_mix_kernels() {
    let mut rig = rig(|cfg| cfg.auto_repeat = false).await;
    mirror_version(&rig.srv, "7.24.6");
    // A gets its BOOTP reply for the 7.25.1 kernels ...
    let reply = rig
        .lan
        .boot(A, "ARM64__boot", rig.started.bootp, WAIT)
        .await
        .expect("BOOTP reply");
    assert_eq!(reply.file, "kernel-7.25.1-arm64.elf");
    // ... then the kernels are swapped before it fetches the file.
    rig.engine
        .set_active_profile(Some(exact("old", "7.24.6")))
        .await
        .unwrap();
    next(&mut rig.events, |e| {
        matches!(e, EngineEvent::KernelsReady { .. })
    })
    .await;
    let tftp = std::net::SocketAddr::from((reply.server, rig.started.tftp.port()));
    assert!(
        ni_sim::lan::tftp_get(tftp, &reply.file).await.is_err(),
        "the old name does not give the new kernel"
    );
    assert!(ni_sim::lan::tftp_get(tftp, "kernel-7.24.6-arm64.elf")
        .await
        .is_ok());
    // Whatever it booted, the engine goes by the name it was given.
    rig.lan
        .add(ni_sim::DeviceSim::new(
            sim_config(A),
            std::time::Instant::now(),
        ))
        .await;
    state(&mut rig.events, A, "waiting").await;
    let err = rig
        .engine
        .flash(A, exact("old", "7.24.6"))
        .await
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("kernel_version")),
        ("profile_version_mismatch", Some("7.25.1"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_with_an_unknown_kernel_is_not_flashed() {
    let mut rig = rig(|_| {}).await;
    mirror_version(&rig.srv, "7.24.6");
    rig.srv
        .route("/dl/7.24.6/netinstall-7.24.6.tar.gz", Reply::Stall);
    rig.engine
        .set_active_profile(Some(exact("old", "7.24.6")))
        .await
        .unwrap();
    // No kernels are served now, and A was never seen netbooting: what it
    // runs is unknown, so it must not be installed.
    rig.lan
        .add(ni_sim::DeviceSim::new(
            sim_config(A),
            std::time::Instant::now(),
        ))
        .await;
    let e = state(&mut rig.events, A, "failed").await;
    let EngineEvent::Device {
        state: DeviceState::Failed { code, .. },
        ..
    } = e
    else {
        unreachable!()
    };
    assert_eq!(code, "kernel_version_unknown");
    let err = rig
        .engine
        .flash(A, exact("old", "7.24.6"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "kernel_version_unknown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cooldowns_have_an_upper_bound() {
    let rig = rig(|_| {}).await;
    for secs in [86_401, u64::MAX] {
        let err = rig
            .engine
            .set_cooldown(Duration::from_secs(secs))
            .await
            .unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("max_secs")),
            ("invalid_cooldown", Some("86400"))
        );
    }
    rig.engine
        .set_cooldown(Duration::from_secs(86_400))
        .await
        .unwrap();
}

#[tokio::test]
async fn cancel_and_forget_need_a_running_engine() {
    let cache = ni_assets::testutil::tempdir("control-idle2");
    let engine = EngineHandle::spawn(cache.join("cache"), offline_http()).unwrap();
    assert_eq!(engine.cancel(A).await.unwrap_err().code, "not_running");
    assert_eq!(engine.forget(A).await.unwrap_err().code, "not_running");
}
