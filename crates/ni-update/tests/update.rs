//! The update check and download against a local HTTP server standing in
//! for the GitHub API and the release downloads (no real network).

use ni_assets::testutil::{tempdir, Reply, TestServer};
use ni_update::{is_newer, SemVer, UpdateConfig, UpdateError, Updater};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Bytes done and the total, as reported.
type Progress = (u64, Option<u64>);

const API: &str = "/repos/SepsiG/Mac-Netinstall/releases/latest";

fn cfg(srv: &TestServer) -> UpdateConfig {
    UpdateConfig {
        api_url: format!("{}{API}", srv.base),
        download_prefix: format!("{}/SepsiG/Mac-Netinstall/releases/download/", srv.base),
        read_timeout: Duration::from_secs(2),
        ..UpdateConfig::default()
    }
}

fn updater(srv: &TestServer) -> Updater {
    Updater::new(cfg(srv)).unwrap()
}

fn dmg_name(v: &str) -> String {
    format!("Mac-Netinstall_{v}_aarch64.dmg")
}

fn dl_path(v: &str, file: &str) -> String {
    format!("/SepsiG/Mac-Netinstall/releases/download/v{v}/{file}")
}

/// A `releases/latest` answer for `tag`, with both assets on `srv`.
fn release_json(srv: &TestServer, tag: &str) -> serde_json::Value {
    let v = tag.trim_start_matches('v');
    let dmg = dmg_name(v);
    serde_json::json!({
        "tag_name": tag,
        "name": format!("Mac Netinstall {v}"),
        "html_url": format!("https://github.com/SepsiG/Mac-Netinstall/releases/tag/{tag}"),
        "body": "## Added\n- Update check\n<script>alert(1)</script>",
        "published_at": "2026-11-20T10:00:00Z",
        "draft": false,
        "prerelease": false,
        "assets": [
            {
                "name": dmg,
                "size": 6,
                "browser_download_url": format!("{}{}", srv.base, dl_path(v, &dmg)),
            },
            {
                "name": format!("{dmg}.sha256"),
                "size": 100,
                "browser_download_url": format!("{}{}", srv.base, dl_path(v, &format!("{dmg}.sha256"))),
            },
        ],
    })
}

fn serve_json(srv: &TestServer, json: &serde_json::Value) {
    srv.route(API, Reply::Body(serde_json::to_vec(json).unwrap()));
}

fn sha_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Serve the DMG `body` and a checksum file saying `sum` for `v`.
fn serve_assets(srv: &TestServer, v: &str, body: &[u8], sum: &str) {
    let dmg = dmg_name(v);
    srv.route(&dl_path(v, &dmg), Reply::Body(body.to_vec()));
    srv.route(
        &dl_path(v, &format!("{dmg}.sha256")),
        Reply::Body(sum.as_bytes().to_vec()),
    );
}

/// A fresh staging directory (the app's cache) for one download.
fn staging() -> ni_assets::testutil::TempDir {
    tempdir("update-staging")
}

