//! The update check through `AppCore`, against a local server standing in
//! for GitHub (no real network): throttling, the setting, skipped
//! versions, rate limits and the verified download.

use mac_netinstall_app::core::{AppCore, Paths};
use ni_assets::testutil::{tempdir, Reply, TestServer};
use ni_assets::HttpConfig;
use ni_update::UpdateConfig;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};

const API: &str = "/repos/SepsiG/Mac-Netinstall/releases/latest";

fn dl_path(v: &str, file: &str) -> String {
    format!("/SepsiG/Mac-Netinstall/releases/download/v{v}/{file}")
}

fn dmg(v: &str) -> String {
    format!("Mac-Netinstall_{v}_aarch64.dmg")
}

/// Publish `v` (with a DMG `body` and a checksum file `sum`; the real
/// checksum if `None`) as the latest release.
fn publish(srv: &TestServer, v: &str, body: &[u8], sum: Option<String>) {
    let name = dmg(v);
    let json = serde_json::json!({
        "tag_name": format!("v{v}"),
        "html_url": format!("https://github.com/SepsiG/Mac-Netinstall/releases/tag/v{v}"),
        "body": "## Fixed\n- things",
        "published_at": "2026-11-20T10:00:00Z",
        "assets": [
            {"name": name, "size": body.len(),
             "browser_download_url": format!("{}{}", srv.base, dl_path(v, &name))},
            {"name": format!("{name}.sha256"), "size": 90,
             "browser_download_url": format!("{}{}", srv.base, dl_path(v, &format!("{name}.sha256")))},
        ],
    });
    srv.route(API, Reply::Body(serde_json::to_vec(&json).unwrap()));
    let hex: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let sum = sum.unwrap_or_else(|| format!("{hex}  {name}\n"));
    srv.route(&dl_path(v, &name), Reply::Body(body.to_vec()));
    srv.route(
        &dl_path(v, &format!("{name}.sha256")),
        Reply::Body(sum.into_bytes()),
    );
}

fn core(srv: &TestServer, dir: &Path) -> AppCore {
    let http = HttpConfig {
        download_base: format!("{}/dl/", srv.base),
        upgrade_base: format!("{}/up", srv.base),
        changelog_base: format!("{}/cl", srv.base),
        ..HttpConfig::default()
    };
    AppCore::new(Paths::under(dir), http)
        .unwrap()
        .with_update_config(UpdateConfig {
            api_url: format!("{}{API}", srv.base),
            download_prefix: format!("{}/SepsiG/Mac-Netinstall/releases/download/", srv.base),
            ..UpdateConfig::default()
        })
}

fn api_requests(srv: &TestServer) -> usize {
    srv.paths().iter().filter(|p| p.ends_with(API)).count()
}

#[tokio::test]
async fn automatic_checks_run_once_a_day_and_follow_the_setting() {
    let srv = TestServer::start().await;
    publish(&srv, "99.0.0", b"dmg", None);
    let dir = tempdir("app-update-auto");
    let c = core(&srv, &dir);
    let before = c.update_status();
    assert_eq!(before.current, env!("CARGO_PKG_VERSION"));
    assert!(!before.available && before.latest.is_none());
    assert!(before.auto);
    let v = c.check_for_updates(false).await.unwrap();
    assert!(v.available);
    let latest = v.latest.as_ref().unwrap();
    assert_eq!(latest.version, "99.0.0");
    assert_eq!(latest.notes, "## Fixed\n- things");
    assert_eq!(latest.size, 3);
    assert!(v.last_checked_unix.is_some());
    // The window never gets a URL.
    let json = serde_json::to_string(&v).unwrap();
    assert!(!json.contains("http"), "{json}");
    // Throttled: answered from what is remembered, also after a restart.
    let again = c.check_for_updates(false).await.unwrap();
    assert!(again.available);
    assert_eq!(api_requests(&srv), 1);
    let restarted = core(&srv, &dir);
    assert!(restarted.update_status().available);
    assert!(restarted.check_for_updates(false).await.unwrap().available);
    assert_eq!(api_requests(&srv), 1);
    // A manual check always asks.
    restarted.check_for_updates(true).await.unwrap();
    assert_eq!(api_requests(&srv), 2);
    // A day later, an automatic check asks again, unless turned off.
    let state = dir.join("update-check.json");
    let mut j: serde_json::Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    j["last_check_unix"] = 1_000.into();
    std::fs::write(&state, j.to_string()).unwrap();
    let c = core(&srv, &dir);
    let mut s = c.settings();
    s.check_updates = false;
    c.set_settings(s).await.unwrap();
    let off = c.check_for_updates(false).await.unwrap();
    assert!(!off.auto);
    assert_eq!(api_requests(&srv), 2);
    let mut s = c.settings();
    s.check_updates = true;
    c.set_settings(s).await.unwrap();
    c.check_for_updates(false).await.unwrap();
    assert_eq!(api_requests(&srv), 3);
}

