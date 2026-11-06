//! Assets as the engine sees them: failing preparations, missing kernels,
//! the offline fallback (only to the version a channel was last resolved
//! to) and the install version reported per device.

mod common;

use common::*;
use ni_assets::testutil::{tempdir, Reply, TestServer};
use ni_assets::{Channel, Version, VersionSpec};
use ni_engine::{DeviceState, EngineEvent, EngineHandle, Failure};
use ni_sim::lan::SimLan;

fn failure(e: &EngineEvent) -> Failure {
    match e {
        EngineEvent::Device {
            state: DeviceState::Failed { code, params },
            ..
        } => Failure {
            code: code.clone(),
            params: params.clone(),
        },
        other => panic!("not a failure: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_package_that_cannot_be_had_fails_the_device_with_assets_failed() {
    let mut rig = rig(|cfg| cfg.auto_repeat = false).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "waiting").await;
    let mut p = profile();
    p.extra_packages = vec!["nosuch".into()];
    rig.engine.flash(A, p).await.unwrap();
    let f = failure(&state(&mut rig.events, A, "failed").await);
    assert_eq!(
        (f.code.as_str(), f.param("cause")),
        ("assets_failed", Some("unknown_package"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_kernels_the_engine_reports_kernels_unavailable_and_keeps_running() {
    let cache = tempdir("engine-nokernels");
    let engine = EngineHandle::spawn(cache.join("cache"), offline_http()).unwrap();
    let mut events = engine.subscribe();
    let lan = SimLan::bind().await.unwrap();
    engine.start(config(&lan)).await.unwrap();
    let e = next(&mut events, |e| matches!(e, EngineEvent::Error(_))).await;
    let EngineEvent::Error(f) = e else {
        unreachable!()
    };
    assert_eq!(f.code, "kernels_unavailable");
    let snap = engine.snapshot().await.unwrap();
    assert!(snap.running);
    assert_eq!(snap.kernel_version, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_the_engine_stays_on_the_remembered_version() {
    // Online: stable resolves to 7.25.1; kernels and packages get cached.
    let mut rig = rig(|_| {}).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
    rig.engine.stop().await.unwrap();
    // A newer version was cached meanwhile (e.g. by `assets fetch`); the
    // offline fallback must not pick it just because it is the newest.
    let srv2 = TestServer::start().await;
    ni_assets::testutil::mirror_version(&srv2, "7.26");
    let newer = ni_assets::Assets::new(rig.cache.join("cache"), http(&srv2)).unwrap();
    newer
        .ensure_kernels(&Version::parse("7.26").unwrap())
        .await
        .unwrap();

    let offline = EngineHandle::spawn(rig.cache.join("cache"), offline_http()).unwrap();
    let mut events = offline.subscribe();
    let started = offline.start(config(&rig.lan)).await.unwrap();
    rig.lan.connect(started.netinstall).await;
    let e = next(
        &mut events,
        |e| matches!(e, EngineEvent::Warning(f) if f.code == "kernels_offline_fallback"),
    )
    .await;
    assert_eq!(
        e,
        EngineEvent::Warning(Failure::new("kernels_offline_fallback").with("version", "7.25.1"))
    );
    next(&mut events, |e| {
        *e == EngineEvent::KernelsReady {
            version: "7.25.1".into(),
        }
    })
    .await;
    rig.started = started;
    let sim = boot(&rig, B, sim_config(B)).await;
    rig.lan.add(sim).await;
    let seen = until_finished(&mut events, B).await;
    let versions: Vec<Option<String>> = seen
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Device {
                mac,
                state: DeviceState::Flashing { version, .. },
            } if *mac == B => Some(Some(version.clone())),
            EngineEvent::Device {
                mac,
                state: DeviceState::Preparing { version },
            } if *mac == B => Some(version.clone()),
            _ => None,
        })
        .collect();
    assert!(!versions.is_empty());
    assert!(
        versions.iter().all(|v| v.as_deref() == Some("7.25.1")),
        "{versions:?}"
    );
    assert_eq!(states(&seen, B).last(), Some(&"done"));
    offline.stop().await.unwrap();

    // A channel never resolved here has nothing to fall back to, even
    // though kernels of other versions are cached.
    let mut cfg = config(&rig.lan);
    let mut p = profile();
    p.version = VersionSpec::Channel(Channel::Testing);
    cfg.profile = Some(p);
    offline.start(cfg).await.unwrap();
    let e = next(&mut events, |e| matches!(e, EngineEvent::Error(_))).await;
    let EngineEvent::Error(f) = e else {
        unreachable!()
    };
    assert_eq!(f.code, "kernels_unavailable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_install_version_is_reported_and_must_match_the_kernels() {
    let mut rig = rig(|cfg| cfg.auto_repeat = false).await;
    // `testing` points at a version other than the kernels being served.
    rig.srv.route(
        "/up/NEWESTa7.testing",
        Reply::Body(b"7.26 1793623193\n".to_vec()),
    );
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "waiting").await;
    // An exact version is checked right away...
    let mut exact = profile();
    exact.version = VersionSpec::Exact(Version::parse("7.24.6").unwrap());
    let err = rig.engine.flash(A, exact).await.unwrap_err();
    assert_eq!(err.code, "profile_version_mismatch");
    assert_eq!(err.param("version"), Some("7.24.6"));
    assert_eq!(err.param("kernel_version"), Some("7.25.1"));
    // ...a channel once it is resolved.
    let mut testing = profile();
    testing.version = VersionSpec::Channel(Channel::Testing);
    rig.engine.flash(A, testing).await.unwrap();
    let f = failure(&state(&mut rig.events, A, "failed").await);
    assert_eq!(
        f,
        Failure::new("profile_version_mismatch")
            .with("version", "7.26")
            .with("kernel_version", "7.25.1")
    );
    // The matching version installs, and the snapshot names it.
    rig.lan.reset(A).await;
    rig.engine.flash(A, profile()).await.unwrap();
    state(&mut rig.events, A, "done").await;
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!(snap.devices[0].version.as_deref(), Some("7.25.1"));
}
