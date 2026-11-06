//! End to end over loopback: a simulated RouterBOARD boots from the engine
//! (BOOTP + TFTP) and gets flashed over a real UDP socket.

mod common;

use common::*;
use ni_engine::{AfterInstall, ConfigMode, DeviceState, EngineEvent, WaitReason};
use ni_proto::message::CONFIG_SCRIPT_NAME;
use ni_proto::Offer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_device_is_booted_and_flashed_with_packages_and_script() {
    let script = b"/system identity set name=sim\n";
    let mut rig = rig(|_| {}).await;
    let path = rig.cache.join("Setup.RSC");
    std::fs::write(&path, script).unwrap();
    let local = rig
        .engine
        .assets()
        .register_local_file(&path)
        .await
        .unwrap();
    assert_eq!(local.name, "setup.rsc");
    rig.engine.stop().await.unwrap();
    let mut p = profile();
    p.config = ConfigMode::Script {
        local_name: local.name,
    };
    let mut cfg = config(&rig.lan);
    cfg.profile = Some(p);
    rig.started = rig.engine.start(cfg).await.unwrap();
    rig.lan.connect(rig.started.netinstall).await;

    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    let events = until_finished(&mut rig.events, A).await;
    assert_eq!(
        dedup(states(&events, A)),
        ["bootp_seen", "discovered", "preparing", "flashing", "done"]
    );
    let discovered = events.iter().find_map(|e| match e {
        EngineEvent::Discovered { mac, info } if *mac == A => Some(info.clone()),
        _ => None,
    });
    let info = discovered.expect("discovered event");
    assert_eq!(
        (info.model.as_str(), info.arch.as_str()),
        ("RB5009UG+S+", "arm64")
    );

    let report = rig
        .lan
        .wait_terminated(A, WAIT)
        .await
        .expect("device rebooted");
    assert_eq!(report.violations, Vec::<String>::new());
    let files: Vec<(&str, &[u8])> = report
        .received
        .iter()
        .map(|f| (f.name.as_str(), f.data.as_slice()))
        .collect();
    assert_eq!(
        files,
        [
            ("routeros-7.25.1-arm64.npk", rig.main.as_slice()),
            ("wireless-7.25.1-arm64.npk", rig.wireless.as_slice()),
            (CONFIG_SCRIPT_NAME, &script[..]),
        ]
    );

    let snap = rig.engine.snapshot().await.unwrap();
    assert!(snap.running);
    assert_eq!(snap.kernel_version.as_deref(), Some("7.25.1"));
    assert_eq!(snap.devices.len(), 1);
    assert_eq!(snap.devices[0].state, DeviceState::Done);
    assert_eq!(snap.devices[0].profile.as_deref(), Some("test"));
    assert_eq!(snap.devices[0].config.as_deref(), Some("script"));
    assert_eq!(snap.devices[0].boot_arch.as_deref(), Some("arm64"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_profile_options_reach_the_device() {
    let mut p = profile();
    p.config = ConfigMode::Keep;
    p.remove_branding = true;
    p.license_key = Some("KEY-1".into());
    p.after_install = AfterInstall::Shutdown;
    let mut rig = rig(|cfg| cfg.profile = Some(p)).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "done").await;
    let report = rig.lan.wait_terminated(A, WAIT).await.unwrap();
    assert_eq!(
        report.offer,
        Some(Offer {
            license_key: "KEY-1".into(),
            keep_config: true,
            remove_branding: true
        })
    );
    assert_eq!(report.after_install, Some(ni_proto::AfterInstall::Shutdown));
    assert_eq!(report.received.len(), 2, "packages only");
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!(snap.devices[0].config.as_deref(), Some("keep"));
}

/// Consecutive duplicates removed (progress repeats the `flashing` state).
fn dedup(mut v: Vec<&'static str>) -> Vec<&'static str> {
    v.dedup();
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_auto_repeat_a_device_waits_for_flash() {
    let mut rig = rig(|cfg| cfg.auto_repeat = false).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    let e = state(&mut rig.events, A, "waiting").await;
    assert_eq!(
        e,
        EngineEvent::Device {
            mac: A,
            state: DeviceState::Waiting {
                reason: WaitReason::NoProfile
            }
        }
    );
    let err = rig.engine.flash(B, profile()).await.unwrap_err();
    assert_eq!(err.code, "unknown_device");
    let mut p = profile();
    p.name = "manual".into();
    rig.engine.flash(A, p).await.unwrap();
    state(&mut rig.events, A, "done").await;
    let snap = rig.engine.snapshot().await.unwrap();
    assert_eq!(snap.devices[0].profile.as_deref(), Some("manual"));
    assert_eq!(
        rig.engine.flash(A, profile()).await.unwrap_err().code,
        "device_done"
    );
    assert_eq!(rig.engine.retry(A).await.unwrap_err().code, "not_failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turning_auto_repeat_on_starts_waiting_devices() {
    let mut rig = rig(|cfg| cfg.auto_repeat = false).await;
    let sim = boot(&rig, A, sim_config(A)).await;
    rig.lan.add(sim).await;
    state(&mut rig.events, A, "waiting").await;
    rig.engine.set_auto_repeat(true).await.unwrap();
    state(&mut rig.events, A, "done").await;
    assert!(rig.engine.snapshot().await.unwrap().auto_repeat);
}
