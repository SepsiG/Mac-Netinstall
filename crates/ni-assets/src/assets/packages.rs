//! Packages: the extra-package list and `.npk` downloads.

use super::{blocking, Assets};
use crate::arch::PkgArch;
use crate::archive::{self, ArchiveError};
use crate::cache::{unix_now, CacheDir, Entry, Index, Lookup};
use crate::error::AssetError;
use crate::events::AssetEvent;
use crate::lock;
use crate::npk::{check_npk, package_names, valid_package_name};
use crate::urls::{self, MAIN_PACKAGE};
use crate::version::Version;
use bytes::Bytes;
use ni_proto::PlanFile;

/// Bytes fetched from the end of `all_packages-*.zip` to read its directory.
const ZIP_TAIL: u64 = 64 * 1024;
/// Largest ZIP central directory we are willing to fetch.
const MAX_ZIP_DIRECTORY: u64 = 4 << 20;

/// The cached package list for `arch`, if present and made only of valid
/// package names (a hand-edited index cannot smuggle names into URLs).
fn cached_list(index: &Index, arch: PkgArch) -> Option<Vec<String>> {
    index
        .package_lists
        .get(arch.as_str())
        .filter(|names| names.iter().all(|n| valid_package_name(n)))
        .cloned()
}

impl Assets {
    /// Names of the extra packages published for `arch` (e.g. `wireless`,
    /// `container`), read from the directory of `all_packages-<arch>-<ver>.zip`
    /// with a ranged request and cached. The main `routeros` package is not
    /// listed.
    pub async fn available_packages(
        &self,
        version: &Version,
        arch: PkgArch,
    ) -> Result<Vec<String>, AssetError> {
        let dir = self.version_dir(version);
        let load = || {
            let dir = dir.clone();
            blocking(move || cached_list(&dir.load(), arch))
        };
        if let Some(list) = load().await? {
            return Ok(list);
        }
        let zip_name = urls::all_packages_name(version, arch);
        let lock = lock::lock_target(dir.lock_path(&zip_name)).await?;
        if let Some(list) = load().await? {
            return Ok(list);
        }
        let url = urls::file(&self.download_base, version, &zip_name);
        let (tail, total) = self.http.tail(&url, ZIP_TAIL).await?;
        let entries = match archive::zip_entry_names(&tail, total) {
            Err(ArchiveError::NeedMore { needed }) if needed <= MAX_ZIP_DIRECTORY => {
                let (tail, total) = self.http.tail(&url, needed).await?;
                archive::zip_entry_names(&tail, total)?
            }
            other => other?,
        };
        let names = package_names(&entries, version.as_str(), arch.as_str());
        let list = names.clone();
        blocking(move || {
            let _lock = lock;
            dir.create()?;
            dir.update(|index| {
                index.package_lists.insert(arch.as_str().to_string(), list);
            })
        })
        .await??;
        Ok(names)
    }

