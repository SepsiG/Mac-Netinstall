//! A fake MikroTik mirror for [`Assets`] tests.

use super::{
    kernel_container, npk_bundle, npk_package, npk_package_version, synthetic_kernels, tar_gz, zip,
    Reply, TestServer,
};
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
    let main = npk_package("system", "arm64", 5000, 0xaa);
    let wireless = npk_package("wireless", "arm64", 3000, 0xbb);
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

/// Also serve `routeros` and `wireless` (with checksums) and the
/// `all_packages` zip for the download architecture `download` of [`V`],
/// with `npk_arch` as the architecture inside both packages (the arch a
/// device names in its hello: `mips` for `mipsbe`). Returns the two
/// packages.
pub fn mirror_arch(srv: &TestServer, download: &str, npk_arch: &str) -> (Vec<u8>, Vec<u8>) {
    let main = npk_package("system", npk_arch, 4000, 0xa1);
    let wireless = npk_package("wireless", npk_arch, 2000, 0xb1);
    let d = format!("/dl/{V}");
    for (name, data) in [("routeros", &main), ("wireless", &wireless)] {
        let file = format!("{name}-{V}-{download}.npk");
        srv.route(&format!("{d}/{file}"), Reply::Body(data.clone()));
        srv.route(&format!("{d}/{file}.sha256"), sum_file(data, &file));
    }
    let wl = format!("wireless-{V}-{download}.npk");
    srv.route(
        &format!("{d}/all_packages-{download}-{V}.zip"),
        Reply::Body(zip(&[(wl.as_str(), &wireless[..])])),
    );
    (main, wireless)
}

/// The newest v6 the fake mirror announces (`NEWEST6.long-term`).
pub const V6: &str = "6.49.23";

/// Serve a RouterOS v6 version `ver` for the download arch `download`
/// (`ppc` is published as `routeros-powerpc-<ver>.npk`) with `npk_arch`
/// inside the packages: the bundle main package (system, ipv6, wireless,
/// ...), the `calea` extra and the `all_packages` zip (listing the bundle's
/// parts too, as real v6 zips do), plus `NEWEST6.long-term` → [`V6`].
/// Checksums are served if `sums`. Returns the bundle and `calea`.
pub fn mirror_v6(
    srv: &TestServer,
    ver: &str,
    download: &str,
    npk_arch: &str,
    sums: bool,
) -> (Vec<u8>, Vec<u8>) {
    let nums: Vec<u8> = ver.split('.').map(|p| p.parse().unwrap()).collect();
    let version = (nums[0], nums[1], nums.get(2).copied().unwrap_or(0));
    let bundle = npk_bundle(
        download,
        npk_arch,
        version,
        &["system", "ipv6", "wireless@", "hotspot", "ppp", "dhcp"],
        4000,
    );
    let calea = npk_package_version("calea", npk_arch, version, 1500, 0xc1);
    let d = format!("/dl/{ver}");
    srv.route(
        "/up/NEWEST6.long-term",
        Reply::Body(format!("{V6} 1793000000\n").into_bytes()),
    );
    let arch6 = if download == "ppc" {
        "powerpc"
    } else {
        download
    };
    let main = format!("routeros-{arch6}-{ver}.npk");
    let extra = format!("calea-{ver}-{download}.npk");
    for (file, data) in [(&main, &bundle), (&extra, &calea)] {
        srv.route(&format!("{d}/{file}"), Reply::Body(data.clone()));
        if sums {
            srv.route(&format!("{d}/{file}.sha256"), sum_file(data, file));
        }
    }
    let entries: Vec<String> = ["system", "wireless", "calea", "gps"]
        .iter()
        .map(|n| format!("{n}-{ver}-{download}.npk"))
        .collect();
    let files: Vec<(&str, &[u8])> = entries.iter().map(|e| (e.as_str(), &b"x"[..])).collect();
    srv.route(
        &format!("{d}/all_packages-{download}-{ver}.zip"),
        Reply::Body(zip(&files)),
    );
    srv.route(&format!("{d}/CHANGELOG"), Reply::Body(b"changes".to_vec()));
    (bundle, calea)
}

pub fn assets(srv: &TestServer, root: &Path) -> Assets {
    Assets::new(
        root,
        HttpConfig {
            download_base: format!("{}/dl/", srv.base),
            upgrade_base: format!("{}/up", srv.base),
            changelog_base: format!("{}/cl", srv.base),
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

/// Names of `*.part` files anywhere under `root`.
pub fn parts(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "part") {
                out.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    out
}
