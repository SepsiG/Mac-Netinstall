//! The bridge without a window: `AppCore` with a real engine on `lo0`, a
//! fake MikroTik mirror and simulated RouterBOARDs (as in ni-engine's
//! tests), plus the event forwarder.

use mac_netinstall_app::core::{AppCore, Paths};
use mac_netinstall_app::forward::{forward_events, Forwarded};
use mac_netinstall_app::settings::Settings;
use mac_netinstall_app::views::StartRequest;
use ni_assets::testutil::{mirror, tempdir, TempDir, TestServer};
use ni_assets::{Channel, DeleteScope, HttpConfig, VersionSpec};
use ni_engine::{EngineEvent, Ports, Profile};
use ni_proto::{Mac, SessionConfig};
use ni_sim::lan::{tftp_get, SimLan};
use ni_sim::{DeviceSim, SimConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const A: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x0a]);
const WAIT: Duration = Duration::from_secs(10);

struct Rig {
    core: Arc<AppCore>,
    events: mpsc::UnboundedReceiver<Forwarded>,
    lan: SimLan,
    srv: TestServer,
    dir: TempDir,
}

fn http(srv: &TestServer) -> HttpConfig {
    HttpConfig {
        download_base: format!("{}/dl/", srv.base),
        upgrade_base: format!("{}/up", srv.base),
        ..HttpConfig::default()
    }
}

/// An app core whose engine runs on `lo0` against `lan`, with a stored
/// profile `test` (stable + wireless); its events go to `events`.
async fn rig(name: &str) -> Rig {
    let srv = TestServer::start().await;
    mirror(&srv);
    let dir = tempdir(name);
    let lan = SimLan::bind().await.unwrap();
    let (bootp, netinstall) = (lan.bootp_addr(), lan.netinstall_addr());
    let core = AppCore::new(Paths::under(&dir), http(&srv))
        .unwrap()
        .with_config_hook(Arc::new(move |cfg| {
            cfg.server_mac = Some(Mac([2, 0, 0, 0, 0, 1]));
            cfg.ports = Ports {
                bootp: 0,
                tftp: 0,
                netinstall: 0,
                bootp_reply_to: bootp,
                netinstall_dest: netinstall,
                ..Ports::default()
            };
            cfg.session = SessionConfig {
                reply_timeout: Duration::from_millis(150),
                format_timeout: Duration::from_secs(2),
                optional_timeout: Duration::from_millis(150),
                max_retransmits: 2,
                chunk_delay: Duration::from_millis(1),
            };
        }));
    let mut p = Profile::new("test", VersionSpec::Channel(Channel::Stable));
    p.extra_packages = vec!["wireless".into()];
    core.save_profile(&p, None).unwrap();
    let core = Arc::new(core);
    let (tx, events) = mpsc::unbounded_channel();
    tokio::spawn(forward_events(core.watch_engine(), move |f| {
        let _ = tx.send(f);
    }));
    Rig {
        core,
        events,
        lan,
        srv,
        dir,
    }
}

fn request() -> StartRequest {
    StartRequest {
        interface: "lo0".into(),
        profile: Some("test".into()),
        auto_repeat: true,
    }
}

async fn next(rig: &mut Rig, pred: impl Fn(&Forwarded) -> bool) -> Forwarded {
    let end = tokio::time::Instant::now() + WAIT;
    loop {
        match tokio::time::timeout_at(end, rig.events.recv()).await {
            Ok(Some(f)) if pred(&f) => return f,
            Ok(Some(_)) => {}
            Ok(None) => panic!("forwarder ended"),
            Err(_) => panic!("no matching event within {WAIT:?}"),
        }
    }
}

async fn device_state(rig: &mut Rig, mac: Mac, name: &str) {
    next(rig, |f| {
        matches!(f, Forwarded::Event(EngineEvent::Device { mac: m, state }) if *m == mac && state.name() == name)
    })
    .await;
}

async fn kernels_ready(rig: &mut Rig) {
    next(rig, |f| {
        matches!(f, Forwarded::Event(EngineEvent::KernelsReady { .. }))
    })
    .await;
}