/// The `com.apple.quarantine` attribute of `path`, if any.
fn quarantine(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = c"com.apple.quarantine";
    let mut buf = vec![0u8; 512];
    // SAFETY: valid C strings and a buffer of the given length.
    let n = unsafe {
        nix::libc::getxattr(
            c.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
            0,
        )
    };
    (n >= 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn versions_compare_numerically_and_prereleases_are_not_versions() {
    assert_eq!(SemVer::parse("v0.1.3"), Some(SemVer(0, 1, 3)));
    assert_eq!(SemVer::parse("1.10.0"), Some(SemVer(1, 10, 0)));
    for bad in [
        "v0.1.3-rc1",
        "0.1",
        "0.1.3.4",
        "v",
        "",
        "x.y.z",
        "0.1.3 ",
        "+1.0.0",
    ] {
        assert_eq!(SemVer::parse(bad), None, "{bad:?}");
    }
    assert!(is_newer("0.1.10", "0.1.9"));
    assert!(is_newer("1.0.0", "0.9.9"));
    assert!(!is_newer("0.1.2", "0.1.2"), "equal");
    assert!(!is_newer("0.1.1", "0.1.2"), "older");
    assert!(!is_newer("0.2.0-rc1", "0.1.2"), "prerelease");
    assert_eq!(SemVer(0, 1, 2).to_string(), "0.1.2");
}

#[tokio::test]
async fn the_latest_release_is_read_with_its_assets() {
    let srv = TestServer::start().await;
    serve_json(&srv, &release_json(&srv, "v0.1.3"));
    let r = updater(&srv).latest().await.unwrap().expect("a release");
    assert_eq!(r.version, "0.1.3");
    assert_eq!(r.tag, "v0.1.3");
    assert_eq!(
        r.page_url,
        "https://github.com/SepsiG/Mac-Netinstall/releases/tag/v0.1.3"
    );
    assert!(r.notes.starts_with("## Added"));
    assert_eq!(r.published_at.as_deref(), Some("2026-11-20T10:00:00Z"));
    assert_eq!(r.dmg.name, "Mac-Netinstall_0.1.3_aarch64.dmg");
    assert_eq!(r.checksum.name, "Mac-Netinstall_0.1.3_aarch64.dmg.sha256");
    let head = srv.requests().remove(0).to_ascii_lowercase();
    assert!(head.starts_with(&format!("get {} ", API.to_ascii_lowercase())));
    assert!(
        head.contains("accept: application/vnd.github+json"),
        "{head}"
    );
    assert!(head.contains("user-agent: mac-netinstall/0."), "{head}");
    assert!(!head.contains("authorization"), "{head}");
    assert!(!head.contains("cookie"), "{head}");
}

#[tokio::test]
async fn newer_equal_and_older_releases() {
    let srv = TestServer::start().await;
    let u = updater(&srv);
    for (tag, newer) in [("v0.1.3", true), ("v0.1.2", false), ("v0.1.1", false)] {
        serve_json(&srv, &release_json(&srv, tag));
        let r = u.latest().await.unwrap().unwrap();
        assert_eq!(is_newer(&r.version, "0.1.2"), newer, "{tag}");
    }
}

#[tokio::test]
async fn prereleases_and_drafts_are_ignored() {
    let srv = TestServer::start().await;
    let u = updater(&srv);
    serve_json(&srv, &release_json(&srv, "v0.2.0-rc1"));
    assert_eq!(u.latest().await.unwrap(), None);
    let mut j = release_json(&srv, "v0.2.0");
    j["prerelease"] = true.into();
    serve_json(&srv, &j);
    assert_eq!(u.latest().await.unwrap(), None);
    let mut j = release_json(&srv, "v0.2.0");
    j["draft"] = true.into();
    serve_json(&srv, &j);
    assert_eq!(u.latest().await.unwrap(), None);
}

#[tokio::test]
async fn malformed_answers_and_missing_assets_are_errors() {
    let srv = TestServer::start().await;
    let u = updater(&srv);
    srv.route(API, Reply::Body(b"{not json".to_vec()));
    assert_eq!(u.latest().await.unwrap_err().code(), "bad_response");
    serve_json(
        &srv,
        &serde_json::json!({"tag_name": "banana", "assets": []}),
    );
    assert_eq!(u.latest().await.unwrap_err().code(), "bad_response");
    let mut j = release_json(&srv, "v0.1.3");
    j["assets"].as_array_mut().unwrap().remove(1);
    serve_json(&srv, &j);
    assert_eq!(u.latest().await.unwrap_err().code(), "missing_asset");
    let mut j = release_json(&srv, "v0.1.3");
    j["assets"].as_array_mut().unwrap().remove(0);
    serve_json(&srv, &j);
    assert_eq!(u.latest().await.unwrap_err().code(), "missing_asset");
    // Asset URLs are pinned: exactly `<prefix><tag>/<name>`.
    for bad in [
        format!("{}{}?x=1", srv.base, dl_path("0.1.3", &dmg_name("0.1.3"))),
        format!("{}{}#x", srv.base, dl_path("0.1.3", &dmg_name("0.1.3"))),
        format!(
            "{}{}",
            srv.base,
            dl_path("0.1.3", &format!("extra/{}", dmg_name("0.1.3")))
        ),
        format!("{}{}", srv.base, dl_path("0.1.4", &dmg_name("0.1.3"))),
    ] {
        let mut j = release_json(&srv, "v0.1.3");
        j["assets"][0]["browser_download_url"] = bad.clone().into();
        serve_json(&srv, &j);
        assert_eq!(u.latest().await.unwrap_err().code(), "url_refused", "{bad}");
    }
    // An asset somewhere else than the project's release downloads.
    let mut j = release_json(&srv, "v0.1.3");
    j["assets"][0]["browser_download_url"] = "https://evil.example/x.dmg".into();
    serve_json(&srv, &j);
    assert_eq!(u.latest().await.unwrap_err().code(), "url_refused");
    // A release page elsewhere is replaced by the constant one.
    let mut j = release_json(&srv, "v0.1.3");
    j["html_url"] = "https://evil.example/notes".into();
    serve_json(&srv, &j);
    let r = u.latest().await.unwrap().unwrap();
    assert_eq!(
        r.page_url,
        "https://github.com/SepsiG/Mac-Netinstall/releases/tag/v0.1.3"
    );
}

#[tokio::test]
async fn rate_limits_and_other_statuses() {
    let srv = TestServer::start().await;
    let u = updater(&srv);
    for (status, code) in [
        (403, "rate_limited"),
        (429, "rate_limited"),
        (404, "no_release"),
        (500, "http_status"),
    ] {
        srv.route(API, Reply::Status(status));
        let err = u.latest().await.unwrap_err();
        assert_eq!(err.code(), code, "{status}");
    }
    assert!(matches!(
        UpdateError::RateLimited { status: 403 },
        UpdateError::RateLimited { .. }
    ));
}

#[tokio::test]
async fn release_notes_are_capped() {
    let srv = TestServer::start().await;
    let mut j = release_json(&srv, "v0.1.3");
    j["body"] = "é".repeat(ni_update::MAX_NOTES_CHARS + 50).into();
    serve_json(&srv, &j);
    let r = updater(&srv).latest().await.unwrap().unwrap();
    assert_eq!(r.notes.chars().count(), ni_update::MAX_NOTES_CHARS + 1);
    assert!(r.notes.ends_with('…'));
}

#[tokio::test]
async fn a_verified_download_lands_in_the_folder_without_overwriting() {
    let srv = TestServer::start().await;
    serve_json(&srv, &release_json(&srv, "v0.1.3"));
    let body = vec![7u8; 700_000];
    let dmg = dmg_name("0.1.3");
    serve_assets(
        &srv,
        "0.1.3",
        &body,
        &format!("{}  {dmg}\n", sha_hex(&body)),
    );
    let u = updater(&srv);
    let r = u.latest().await.unwrap().unwrap();
    let dir = tempdir("update-dl");
    let st = staging();
    std::fs::write(dir.join(&dmg), b"an older download").unwrap();
    let progress: Arc<Mutex<Vec<Progress>>> = Arc::default();
    let p = progress.clone();
    let path = u
        .download(&r, &st, &dir, &move |done, total| {
            p.lock().unwrap().push((done, total))
        })
        .await
        .unwrap();
    assert_eq!(
        path.file_name().unwrap(),
        "Mac-Netinstall_0.1.3_aarch64 (1).dmg"
    );
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(
        std::fs::read(dir.join(&dmg)).unwrap(),
        b"an older download",
        "untouched"
    );
    assert_eq!(
        files(&dir),
        [
            "Mac-Netinstall_0.1.3_aarch64 (1).dmg".to_string(),
            dmg.clone()
        ],
        "no partial or checksum file left"
    );
    assert!(files(&st).is_empty(), "the partial file is gone");
    // Quarantined like a browser download, so Gatekeeper checks the app.
    let q = quarantine(&path).expect("quarantined");
    let parts: Vec<&str> = q.split(';').collect();
    assert_eq!(parts.len(), 4, "{q}");
    assert_eq!(parts[0], "0083");
    let secs = u64::from_str_radix(parts[1], 16).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now.abs_diff(secs) < 600, "{q}");
    assert_eq!(parts[2], "Mac Netinstall");
    assert_eq!(parts[3].len(), 36, "{q}");
    assert_eq!(parts[3], parts[3].to_ascii_uppercase(), "{q}");
    assert_eq!(parts[3].matches('-').count(), 4, "{q}");
    let progress = progress.lock().unwrap().clone();
    assert_eq!(progress.last(), Some(&(700_000, Some(700_000))));
    assert!(progress.len() >= 2, "{progress:?}");
    // `*name` (binary mode, as `shasum -b` writes) is accepted too.
    let again = u.download(&r, &staging(), &dir, &|_, _| {}).await.unwrap();
    assert_eq!(
        again.file_name().unwrap(),
        "Mac-Netinstall_0.1.3_aarch64 (2).dmg"
    );
    serve_assets(&srv, "0.1.3", &body, &format!("{} *{dmg}", sha_hex(&body)));
    u.download(&r, &staging(), &dir, &|_, _| {}).await.unwrap();
}

#[tokio::test]
async fn a_checksum_mismatch_leaves_nothing_behind() {
    let srv = TestServer::start().await;
    serve_json(&srv, &release_json(&srv, "v0.1.3"));
    let dmg = dmg_name("0.1.3");
    let u = updater(&srv);
    let r = u.latest().await.unwrap().unwrap();
    let dir = tempdir("update-mismatch");
    serve_assets(
        &srv,
        "0.1.3",
        b"tampered",
        &format!("{}  {dmg}\n", sha_hex(b"original")),
    );
    let err = u
        .download(&r, &staging(), &dir, &|_, _| {})
        .await
        .unwrap_err();
    assert_eq!(err.code(), "checksum_mismatch");
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    // A checksum file for another file, or not a checksum at all.
    for sum in [
        format!("{}  Other_0.1.3.dmg\n", sha_hex(b"tampered")),
        "not a checksum".to_string(),
        format!("{}\n", sha_hex(b"tampered")),
    ] {
        serve_assets(&srv, "0.1.3", b"tampered", &sum);
        let err = u
            .download(&r, &staging(), &dir, &|_, _| {})
            .await
            .unwrap_err();
        assert_eq!(err.code(), "checksum_invalid", "{sum:?}");
        assert!(files(&dir).is_empty());
    }
}

#[tokio::test]
async fn redirects_only_to_allowed_hosts() {
    let srv = TestServer::start().await;
    let port = srv.base.rsplit(':').next().unwrap().to_string();
    serve_json(&srv, &release_json(&srv, "v0.1.3"));
    let u = updater(&srv);
    let r = u.latest().await.unwrap().unwrap();
    let dmg = dmg_name("0.1.3");
    let body = b"dmg bytes".to_vec();
    let sum = format!("{}  {dmg}\n", sha_hex(&body));
    serve_assets(&srv, "0.1.3", &body, &sum);
    // To the same server: followed.
    srv.route("/blob", Reply::Body(body.clone()));
    srv.route(&dl_path("0.1.3", &dmg), Reply::Redirect("/blob".into()));
    let dir = tempdir("update-redirect");
    u.download(&r, &staging(), &dir, &|_, _| {}).await.unwrap();
    // To another host (`localhost` is not `127.0.0.1`), or plain http
    // elsewhere: refused.
    srv.route(
        &dl_path("0.1.3", &dmg),
        Reply::Redirect(format!("http://localhost:{port}/blob")),
    );
    let dir = tempdir("update-redirect-refused");
    let err = u
        .download(&r, &staging(), &dir, &|_, _| {})
        .await
        .unwrap_err();
    assert_eq!(err.code(), "redirect_refused");
    assert!(files(&dir).is_empty());
    assert!(ni_update::redirect_allowed(
        &"https://github.com/a".parse().unwrap(),
        &"https://objects.githubusercontent.com/b".parse().unwrap()
    ));
    assert!(ni_update::redirect_allowed(
        &"https://github.com/a".parse().unwrap(),
        &"https://release-assets.githubusercontent.com/b"
            .parse()
            .unwrap()
    ));
    assert!(!ni_update::redirect_allowed(
        &"https://github.com/a".parse().unwrap(),
        &"http://objects.githubusercontent.com/b".parse().unwrap()
    ));
    assert!(!ni_update::redirect_allowed(
        &"https://github.com/a".parse().unwrap(),
        &"https://evil.example/b".parse().unwrap()
    ));
}

#[tokio::test]
async fn downloads_are_capped() {
    let srv = TestServer::start().await;
    serve_json(&srv, &release_json(&srv, "v0.1.3"));
    let dmg = dmg_name("0.1.3");
    let body = vec![1u8; 5000];
    serve_assets(&srv, "0.1.3", &body, &format!("{}  {dmg}", sha_hex(&body)));
    let u = Updater::new(UpdateConfig {
        max_download_bytes: 4000,
        ..cfg(&srv)
    })
    .unwrap();
    let r = u.latest().await.unwrap().unwrap();
    let dir = tempdir("update-cap");
    let err = u
        .download(&r, &staging(), &dir, &|_, _| {})
        .await
        .unwrap_err();
    assert_eq!(err.code(), "too_large");
    assert!(files(&dir).is_empty());
    assert_eq!(UpdateConfig::default().max_download_bytes, 200 << 20);
}

#[test]
fn production_endpoints_are_constant() {
    let c = UpdateConfig::default();
    assert_eq!(
        c.api_url,
        "https://api.github.com/repos/SepsiG/Mac-Netinstall/releases/latest"
    );
    assert_eq!(
        c.download_prefix,
        "https://github.com/SepsiG/Mac-Netinstall/releases/download/"
    );
    assert!(ni_update::release_page_allowed(
        "https://github.com/SepsiG/Mac-Netinstall/releases/tag/v0.1.3"
    ));
    for bad in [
        "https://github.com/SepsiG/Mac-Netinstall/releases/../../evil",
        "https://github.com/SepsiG/Mac-Netinstall/releasesX",
        "https://github.com.evil.example/SepsiG/Mac-Netinstall/releases/",
        "http://github.com/SepsiG/Mac-Netinstall/releases/tag/v1",
        "https://github.com/SepsiG/Mac-Netinstall/releases/tag/v1 --args",
        "javascript:alert(1)",
    ] {
        assert!(!ni_update::release_page_allowed(bad), "{bad}");
    }
}

#[test]
fn checks_are_due_once_a_day() {
    let day = 24 * 60 * 60;
    assert!(ni_update::check_due(None, 1000));
    assert!(!ni_update::check_due(Some(1000), 1000 + day - 1));
    assert!(ni_update::check_due(Some(1000), 1000 + day));
    assert!(
        ni_update::check_due(Some(5000), 1000),
        "the clock went back"
    );
}

#[test]
fn stale_partial_downloads_are_swept() {
    let dir = tempdir("update-sweep");
    std::fs::write(dir.join(".Mac-Netinstall_0.1.3_aarch64.dmg.1-0.part"), b"x").unwrap();
    std::fs::write(dir.join("keep.txt"), b"x").unwrap();
    assert_eq!(ni_update::sweep_partials(&dir, Duration::ZERO), 1);
    assert_eq!(files(&dir), ["keep.txt"]);
    // Young ones (another download running) stay.
    std::fs::write(dir.join(".a.dmg.2-0.part"), b"x").unwrap();
    assert_eq!(
        ni_update::sweep_partials(&dir, Duration::from_secs(3600)),
        0
    );
    // A missing directory is nothing to sweep.
    assert_eq!(
        ni_update::sweep_partials(&dir.join("none"), Duration::ZERO),
        0
    );
}

#[tokio::test]
async fn every_request_answers_with_the_release_published_now() {
    // The app's "Download" relies on this: no caching in the client.
    let srv = TestServer::start().await;
    let u = updater(&srv);
    serve_json(&srv, &release_json(&srv, "v0.1.3"));
    assert_eq!(u.latest().await.unwrap().unwrap().version, "0.1.3");
    serve_json(&srv, &release_json(&srv, "v0.1.4"));
    let r = u.latest().await.unwrap().unwrap();
    assert_eq!(r.version, "0.1.4");
    assert!(r
        .dmg
        .url
        .ends_with("/v0.1.4/Mac-Netinstall_0.1.4_aarch64.dmg"));
}