#[tokio::test]
async fn older_equal_and_skipped_releases_are_not_announced() {
    let srv = TestServer::start().await;
    let dir = tempdir("app-update-skip");
    let c = core(&srv, &dir);
    publish(&srv, "0.0.1", b"dmg", None);
    assert!(!c.check_for_updates(true).await.unwrap().available);
    publish(&srv, env!("CARGO_PKG_VERSION"), b"dmg", None);
    assert!(!c.check_for_updates(true).await.unwrap().available);
    publish(&srv, "99.0.0", b"dmg", None);
    assert!(c.check_for_updates(true).await.unwrap().available);
    let v = c.skip_update("99.0.0").unwrap();
    assert!(!v.available);
    assert_eq!(v.skipped.as_deref(), Some("99.0.0"));
    assert_eq!(
        c.settings().skipped_update_version.as_deref(),
        Some("99.0.0")
    );
    assert!(!c.check_for_updates(true).await.unwrap().available);
    // A newer one than the skipped one is announced again.
    publish(&srv, "99.0.1", b"dmg", None);
    assert!(c.check_for_updates(true).await.unwrap().available);
    let v = c.clear_skipped_update().unwrap();
    assert_eq!(v.skipped, None);
    let err = c.skip_update("v1; rm -rf").unwrap_err();
    assert_eq!(err.code, "invalid_setting");
    // A prerelease as the latest: nothing to announce.
    srv.route(
        API,
        Reply::Body(br#"{"tag_name": "v100.0.0-rc1", "assets": []}"#.to_vec()),
    );
    assert!(!c.check_for_updates(true).await.unwrap().available);
}

#[tokio::test]
async fn failures_are_quiet_for_automatic_checks_and_codes_for_manual_ones() {
    let srv = TestServer::start().await;
    let dir = tempdir("app-update-fail");
    let c = core(&srv, &dir);
    srv.route(API, Reply::Status(403));
    let v = c.check_for_updates(false).await.unwrap();
    assert!(!v.available);
    // Rate limited: not asked again today.
    c.check_for_updates(false).await.unwrap();
    assert_eq!(api_requests(&srv), 1);
    let err = c.check_for_updates(true).await.unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("problem")),
        ("update_check_failed", Some("rate_limited"))
    );
    srv.route(API, Reply::Body(b"<html>".to_vec()));
    let err = c.check_for_updates(true).await.unwrap_err();
    assert_eq!(err.param("problem"), Some("bad_response"));
    // Errors carry no URLs.
    assert!(!format!("{err:?}").contains("http"), "{err:?}");
}

#[tokio::test]
async fn the_download_is_verified_and_lands_in_downloads() {
    let srv = TestServer::start().await;
    let dir = tempdir("app-update-dl");
    let c = core(&srv, &dir);
    let err = c.download_update(&|_, _| {}).await.unwrap_err();
    assert_eq!(err.code, "update_not_available");
    let body = vec![3u8; 600_000];
    publish(&srv, "99.0.0", &body, None);
    c.check_for_updates(true).await.unwrap();
    let seen: Arc<Mutex<Vec<u64>>> = Arc::default();
    let s = seen.clone();
    let path = c
        .download_update(&move |done, _| s.lock().unwrap().push(done))
        .await
        .unwrap();
    assert_eq!(path, dir.join("downloads").join(dmg("99.0.0")));
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(seen.lock().unwrap().last(), Some(&600_000));
    // Tampered: deleted, and a code.
    publish(
        &srv,
        "99.0.0",
        &body,
        Some(format!("{}  {}\n", "0".repeat(64), dmg("99.0.0"))),
    );
    let err = c.download_update(&|_, _| {}).await.unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.param("problem")),
        ("update_download_failed", Some("checksum_mismatch"))
    );
    let names: Vec<String> = std::fs::read_dir(dir.join("downloads"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [dmg("99.0.0")], "only the first, good download");
}

#[tokio::test]
async fn the_release_page_is_one_of_the_projects() {
    let srv = TestServer::start().await;
    let dir = tempdir("app-update-page");
    let c = core(&srv, &dir);
    assert_eq!(
        c.release_page_url(),
        "https://github.com/SepsiG/Mac-Netinstall/releases/latest"
    );
    publish(&srv, "99.0.0", b"dmg", None);
    c.check_for_updates(true).await.unwrap();
    assert_eq!(
        c.release_page_url(),
        "https://github.com/SepsiG/Mac-Netinstall/releases/tag/v99.0.0"
    );
    // A tampered state file cannot point the browser elsewhere.
    let state = dir.join("update-check.json");
    let text = std::fs::read_to_string(&state).unwrap();
    std::fs::write(
        &state,
        text.replace(
            "https://github.com/SepsiG/Mac-Netinstall/releases/tag/v99.0.0",
            "https://evil.example/x",
        ),
    )
    .unwrap();
    let c = core(&srv, &dir);
    assert_eq!(
        c.release_page_url(),
        "https://github.com/SepsiG/Mac-Netinstall/releases/latest"
    );
}
