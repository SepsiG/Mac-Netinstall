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
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
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
    /// The rig's directory (app support, cache, profiles).
    dir: PathBuf,
    /// Removes `dir`; [`rig_test`] takes it and drops it only after the
    /// runtime is shut down.
    guard: Option<TempDir>,
}

fn http(srv: &TestServer) -> HttpConfig {
    HttpConfig {
        download_base: format!("{}/dl/", srv.base),
        upgrade_base: format!("{}/up", srv.base),
        changelog_base: format!("{}/cl", srv.base),
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
                offer_retry: Duration::from_millis(50),
                finish_timeout: Duration::from_millis(50),
                poll_interval: Duration::from_millis(10),
                finish_reply_timeout: Duration::from_millis(150),
            };
        }));
    let mut p = Profile::new("test", VersionSpec::Channel(Channel::Stable));
    p.extra_packages = vec!["wireless".into()];
    core.save_profile(&p, None).unwrap();
    core.accept_disclaimer();
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
        dir: dir.to_path_buf(),
        guard: Some(dir),
    }
}

/// Run `body` with a fresh rig on its own multi-threaded runtime, then
/// shut that runtime down and only then remove the rig's directory, and
/// check that nothing is left behind.
///
/// Engine tasks outlive the test body: e.g. the kernel fetch of an engine
/// task replaced mid-download keeps writing into the cache. Removing the
/// directory under them (a `TempDir` dropped at the end of a
/// `#[tokio::test]`) raced with those writes, failed, and left
/// `ni-assets-test-*-bridge-race-*` directories in `$TMPDIR`. Dropping the
/// runtime cancels the async tasks and waits for blocking file work, so
/// nothing writes into the directory any more when it is removed.
fn rig_test<F: Future<Output = ()>>(name: &str, body: impl FnOnce(Rig) -> F) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (dir, guard) = rt.block_on(async {
        let mut rig = rig(name).await;
        let guard = rig.guard.take().expect("a fresh rig");
        let dir = rig.dir.clone();
        body(rig).await;
        (dir, guard)
    });
    drop(rt);
    drop(guard);
    assert!(!dir.exists(), "{} was left behind", dir.display());
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

#[test]
fn start_flash_and_remember_the_choices() {
    rig_test("bridge-flash", |mut rig| async move {
        assert_eq!(
            rig.core.settings(),
            Settings {
                disclaimer_accepted_version: Some(ni_engine::DISCLAIMER_VERSION.into()),
                ..Settings::default()
            }
        );
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
    });
}

