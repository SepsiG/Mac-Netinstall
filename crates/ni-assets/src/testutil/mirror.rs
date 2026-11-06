//! A fake MikroTik mirror for [`Assets`] tests.

use super::{kernel_container, npk, synthetic_kernels, tar_gz, zip, Reply, TestServer};
use crate::events::AssetEvent;
use crate::http::HttpConfig;
use crate::npk::sha256_hex;
use crate::version::Version;
use crate::Assets;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const V: &str = "7.25.1";

/// A tarball like `netinstall-<ver>.tar.gz` with a synthetic `netinstall-cli`.
pub fn netinstall_tarball() -> Vec<u8> {
    tar_gz(&[("netinstall-cli", &kernel_container(&synthetic_kernels()))])
}

pub fn v() -> Version {
    Version::parse(V).unwrap()
}

pub fn sum_file(data: &[u8], name: &str) -> Reply {
    Reply::Body(format!("{}  {name}\n", sha256_hex(data)).into_bytes())
}

/// Serve a fake MikroTik mirror: tarball (with checksum), arm64 main
/// package (with checksum), `wireless` (without checksum) and the
/// arm64 `all_packages` zip.
pub fn mirror(srv: &TestServer) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let tarball = netinstall_tarball();
    let main = npk(5000, 0xaa);
    let wireless = npk(3000, 0xbb);
    let d = format!("/dl/{V}");
    srv.route(
        "/up/NEWESTa7.stable",
        Reply::Body(b"7.25.1 1793623193\n".to_vec()),
    );
    srv.route(
        &format!("{d}/netinstall-{V}.tar.gz"),
        Reply::Body(tarball.clone()),
    );
    srv.route(
        &format!("{d}/netinstall-{V}.tar.gz.sha256"),
        sum_file(&tarball, &format!("netinstall-{V}.tar.gz")),
    );
    srv.route(
        &format!("{d}/routeros-{V}-arm64.npk"),
        Reply::Body(main.clone()),
    );
    srv.route(
        &format!("{d}/routeros-{V}-arm64.npk.sha256"),
        sum_file(&main, &format!("routeros-{V}-arm64.npk")),
    );
    srv.route(
        &format!("{d}/wireless-{V}-arm64.npk"),
        Reply::Body(wireless.clone()),
    );
    let wl = format!("wireless-{V}-arm64.npk");
    let ct = format!("container-{V}-arm64.npk");
    srv.route(
        &format!("{d}/all_packages-arm64-{V}.zip"),
        Reply::Body(zip(&[(wl.as_str(), &wireless[..]), (ct.as_str(), b"c")])),
    );
    (tarball, main, wireless)
}

pub fn assets(srv: &TestServer, root: &Path) -> Assets {
    Assets::new(
        root,
        HttpConfig {
            download_base: format!("{}/dl/", srv.base),
            upgrade_base: format!("{}/up", srv.base),
            ..HttpConfig::default()
        },
    )
    .unwrap()
}

pub fn recording(a: Assets) -> (Assets, Arc<Mutex<Vec<AssetEvent>>>) {
    let events: Arc<Mutex<Vec<AssetEvent>>> = Arc::default();
    let sink = events.clone();
    let a = a.with_events(Arc::new(move |e| sink.lock().unwrap().push(e)));
    (a, events)
}
