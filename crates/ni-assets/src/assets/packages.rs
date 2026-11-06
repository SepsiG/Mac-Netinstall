//! Packages: the extra-package list and `.npk` downloads.

use super::{blocking, check_published, Assets};
use crate::arch::PkgArch;
use crate::archive::{self, ArchiveError};
use crate::cache::{unix_now, CacheDir, Entry, Lookup};
use crate::error::AssetError;
use crate::events::AssetEvent;
use crate::npk::{check_npk, package_names, valid_package_name};
use crate::urls::{self, MAIN_PACKAGE};
use crate::version::Version;
use bytes::Bytes;
use ni_proto::PlanFile;

/// Bytes fetched from the end of `all_packages-*.zip` to read its directory.
const ZIP_TAIL: u64 = 64 * 1024;
/// Largest ZIP central directory we are willing to fetch.
const MAX_ZIP_DIRECTORY: u64 = 4 << 20;

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
        let _guard = self.lock.lock().await;
        self.package_list(version, arch).await
    }

    async fn package_list(
        &self,
        version: &Version,
        arch: PkgArch,
    ) -> Result<Vec<String>, AssetError> {
        let dir = self.version_dir(version);
        let mut index = dir.load();
        if let Some(list) = index.package_lists.get(arch.as_str()) {
            return Ok(list.clone());
        }
        let url = urls::file(
            &self.download_base,
            version,
            &urls::all_packages_name(version, arch),
        );
        let (tail, total) = self.http.tail(&url, ZIP_TAIL).await?;
        let entries = match archive::zip_entry_names(&tail, total) {
            Err(ArchiveError::NeedMore { needed }) if needed <= MAX_ZIP_DIRECTORY => {
                let (tail, total) = self.http.tail(&url, needed).await?;
                archive::zip_entry_names(&tail, total)?
            }
            other => other?,
        };
        let names = package_names(&entries, version.as_str(), arch.as_str());
        index
            .package_lists
            .insert(arch.as_str().to_string(), names.clone());
        blocking(move || dir.save(&index)).await?;
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
        let _guard = self.lock.lock().await;
        if names.len() > 1 {
            let available = self.package_list(version, arch).await?;
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
        let lookup = {
            let (dir, file) = (dir.clone(), file.to_string());
            blocking(move || dir.lookup(&dir.load(), &file)).await
        };
        if let Lookup::Valid(data) = lookup {
            self.emit(AssetEvent::CacheHit { file: file.into() });
            return Ok(data);
        }
        let url = urls::file(&self.download_base, version, file);
        let published = self.published_checksum(&url, file).await?;
        {
            let dir = dir.clone();
            blocking(move || dir.create()).await?;
        }
        let part = dir.path(&format!("{file}.part"));
        let got = self
            .http
            .download(&url, &part, file, &|e| self.emit(e))
            .await?;
        let (dir, file) = (dir.clone(), file.to_string());
        blocking(move || {
            let result = (|| {
                check_published(&file, &published, &got.sha256)?;
                let data = std::fs::read(&part).map_err(AssetError::io("read", &part))?;
                check_npk(&file, &data)?;
                dir.commit(&part, &file)?;
                let mut index = dir.load();
                let entry = Entry {
                    size: got.size,
                    sha256: got.sha256,
                    source: url,
                    published_checksum: published.is_some(),
                    fetched_at: unix_now(),
                };
                index.files.insert(file.clone(), entry);
                dir.save(&index)?;
                Ok(Bytes::from(data))
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&part);
            }
            result
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::INDEX_FILE;
    use crate::testutil::*;
    use ni_proto::InstallPlan;

    #[tokio::test]
    async fn lists_extra_packages_from_the_zip_directory() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let a = assets(&srv, &tempdir("list"));
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
        assert!(!index.files[&format!("wireless-{V}-arm64.npk")].published_checksum);

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
        let a = assets(&srv, &tempdir("main-only"));
        let files = a.ensure_packages(&v(), PkgArch::Arm64, &[]).await.unwrap();
        assert_eq!(files.len(), 1);
        assert!(!srv.paths().iter().any(|p| p.contains("all_packages")));
    }

    #[tokio::test]
    async fn bad_package_requests_are_rejected() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let a = assets(&srv, &tempdir("bad-packages"));
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
        srv.route(
            &format!("{d}/routeros-{V}-arm64.npk.sha256"),
            Reply::Status(404),
        );
        srv.route(
            &format!("{d}/routeros-{V}-arm64.npk"),
            Reply::Body(b"<html>maintenance</html>".to_vec()),
        );
        let root = tempdir("not-npk");
        let a = assets(&srv, &root);
        let err = a
            .ensure_packages(&v(), PkgArch::Arm64, &[])
            .await
            .unwrap_err();
        assert_eq!(err.code(), "not_npk");
        let mut short = npk(100, 1);
        short.truncate(90);
        srv.route(&format!("{d}/routeros-{V}-arm64.npk"), Reply::Body(short));
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
}