/// Nothing starts before the disclaimer is accepted; accepting is stored.
#[tokio::test]
async fn starting_needs_the_accepted_disclaimer() {
    let srv = TestServer::start().await;
    let dir = tempdir("bridge-disclaimer");
    let core = AppCore::new(Paths::under(&dir), http(&srv)).unwrap();
    let err = core.start(&request()).await.unwrap_err();
    assert_eq!(err.code, "disclaimer_not_accepted");
    assert_eq!(err.param("version"), Some(ni_engine::DISCLAIMER_VERSION));
    assert!(!core.snapshot().await.unwrap().running);
    core.accept_disclaimer();
    let stored: Settings =
        serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(
        stored.disclaimer_accepted_version.as_deref(),
        Some(ni_engine::DISCLAIMER_VERSION)
    );
    // An older accepted version is asked again.
    let old = Settings {
        disclaimer_accepted_version: Some("0".into()),
        ..Settings::default()
    };
    std::fs::write(dir.join("settings.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    let again = AppCore::new(Paths::under(&dir), http(&srv)).unwrap();
    let err = again.start(&request()).await.unwrap_err();
    assert_eq!(err.code, "disclaimer_not_accepted");
}

/// Quitting deletes the downloads (not local files) unless kept, and
/// never while the engine runs.
#[test]
fn downloads_are_purged_unless_kept() {
    rig_test("bridge-purge", |mut rig| async move {
        let cache = rig.dir.join("cache");
        let script = rig.dir.join("Keep.rsc");
        std::fs::write(&script, b"/x\n").unwrap();
        rig.core.register_script(&script).await.unwrap();
        rig.core.start(&request()).await.unwrap();
        kernels_ready(&mut rig).await;
        assert!(cache.join("7.25.1").exists());
        assert!(!rig.core.purge_downloads_if_wanted().await, "running");
        assert!(cache.join("7.25.1").exists());
        rig.core.stop().await.unwrap();
        let mut keep = rig.core.settings();
        keep.keep_downloads = true;
        rig.core.set_settings(keep.clone()).await.unwrap();
        assert!(!rig.core.purge_downloads_if_wanted().await, "kept");
        assert!(cache.join("7.25.1").exists());
        keep.keep_downloads = false;
        rig.core.set_settings(keep).await.unwrap();
        assert!(rig.core.purge_downloads_if_wanted().await);
        assert!(!cache.join("7.25.1").exists());
        assert!(cache.join("local/keep.rsc").exists());
        assert!(cache.join("channels.json").exists());
    });
}

/// The version picker's data: channels (v6 included) and the list; typed
/// versions are checked; picked packages are registered with their header.
#[test]
fn versions_and_local_packages() {
    rig_test("bridge-versions", |rig| async move {
        ni_assets::testutil::mirror_v6(&rig.srv, "6.49.23", "arm", "arm", true);
        let view = rig.core.available_versions(false).await;
        let channels: Vec<(&str, Option<&str>)> = view
            .channels
            .iter()
            .map(|c| (c.channel.as_str(), c.version.as_deref()))
            .collect();
        assert_eq!(channels[0], ("stable", Some("7.25.1")));
        assert_eq!(channels[4], ("v6-long-term", Some("6.49.23")));
        // No changelog pages on the fake mirror: the built-in list.
        assert_eq!(view.source, ni_assets::ListSource::Seed);
        assert!(view.error.is_some());
        assert_eq!(view.versions[0].version.as_str(), "7.25.1");
        assert!(rig.core.check_version_exists("6.49.23").await.unwrap());
        assert!(!rig.core.check_version_exists("6.49.22").await.unwrap());
        let err = rig.core.check_version_exists("5.26").await.unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("cause")),
            ("version_unavailable", Some("version_too_old"))
        );
        let file = rig.dir.join("Mine.npk");
        std::fs::write(
            &file,
            ni_assets::testutil::npk_package_version("system", "arm", (7, 24, 6), 600, 3),
        )
        .unwrap();
        let got = rig.core.register_packages(&[file]).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            (
                got[0].name.as_str(),
                got[0].package.as_str(),
                got[0].arch.as_str(),
                got[0].version.as_deref()
            ),
            ("mine.npk", "system", "arm", Some("7.24.6"))
        );
        let err = rig
            .core
            .register_packages(&[rig.dir.join("x.txt")])
            .await
            .unwrap_err();
        assert_eq!(err.code, "not_a_package");
        let bad = rig.dir.join("bad.npk");
        std::fs::write(&bad, b"nope").unwrap();
        let err = rig.core.register_packages(&[bad]).await.unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("cause")),
            ("package_failed", Some("not_npk"))
        );
        // A complete NPK without a readable header is never registered.
        let headless = rig.dir.join("headless.npk");
        std::fs::write(&headless, ni_assets::testutil::npk(300, 1)).unwrap();
        let err = rig.core.register_packages(&[headless]).await.unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("cause")),
            ("package_failed", Some("npk_malformed"))
        );
        assert!(rig
            .core
            .engine()
            .assets()
            .local_file("headless.npk")
            .await
            .is_err());
        // Another file of the same name (any case) is refused, not
        // overwritten; the same file again is fine.
        let again = rig.dir.join("sub");
        std::fs::create_dir_all(&again).unwrap();
        let same = again.join("MINE.npk");
        std::fs::write(
            &same,
            ni_assets::testutil::npk_package_version("system", "arm", (7, 24, 6), 600, 3),
        )
        .unwrap();
        assert_eq!(
            rig.core
                .register_packages(std::slice::from_ref(&same))
                .await
                .unwrap()[0]
                .name,
            "mine.npk"
        );
        std::fs::write(
            &same,
            ni_assets::testutil::npk_package_version("system", "arm", (7, 25, 1), 600, 4),
        )
        .unwrap();
        let err = rig.core.register_packages(&[same]).await.unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("name")),
            ("package_exists", Some("mine.npk"))
        );
        let kept = rig
            .core
            .engine()
            .assets()
            .local_file("mine.npk")
            .await
            .unwrap();
        assert_eq!(
            ni_assets::npk::package_version(&kept).unwrap().as_str(),
            "7.24.6"
        );
    });
}

