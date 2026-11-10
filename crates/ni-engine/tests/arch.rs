//! Device architectures: every package is checked against the device's
//! hello arch before anything is offered, and an arch string we do not
//! know is resolved from the packages' contents.

mod common;

use common::*;
use ni_assets::testutil::mirror_arch;
use ni_engine::{DeviceState, EngineEvent, Failure};
use ni_proto::Mac;
use ni_sim::{DeviceSim, SimConfig};
use std::net::SocketAddr;
use std::time::Instant;

/// Boot a simulated MIPS board (`Mips_boot`) saying `arch` in its hello.
async fn boot_mips(rig: &Rig, mac: Mac, arch: &str) -> DeviceSim {
    let reply = rig
        .lan
        .boot(mac, "Mips_boot", rig.started.bootp, WAIT)
        .await
        .expect("BOOTP reply");
    assert_eq!(reply.file, "kernel-7.25.1-mips.elf");
    let tftp = SocketAddr::from((reply.server, rig.started.tftp.port()));
    ni_sim::lan::tftp_get(tftp, &reply.file).await.unwrap();
    let mut cfg: SimConfig = sim_config(mac);
    cfg.info.model = "RB-mips".into();
    cfg.info.arch = arch.into();
    DeviceSim::new(cfg, Instant::now())
}

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

/// A board saying `smips` whose `smips` downloads are built for `mips`:
/// refused before anything is offered (no OFFR, nothing formatted).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn packages_for_another_arch_are_refused_before_the_offer() {
    let mut rig = rig(|_| {}).await;
    mirror_arch(&rig.srv, "smips", "mips");
    mirror_arch(&rig.srv, "mipsbe", "mips");
    let sim = boot_mips(&rig, A, "smips").await;
    rig.lan.add(sim).await;
    let f = failure(&state(&mut rig.events, A, "failed").await);
    assert_eq!(f.code, "package_arch_mismatch");
    assert_eq!(f.param("arch"), Some("smips"));
    assert_eq!(f.param("package"), Some("routeros-7.25.1-smips.npk"));
    assert_eq!(f.param("package_arch"), Some("mips"));
    let report = rig.lan.report(A).await.unwrap();
    assert_eq!(report.frames_to_device, 0, "no frame (OFFR) was sent");
    assert_eq!(report.offer, None);
    assert!(report.received.is_empty());
}

/// A made-up `xmips` from a `Mips_boot` board: the smips download says
/// `xmips` inside, so it is installed, and the resolution is reported and
/// remembered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_arch_is_resolved_and_installed() {
    let mut rig = rig(|_| {}).await;
    mirror_arch(&rig.srv, "mipsbe", "mips");
    let (main, wireless) = mirror_arch(&rig.srv, "smips", "xmips");
    let sim = boot_mips(&rig, A, "xmips").await;
    rig.lan.add(sim).await;
    let seen = until_finished(&mut rig.events, A).await;
    let info = seen
        .iter()
        .find_map(|e| match e {
            EngineEvent::Info(f) if f.code == "arch_resolved" => Some(f.clone()),
            _ => None,
        })
        .expect("arch_resolved");
    assert_eq!(info.param("arch"), Some("xmips"));
    assert_eq!(info.param("download_arch"), Some("smips"));
    assert_eq!(info.param("mac"), Some(A.to_string().as_str()));
    assert_eq!(states(&seen, A).last(), Some(&"done"));
    let report = rig.lan.report(A).await.unwrap();
    let got: Vec<(&str, &[u8])> = report
        .received
        .iter()
        .map(|f| (f.name.as_str(), f.data.as_slice()))
        .collect();
    assert_eq!(
        got,
        [
            ("routeros-7.25.1-smips.npk", main.as_slice()),
            ("wireless-7.25.1-smips.npk", wireless.as_slice()),
        ]
    );

    // A second board with the same hello arch: no new resolution.
    let sim = boot_mips(&rig, B, "XMIPS").await;
    rig.lan.add(sim).await;
    let seen = until_finished(&mut rig.events, B).await;
    assert_eq!(states(&seen, B).last(), Some(&"done"));
    assert!(!seen.iter().any(|e| matches!(e, EngineEvent::Info(_))));
}

/// No candidate's packages say the hello arch: `unknown_device_arch`,
/// with the details for a bug report, and nothing sent to the device.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_arch_no_package_is_built_for_is_unknown_device_arch() {
    let mut rig = rig(|_| {}).await;
    mirror_arch(&rig.srv, "mipsbe", "mips");
    mirror_arch(&rig.srv, "smips", "smips");
    let sim = boot_mips(&rig, A, "xmips").await;
    rig.lan.add(sim).await;
    let f = failure(&state(&mut rig.events, A, "failed").await);
    assert_eq!(f.code, "unknown_device_arch");
    assert_eq!(f.param("arch"), Some("xmips"));
    assert_eq!(f.param("model"), Some("RB-mips"));
    assert_eq!(f.param("boot_arch"), Some("mips"));
    assert!(f.param("hello").unwrap().contains("RB-mips | xmips"));
    let report = rig.lan.report(A).await.unwrap();
    assert_eq!(report.frames_to_device, 0);
}
