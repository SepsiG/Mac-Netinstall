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

/// Serve a fake MikroTik mirror for [`V`]: tarball, arm64 main package,
/// `wireless` (all with checksums) and the arm64 `all_packages` zip.
pub fn mirror(srv: &TestServer) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    mirror_version(srv, V)
}

/// Like [`mirror`], for version `ver`.
pub fn mirror_version(srv: &TestServer, ver: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let tarball = netinstall_tarball();
    let main = npk(5000, 0xaa);
    let wireless = npk(3000, 0xbb);
    let d = format!("/dl/{ver}");
    srv.route(
        "/up/NEWESTa7.stable",
        Reply::Body(b"7.25.1 1793623193\n".to_vec()),
    );
    srv.route(
        &format!("{d}/netinstall-{ver}.tar.gz"),
        Reply::Body(tarball.clone()),
    );
    srv.route(
        &format!("{d}/netinstall-{ver}.tar.gz.sha256"),
        sum_file(&tarball, &format!("netinstall-{ver}.tar.gz")),
    );
    srv.route(
        &format!("{d}/routeros-{ver}-arm64.npk"),
        Reply::Body(main.clone()),
    );
    srv.route(
        &format!("{d}/routeros-{ver}-arm64.npk.sha256"),
        sum_file(&main, &format!("routeros-{ver}-arm64.npk")),
    );
    srv.route(
        &format!("{d}/wireless-{ver}-arm64.npk"),
        Reply::Body(wireless.clone()),
    );
    srv.route(
        &format!("{d}/wireless-{ver}-arm64.npk.sha256"),
        sum_file(&wireless, &format!("wireless-{ver}-arm64.npk")),
    );
    let wl = format!("wireless-{ver}-arm64.npk");
    let ct = format!("container-{ver}-arm64.npk");
    srv.route(
        &format!("{d}/all_packages-arm64-{ver}.zip"),
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