#[test]
fn the_quick_interface_check_makes_no_requests() {
    rig_test("bridge-local-check", |rig| async move {
        let before = rig.srv.paths().len();
        let report = rig.core.check_interface(&request()).await.unwrap();
        assert!(report.network.is_some());
        assert_eq!(rig.srv.paths().len(), before, "no HTTP for the quick check");
        // The full preflight does look at the profile's assets.
        rig.core.preflight(&request()).await.unwrap();
        assert!(rig.srv.paths().len() > before);
    });
}

#[test]
fn errors_are_codes_without_paths() {
    rig_test("bridge-errors", |rig| async move {
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
    });
}

#[test]
fn runtime_controls_reach_the_engine_and_are_remembered() {
    rig_test("bridge-controls", |mut rig| async move {
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
    });
}

#[test]
fn stale_settings_do_not_overwrite_remembered_choices() {
    rig_test("bridge-stale", |mut rig| async move {
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
    });
}

#[test]
fn concurrent_preflight_and_start_do_not_orphan_the_engine() {
    rig_test("bridge-race", |rig| async move {
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
    });
}

#[test]
fn a_new_download_timeout_replaces_the_idle_engine() {
    rig_test("bridge-timeout", |mut rig| async move {
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
    });
}

#[test]
fn window_and_dialog_sizes_are_remembered() {
    use mac_netinstall_app::settings::WindowGeometry;
    rig_test("bridge-sizes", |rig| async move {
        rig.core.set_dialog_size("profile", 700, 520).unwrap();
        rig.core.set_dialog_size("confirm", 420, 200).unwrap();
        for (id, w, h) in [
            ("profile", 50, 520),
            ("Bad Id", 700, 520),
            ("x", 700, 100_000),
        ] {
            let err = rig.core.set_dialog_size(id, w, h).unwrap_err();
            assert_eq!(
                (err.code.as_str(), err.param("field")),
                ("invalid_setting", Some("dialog_sizes")),
                "{id} {w}x{h}"
            );
        }
        let g = WindowGeometry {
            x: 30,
            y: 40,
            width: 1200,
            height: 800,
        };
        rig.core.remember_window(g);
        rig.core.remember_window(WindowGeometry { width: 10, ..g }); // ignored
        let s = rig.core.settings();
        assert_eq!(s.window, Some(g));
        assert_eq!(s.dialog_sizes["profile"].width, 700);
        assert_eq!(s.dialog_sizes.len(), 2);
        // Stored: a new core reads them back; the settings dialog keeps them.
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(rig.dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(stored["dialog_sizes"]["confirm"]["height"], 200);
        assert_eq!(stored["window"]["width"], 1200);
        let saved = rig
            .core
            .set_settings(mac_netinstall_app::settings::Settings::default())
            .await
            .unwrap();
        assert_eq!(saved.window, Some(g));
        assert_eq!(saved.dialog_sizes.len(), 2);
    });
}

#[test]
fn profiles_are_created_renamed_and_deleted() {
    rig_test("bridge-profiles", |rig| async move {
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
    });
}

#[test]
fn packages_scripts_cache_and_log() {
    rig_test("bridge-assets", |rig| async move {
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

        let key = rig.dir.join("device.key");
        std::fs::write(
            &key,
            b"-----BEGIN KEY-----\r\nAAAA\nBBBB==\n-----END KEY-----\n",
        )
        .unwrap();
        let view = rig.core.read_license_key(&key).unwrap();
        assert_eq!(view.key, "-----BEGIN KEY-----AAAABBBB==-----END KEY-----");
        let code = |p: &std::path::Path| rig.core.read_license_key(p).unwrap_err();
        let err = code(&rig.dir.join("missing.key"));
        assert_eq!(
            (err.code.as_str(), err.param("reason")),
            ("key_file_failed", Some("unreadable"))
        );
        assert!(err.param("path").is_none(), "no path for the window");
        std::fs::write(&key, vec![b'k'; 64 * 1024 + 1]).unwrap();
        assert_eq!(code(&key).param("reason"), Some("too_large"));
        std::fs::write(&key, b"\n\n").unwrap();
        assert_eq!(code(&key).code, "invalid_license_key");
        // A FIFO (or any non-regular file) is refused without blocking on it.
        let fifo = rig.dir.join("pipe.key");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let core = rig.core.clone();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(core.read_license_key(&path));
        });
        let err = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a FIFO must not block the read")
            .unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("reason")),
            ("key_file_failed", Some("not_a_file"))
        );
        std::fs::create_dir(rig.dir.join("dir.key")).unwrap();
        assert_eq!(
            code(&rig.dir.join("dir.key")).param("reason"),
            Some("not_a_file")
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
    });
}
