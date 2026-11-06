//! Runtime control for the GUI: kernel state, snapshot details, and
//! changing settings while running.

mod common;

use common::*;
use ni_engine::{EngineEvent, EngineHandle, KernelState};
use std::time::{SystemTime, UNIX_EPOCH};

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
    assert_eq!((snap.max_parallel, snap.cooldown_secs), (4, 300));
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