/// BOOTP + TFTP like RouterBOOT, then the Netinstall device joins.
async fn boot(rig: &Rig, mac: Mac) {
    let snap = rig.core.snapshot().await.unwrap();
    let started = snap.started.unwrap();
    rig.lan.connect(started.netinstall).await;
    let reply = rig
        .lan
        .boot(mac, "ARM64__boot", started.bootp, WAIT)
        .await
        .expect("BOOTP reply");
    let tftp = SocketAddr::from((reply.server, started.tftp.port()));
    tftp_get(tftp, &reply.file).await.unwrap();
    let mut cfg = SimConfig::new(mac, "RB5009UG+S+", "arm64");
    cfg.hello_interval = Duration::from_millis(100);
    cfg.resend_interval = Duration::from_millis(100);
    rig.lan.add(DeviceSim::new(cfg, Instant::now())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_flash_and_remember_the_choices() {
    let mut rig = rig("bridge-flash").await;
    assert_eq!(rig.core.settings(), Settings::default());
    let report = rig.core.preflight(&request()).await.unwrap();
    assert!(report.network.is_some());
    let info = rig.core.start(&request()).await.unwrap();
    assert_eq!(info.interface, "lo0");
    kernels_ready(&mut rig).await;
    boot(&rig, A).await;
    device_state(&mut rig, A, "done").await;
    let snap = rig.core.snapshot().await.unwrap();
    assert_eq!(snap.devices[0].state.name(), "done");
    assert_eq!(snap.profile.as_deref(), Some("test"));
    let s = rig.core.settings();
    assert_eq!(
        (
            s.last_interface.as_deref(),
            s.last_profile.as_deref(),
            s.auto_repeat
        ),
        (Some("lo0"), Some("test"), true)
    );
    // Stored, too.
    let stored: Settings =
        serde_json::from_slice(&std::fs::read(rig.dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(stored, s);
    // Just flashed: kept until its cooldown is over.
    let err = rig.core.forget(&A.to_string()).await.unwrap_err();
    assert_eq!(err.code, "device_in_cooldown");
    assert_eq!(rig.core.snapshot().await.unwrap().devices.len(), 1);
    rig.core.stop().await.unwrap();
    next(&mut rig, |f| {
        matches!(f, Forwarded::Event(EngineEvent::Stopped))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_are_codes_without_paths() {
    let rig = rig("bridge-errors").await;
    let err = rig.core.flash("nope", "test").await.unwrap_err();
    assert_eq!(err.code, "invalid_mac");
    let mut req = request();
    req.profile = Some("missing".into());
    let err = rig.core.start(&req).await.unwrap_err();
    assert_eq!(err.code, "profile_not_found");
    assert_eq!(err.param("path"), None);
    let err = rig.core.retry(&A.to_string()).await.unwrap_err();
    assert_eq!(err.code, "not_running");
    let err = rig.core.set_only_mac(Some("zz")).await.unwrap_err();
    assert_eq!(err.code, "invalid_mac");
    let json = serde_json::to_value(&err).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"code": "invalid_mac", "params": {"mac": "zz"}})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_controls_reach_the_engine_and_are_remembered() {
    let mut rig = rig("bridge-controls").await;
    rig.core.start(&request()).await.unwrap();
    kernels_ready(&mut rig).await;
    rig.core.set_max_parallel(2).await.unwrap();
    rig.core.set_cooldown(120).await.unwrap();
    rig.core.set_only_mac(Some(&A.to_string())).await.unwrap();
    rig.core.set_auto_repeat(false).await.unwrap();
    let snap = rig.core.snapshot().await.unwrap();
    assert_eq!((snap.max_parallel, snap.cooldown_secs), (2, 120));
    assert_eq!((snap.only_mac, snap.auto_repeat), (Some(A), false));
    let s = rig.core.settings();
    assert_eq!(
        (s.max_parallel, s.cooldown_secs, s.auto_repeat),
        (2, 120, false)
    );
    // Settings saved while running apply at once.
    let mut s = rig.core.settings();
    s.max_parallel = 3;
    s.cooldown_secs = 60;
    rig.core.set_settings(s).await.unwrap();
    let snap = rig.core.snapshot().await.unwrap();
    assert_eq!((snap.max_parallel, snap.cooldown_secs), (3, 60));
    let mut bad = rig.core.settings();
    bad.language = Some("xx".into());
    let err = rig.core.set_settings(bad).await.unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("field")),
        ("invalid_setting", Some("language"))
    );
    rig.core.set_active_profile(None).await.unwrap();
    assert_eq!(rig.core.settings().last_profile, None);
    // What the running engine serves cannot be deleted; the rest can.
    let version = DeleteScope::Version {
        version: "7.25.1".into(),
    };
    let err = rig.core.delete_cached(version.clone()).await.unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("version")),
        ("asset_in_use", Some("7.25.1"))
    );
    let err = rig.core.delete_cached(DeleteScope::All).await.unwrap_err();
    assert_eq!(err.code, "asset_in_use");
    rig.core.stop().await.unwrap();
    rig.core.delete_cached(version).await.unwrap();
    rig.core.cancel(&A.to_string()).await.unwrap_err();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_settings_do_not_overwrite_remembered_choices() {
    let mut rig = rig("bridge-stale").await;
    // The settings dialog was opened before the start ...
    let stale = rig.core.settings();
    rig.core.start(&request()).await.unwrap();
    kernels_ready(&mut rig).await;
    // ... and saved after it: the remembered choices stay.
    let mut edit = stale.clone();
    edit.language = Some("hu".into());
    edit.max_parallel = 2;
    let saved = rig.core.set_settings(edit).await.unwrap();
    assert_eq!(saved.language.as_deref(), Some("hu"));
    assert_eq!(saved.max_parallel, 2);
    assert_eq!(
        (
            saved.last_interface.as_deref(),
            saved.last_profile.as_deref(),
            saved.auto_repeat
        ),
        (Some("lo0"), Some("test"), true)
    );
    assert_eq!(rig.core.settings(), saved);
    let stored: Settings =
        serde_json::from_slice(&std::fs::read(rig.dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(stored, saved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_preflight_and_start_do_not_orphan_the_engine() {
    let rig = rig("bridge-race").await;
    for (i, start_first) in [true, false, true, false].into_iter().enumerate() {
        // A new download timeout: the idle engine gets replaced.
        let mut s = rig.core.settings();
        s.download_timeout_secs = 600 + i as u64;
        rig.core.set_settings(s).await.unwrap();
        let req = request();
        if start_first {
            let (st, pf) = tokio::join!(rig.core.start(&req), rig.core.preflight(&req));
            st.unwrap();
            pf.unwrap();
        } else {
            let (pf, st) = tokio::join!(rig.core.preflight(&req), rig.core.start(&req));
            st.unwrap();
            pf.unwrap();
        }
        let snap = rig.core.snapshot().await.unwrap();
        assert!(
            snap.running,
            "round {i}: the started engine is the current one"
        );
        rig.core.stop().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_download_timeout_replaces_the_idle_engine() {
    let mut rig = rig("bridge-timeout").await;
    let before = rig.core.engine().assets().clone();
    let mut s = rig.core.settings();
    s.download_timeout_secs = 600;
    rig.core.set_settings(s).await.unwrap();
    rig.core.start(&request()).await.unwrap();
    assert!(
        !Arc::ptr_eq(&before, rig.core.engine().assets()),
        "a new engine task"
    );
    // The forwarder told the window to resync, and follows the new task.
    next(&mut rig, |f| *f == Forwarded::Lagged(0)).await;
    kernels_ready(&mut rig).await;
    // Unchanged timeout: same task.
    let engine = rig.core.engine().assets().clone();
    rig.core.stop().await.unwrap();
    rig.core.start(&request()).await.unwrap();
    assert!(Arc::ptr_eq(&engine, rig.core.engine().assets()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profiles_are_created_renamed_and_deleted() {
    let rig = rig("bridge-profiles").await;
    let mut p = Profile::new("Lab", VersionSpec::Channel(Channel::Testing));
    rig.core.save_profile(&p, None).unwrap();
    let err = rig.core.save_profile(&p, None).unwrap_err();
    assert_eq!(err.code, "profile_exists");
    let mut test = rig.core.list_profiles().profiles[1].clone();
    assert_eq!(test.name, "test");
    test.name = "lab".into();
    let err = rig.core.save_profile(&test, Some("test")).unwrap_err();
    assert_eq!(err.code, "profile_exists", "case-insensitive clash");
    // Rename Lab -> Lab 2, while it is the last used profile.
    let mut req = request();
    req.profile = Some("Lab".into());
    req.auto_repeat = false;
    rig.core.start(&req).await.unwrap();
    rig.core.stop().await.unwrap();
    assert_eq!(rig.core.settings().last_profile.as_deref(), Some("Lab"));
    p.name = "Lab 2".into();
    rig.core.save_profile(&p, Some("Lab")).unwrap();
    let names: Vec<_> = rig
        .core
        .list_profiles()
        .profiles
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["Lab 2", "test"]);
    assert_eq!(rig.core.settings().last_profile.as_deref(), Some("Lab 2"));
    // Editing in place.
    p.extra_packages = vec!["wireless".into()];
    rig.core.save_profile(&p, Some("lab 2")).unwrap();
    rig.core.delete_profile("Lab 2").unwrap();
    assert_eq!(rig.core.settings().last_profile, None);
    assert_eq!(
        rig.core.delete_profile("Lab 2").unwrap_err().code,
        "profile_not_found"
    );
    std::fs::write(rig.dir.join("profiles/broken.json"), b"{").unwrap();
    let listing = rig.core.list_profiles();
    assert_eq!(listing.broken.len(), 1);
    assert_eq!(
        (
            listing.broken[0].file.as_str(),
            listing.broken[0].code.as_str()
        ),
        ("broken.json", "invalid_profile")
    );
    let mut bad = Profile::new("x", VersionSpec::Channel(Channel::Stable));
    bad.extra_packages = vec!["routeros".into()];
    assert_eq!(
        rig.core.save_profile(&bad, None).unwrap_err().code,
        "invalid_package_name"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn packages_scripts_cache_and_log() {
    let rig = rig("bridge-assets").await;
    let pk = rig
        .core
        .available_packages("stable", "arm64")
        .await
        .unwrap();
    assert_eq!(
        (pk.version.as_str(), pk.arch.as_str(), pk.packages),
        (
            "7.25.1",
            "arm64",
            vec!["container".to_string(), "wireless".to_string()]
        )
    );
    let err = rig
        .core
        .available_packages("stable", "x86")
        .await
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("cause")),
        ("packages_unavailable", Some("unsupported_arch"))
    );
    let err = rig
        .core
        .available_packages("newest", "arm64")
        .await
        .unwrap_err();
    assert_eq!(err.param("cause"), Some("invalid_version"));
    let err = rig
        .core
        .available_packages("7.24.6", "arm64")
        .await
        .unwrap_err();
    assert_eq!(err.param("cause"), Some("not_found"));
    assert!(rig.srv.paths().iter().any(|p| p.contains("all_packages")));

    let script = rig.dir.join("Setup.RSC");
    std::fs::write(&script, b"/system identity set name=lab\n").unwrap();
    let view = rig.core.register_script(&script).await.unwrap();
    assert_eq!((view.name.as_str(), view.size), ("setup.rsc", 30));
    let txt = rig.dir.join("notes.txt");
    std::fs::write(&txt, b"x").unwrap();
    assert_eq!(
        rig.core.register_script(&txt).await.unwrap_err().code,
        "not_a_script"
    );

    let view = rig.core.cache_inventory(true).await.unwrap();
    assert!(view.inventory.total > 0, "packages were cached");
    assert!(view
        .inventory
        .versions
        .iter()
        .flat_map(|v| &v.items)
        .all(|i| i.integrity != ni_assets::Integrity::Unchecked));
    assert!(view.location.starts_with('/') || view.location.starts_with('~'));
    assert_eq!(view.inventory.local.len(), 1, "the script");
    rig.core.delete_cached(DeleteScope::All).await.unwrap();
    let after = rig.core.cache_inventory(false).await.unwrap();
    assert!(after.inventory.versions.is_empty());
    assert_eq!(after.inventory.local.len(), 1, "local files are kept");
    let err = rig
        .core
        .delete_cached(DeleteScope::Item { id: "../x".into() })
        .await
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("cause")),
        ("cache_failed", Some("invalid_item"))
    );

    let log = rig.dir.join("out.txt");
    rig.core.write_log(&log, "line 1\n").unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "line 1\n");
    let err = rig
        .core
        .write_log(&rig.dir.join("missing/out.txt"), "x")
        .unwrap_err();
    assert_eq!(err.code, "log_io");
}