    /// The main `routeros` package followed by `extras` (duplicates and
    /// `routeros` itself ignored), verified and ready for
    /// [`ni_proto::InstallPlan::packages`]. Each [`PlanFile::name`] is the
    /// package file name, e.g. `routeros-7.25.1-arm64.npk`.
    pub async fn ensure_packages(
        &self,
        version: &Version,
        arch: PkgArch,
        extras: &[String],
    ) -> Result<Vec<PlanFile>, AssetError> {
        if let Some(bad) = extras.iter().find(|n| !valid_package_name(n)) {
            return Err(AssetError::InvalidPackageName(bad.clone()));
        }
        let mut names = vec![MAIN_PACKAGE.to_string()];
        for name in extras {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        if names.len() > 1 {
            let available = self.available_packages(version, arch).await?;
            if let Some(missing) = names[1..].iter().find(|n| !available.contains(n)) {
                return Err(AssetError::UnknownPackage {
                    name: missing.clone(),
                    arch,
                });
            }
        }
        let dir = self.version_dir(version);
        let mut files = Vec::with_capacity(names.len());
        for name in names {
            let file = urls::package_file_name(&name, version, arch);
            let data = self.ensure_npk(&dir, version, &file).await?;
            files.push(PlanFile { name: file, data });
        }
        Ok(files)
    }

    async fn ensure_npk(
        &self,
        dir: &CacheDir,
        version: &Version,
        file: &str,
    ) -> Result<Bytes, AssetError> {
        let lookup = || {
            let (dir, file) = (dir.clone(), file.to_string());
            blocking(move || dir.lookup(&dir.load(), &file))
        };
        if let Lookup::Valid(data) = lookup().await? {
            self.emit(AssetEvent::CacheHit { file: file.into() });
            return Ok(data);
        }
        let lock = lock::lock_target(dir.lock_path(file)).await?;
        if let Lookup::Valid(data) = lookup().await? {
            self.emit(AssetEvent::CacheHit { file: file.into() });
            return Ok(data);
        }
        let fetched = self.fetch(dir, version, file, lock).await?;
        let (dir, file) = (dir.clone(), file.to_string());
        blocking(move || {
            let (data, sha256) = fetched.verify(&file)?;
            check_npk(&file, &data)?;
            let entry = Entry {
                size: data.len() as u64,
                sha256,
                source: fetched.url.clone(),
                published_checksum: fetched.published.is_some(),
                fetched_at: unix_now(),
            };
            let super::Fetched { part, _lock, .. } = fetched;
            dir.commit(part.path(), &file)?;
            part.committed();
            dir.update(|index| {
                index.files.insert(file.clone(), entry);
            })?;
            drop(_lock);
            Ok(Bytes::from(data))
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::INDEX_FILE;
    use crate::testutil::*;
    use ni_proto::InstallPlan;
    use std::time::Duration;

    #[tokio::test]
    async fn lists_extra_packages_from_the_zip_directory() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let root = tempdir("list");
        let a = assets(&srv, &root);
        let names = a.available_packages(&v(), PkgArch::Arm64).await.unwrap();
        assert_eq!(names, ["container", "wireless"]);
        assert_eq!(
            a.available_packages(&v(), PkgArch::Arm64).await.unwrap(),
            names
        );
        assert_eq!(srv.paths().len(), 1, "list is cached");
        assert!(srv.requests()[0]
            .to_ascii_lowercase()
            .contains("range: bytes=-65536"));
        let err = a.available_packages(&v(), PkgArch::Tile).await.unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[tokio::test]
    async fn packages_come_main_first_and_form_a_valid_plan() {
        let srv = TestServer::start().await;
        let (_, main, wireless) = mirror(&srv);
        let root = tempdir("packages");
        let a = assets(&srv, &root);
        let extras = ["wireless".to_string(), "routeros".into(), "wireless".into()];
        let files = a
            .ensure_packages(&v(), PkgArch::Arm64, &extras)
            .await
            .unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                format!("routeros-{V}-arm64.npk"),
                format!("wireless-{V}-arm64.npk")
            ]
        );
        assert_eq!(files[0].data.as_ref(), main.as_slice());
        assert_eq!(files[1].data.as_ref(), wireless.as_slice());
        let plan = InstallPlan {
            packages: files.clone(),
            config_script: None,
        };
        assert_eq!(plan.validate(), Ok(()));

        let index = CacheDir::new(root.join(V)).load();
        assert!(index.files[&format!("routeros-{V}-arm64.npk")].published_checksum);
        assert!(index.files[&format!("wireless-{V}-arm64.npk")].published_checksum);

        let before = srv.paths().len();
        assert_eq!(
            a.ensure_packages(&v(), PkgArch::Arm64, &extras)
                .await
                .unwrap(),
            files
        );
        assert_eq!(srv.paths().len(), before, "served from the cache");
    }

    #[tokio::test]
    async fn main_package_alone_needs_no_package_list() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let root = tempdir("main-only");
        let a = assets(&srv, &root);
        let files = a.ensure_packages(&v(), PkgArch::Arm64, &[]).await.unwrap();
        assert_eq!(files.len(), 1);
        assert!(!srv.paths().iter().any(|p| p.contains("all_packages")));
    }

    #[tokio::test]
    async fn bad_package_requests_are_rejected() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let root = tempdir("bad-packages");
        let a = assets(&srv, &root);
        let err = a
            .ensure_packages(&v(), PkgArch::Arm64, &["../etc".into()])
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_package_name");
        let err = a
            .ensure_packages(&v(), PkgArch::Arm64, &["dude".into()])
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AssetError::UnknownPackage { name, arch: PkgArch::Arm64 } if name == "dude"),
            "{err:?}"
        );
        let err = a
            .ensure_packages(&v(), PkgArch::Mipsbe, &[])
            .await
            .unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[tokio::test]
    async fn downloads_that_are_not_packages_are_rejected() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let d = format!("/dl/{V}");
        let file = format!("routeros-{V}-arm64.npk");
        let serve = |body: Vec<u8>| {
            srv.route(&format!("{d}/{file}.sha256"), sum_file(&body, &file));
            srv.route(&format!("{d}/{file}"), Reply::Body(body));
        };
        serve(b"<html>maintenance</html>".to_vec());
        let root = tempdir("not-npk");
        let a = assets(&srv, &root);
        let err = a
            .ensure_packages(&v(), PkgArch::Arm64, &[])
            .await
            .unwrap_err();
        assert_eq!(err.code(), "not_npk");
        let mut short = npk(100, 1);
        short.truncate(90);
        serve(short);
        let err = a
            .ensure_packages(&v(), PkgArch::Arm64, &[])
            .await
            .unwrap_err();
        assert_eq!(err.code(), "npk_length");
        let leftovers: Vec<_> = std::fs::read_dir(root.join(V))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != INDEX_FILE)
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[tokio::test]
    async fn package_checksums_are_required_from_7_15_only() {
        let srv = TestServer::start().await;
        let root = tempdir("npk-sum-policy");
        let a = assets(&srv, &root);
        for (ver, required) in [("7.14", false), ("7.15", true)] {
            let (_, main, _) = mirror_version(&srv, ver);
            srv.route(
                &format!("/dl/{ver}/routeros-{ver}-arm64.npk.sha256"),
                Reply::Status(404),
            );
            let version = Version::parse(ver).unwrap();
            let got = a.ensure_packages(&version, PkgArch::Arm64, &[]).await;
            if required {
                assert_eq!(got.unwrap_err().code(), "checksum_missing", "{ver}");
            } else {
                assert_eq!(got.unwrap()[0].data.as_ref(), main.as_slice(), "{ver}");
                let index = CacheDir::new(root.join(ver)).load();
                assert!(!index.files[&format!("routeros-{ver}-arm64.npk")].published_checksum);
            }
        }
    }

    #[tokio::test]
    async fn a_damaged_cached_package_is_downloaded_again() {
        let srv = TestServer::start().await;
        let (_, main, _) = mirror(&srv);
        let root = tempdir("npk-damaged");
        let a = assets(&srv, &root);
        a.ensure_packages(&v(), PkgArch::Arm64, &[]).await.unwrap();
        let path = root.join(V).join(format!("routeros-{V}-arm64.npk"));
        std::fs::write(&path, npk(5000, 0xcc)).unwrap();
        let files = a.ensure_packages(&v(), PkgArch::Arm64, &[]).await.unwrap();
        assert_eq!(files[0].data.as_ref(), main.as_slice());
        assert_eq!(std::fs::read(&path).unwrap(), main);
    }

    fn slow(body: &[u8]) -> Reply {
        Reply::Slow {
            body: body.to_vec(),
            chunk: 500,
            delay: Duration::from_millis(20),
        }
    }

    #[tokio::test]
    async fn two_instances_downloading_the_same_package_never_corrupt_it() {
        let srv = TestServer::start().await;
        let ver = v();
        let (_, main, _) = mirror(&srv);
        let file = format!("routeros-{V}-arm64.npk");
        srv.route(&format!("/dl/{V}/{file}"), slow(&main));
        let root = tempdir("race-npk");
        let (a, b) = (assets(&srv, &root), assets(&srv, &root));
        let late_b = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            b.ensure_packages(&ver, PkgArch::Arm64, &[]).await
        };
        let (ra, rb) = tokio::join!(a.ensure_packages(&ver, PkgArch::Arm64, &[]), late_b);
        assert_eq!(ra.unwrap()[0].data.as_ref(), main.as_slice());
        assert_eq!(rb.unwrap()[0].data.as_ref(), main.as_slice());
        assert_eq!(std::fs::read(root.join(V).join(&file)).unwrap(), main);
        let index = CacheDir::new(root.join(V)).load();
        assert_eq!(index.files[&file].sha256, crate::npk::sha256_hex(&main));
        let downloads = srv.paths().iter().filter(|p| p.ends_with(".npk")).count();
        assert_eq!(downloads, 1, "the second instance waits and uses the cache");
        assert_eq!(parts(&root), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_lock_held_elsewhere_is_waited_for_and_the_cache_rechecked() {
        let srv = TestServer::start().await;
        let ver = v();
        let (_, main, _) = mirror(&srv);
        let file = format!("routeros-{V}-arm64.npk");
        let root = tempdir("lock-elsewhere");
        let dir = CacheDir::new(root.join(V));
        // Another process holds the file lock while it downloads.
        let held = crate::lock::try_lock_file(&dir.lock_path(&file))
            .unwrap()
            .expect("free");
        let a = assets(&srv, &root);
        let fut = a.ensure_packages(&ver, PkgArch::Arm64, &[]);
        tokio::pin!(fut);
        let waited = tokio::time::timeout(Duration::from_millis(300), &mut fut).await;
        assert!(waited.is_err(), "must wait for the lock");
        // ... and finishes the file before releasing it.
        dir.write_atomic(&file, &main).unwrap();
        dir.update(|index| {
            index
                .files
                .insert(file.clone(), Entry::new(&main, "elsewhere", true));
        })
        .unwrap();
        drop(held);
        let files = fut.await.unwrap();
        assert_eq!(files[0].data.as_ref(), main.as_slice());
        assert!(
            !srv.paths().iter().any(|p| p.ends_with(".npk")),
            "{:?}",
            srv.paths()
        );
    }

    #[tokio::test]
    async fn a_dropped_download_leaves_the_cache_usable() {
        let srv = TestServer::start().await;
        let ver = v();
        let (_, main, wireless) = mirror(&srv);
        let root = tempdir("cancel");
        let a = assets(&srv, &root);
        a.ensure_packages(&ver, PkgArch::Arm64, &[]).await.unwrap();
        let wl = format!("wireless-{V}-arm64.npk");
        srv.route(&format!("/dl/{V}/{wl}"), slow(&wireless));
        let extras = ["wireless".to_string()];
        let cut = tokio::time::timeout(
            Duration::from_millis(80),
            a.ensure_packages(&ver, PkgArch::Arm64, &extras),
        )
        .await;
        assert!(cut.is_err(), "dropped mid-download");
        assert_eq!(parts(&root), Vec::<String>::new(), "no stray part file");
        srv.route(&format!("/dl/{V}/{wl}"), Reply::Body(wireless.clone()));
        let files = a
            .ensure_packages(&ver, PkgArch::Arm64, &extras)
            .await
            .unwrap();
        assert_eq!(files[0].data.as_ref(), main.as_slice());
        assert_eq!(files[1].data.as_ref(), wireless.as_slice());
        let index = CacheDir::new(root.join(V)).load();
        assert!(index.files.contains_key(&format!("routeros-{V}-arm64.npk")));
        assert!(index.files.contains_key(&wl));
        assert_eq!(index.package_lists["arm64"], ["container", "wireless"]);
    }

    #[tokio::test]
    async fn one_instance_serves_concurrent_requests_and_keeps_every_index_entry() {
        let srv = TestServer::start().await;
        let ver = v();
        let (_, main, wireless) = mirror(&srv);
        let d = format!("/dl/{V}");
        srv.route(&format!("{d}/routeros-{V}-arm64.npk"), slow(&main));
        let root = tempdir("concurrent");
        let a = assets(&srv, &root);
        let extras = ["wireless".to_string()];
        let (k, p1, p2, list) = tokio::join!(
            a.ensure_kernels(&ver),
            a.ensure_packages(&ver, PkgArch::Arm64, &extras),
            a.ensure_packages(&ver, PkgArch::Arm64, &[]),
            a.available_packages(&ver, PkgArch::Arm64),
        );
        assert_eq!(k.unwrap().len(), 8);
        let p1 = p1.unwrap();
        assert_eq!(p1[1].data.as_ref(), wireless.as_slice());
        assert_eq!(p2.unwrap()[0].data.as_ref(), main.as_slice());
        assert_eq!(list.unwrap(), ["container", "wireless"]);
        let index = CacheDir::new(root.join(V)).load();
        let mut names: Vec<&str> = index.files.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(names.len(), 10, "{names:?}");
        assert!(index.package_lists.contains_key("arm64"));
    }

    #[tokio::test]
    async fn a_stalled_download_does_not_block_cached_reads() {
        let srv = TestServer::start().await;
        let ver = v();
        mirror(&srv);
        let root = tempdir("stall");
        let a = assets(&srv, &root);
        a.ensure_kernels(&ver).await.unwrap();
        let script = root.join("s.rsc");
        std::fs::write(&script, b"#").unwrap();
        a.register_local_file(&script).await.unwrap();
        srv.route(&format!("/dl/{V}/routeros-{V}-arm64.npk"), Reply::Stall);
        let stalled = a.ensure_packages(&ver, PkgArch::Arm64, &[]);
        let others = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let limit = Duration::from_secs(2);
            let local = tokio::time::timeout(limit, a.local_file("s.rsc")).await;
            let kernels = tokio::time::timeout(limit, a.ensure_kernels(&ver)).await;
            let list =
                tokio::time::timeout(limit, a.available_packages(&ver, PkgArch::Arm64)).await;
            (local, kernels, list)
        };
        tokio::select! {
            _ = stalled => panic!("the stalled download finished"),
            (local, kernels, list) = others => {
                assert_eq!(local.expect("local_file blocked").unwrap().as_ref(), b"#");
                assert_eq!(kernels.expect("cached kernels blocked").unwrap().len(), 8);
                list.expect("package list blocked").unwrap();
            }
        }
    }

    #[tokio::test]
    async fn invalid_names_in_a_cached_package_list_are_not_trusted() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let root = tempdir("list-tampered");
        let dir = CacheDir::new(root.join(V));
        dir.update(|index| {
            index
                .package_lists
                .insert("arm64".into(), vec!["../../evil".into(), "wireless".into()]);
        })
        .unwrap();
        let a = assets(&srv, &root);
        let names = a.available_packages(&v(), PkgArch::Arm64).await.unwrap();
        assert_eq!(names, ["container", "wireless"]);
        assert_eq!(dir.load().package_lists["arm64"], names);
    }

    #[tokio::test]
    async fn a_large_zip_directory_is_fetched_with_a_second_range_request() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let long = "x".repeat(55);
        let names: Vec<String> = (0..600)
            .map(|i| format!("p{i:03}{long}-{V}-arm64.npk"))
            .collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
        srv.route(
            &format!("/dl/{V}/all_packages-arm64-{V}.zip"),
            Reply::Body(zip(&entries)),
        );
        let root = tempdir("list-needmore");
        let a = assets(&srv, &root);
        let got = a.available_packages(&v(), PkgArch::Arm64).await.unwrap();
        assert_eq!(got.len(), 600);
        assert_eq!(got[0], format!("p000{long}"));
        let ranges: Vec<u64> = srv
            .requests()
            .iter()
            .filter_map(|r| {
                let r = r.to_ascii_lowercase();
                let at = r.find("range: bytes=-")? + "range: bytes=-".len();
                r[at..].lines().next()?.trim().parse().ok()
            })
            .collect();
        assert_eq!(ranges.len(), 2, "{ranges:?}");
        assert_eq!(ranges[0], ZIP_TAIL);
        assert!(ranges[1] > ZIP_TAIL);
    }

    #[tokio::test]
    async fn a_zip64_package_archive_is_rejected() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let mut z = zip(&[("wireless-7.25.1-arm64.npk", b"x")]);
        let eocd = z.len() - 22;
        z[eocd + 8..eocd + 12].copy_from_slice(&[0xff; 4]);
        srv.route(
            &format!("/dl/{V}/all_packages-arm64-{V}.zip"),
            Reply::Body(z),
        );
        let root = tempdir("list-zip64");
        let err = assets(&srv, &root)
            .available_packages(&v(), PkgArch::Arm64)
            .await
            .unwrap_err();
        assert!(
            matches!(err, AssetError::Archive(ArchiveError::BadZip(r)) if r.contains("zip64")),
            "{err:?}"
        );
    }
}
