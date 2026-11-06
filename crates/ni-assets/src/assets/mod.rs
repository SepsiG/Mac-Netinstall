//! The asset manager: version resolution, boot kernels, packages and
//! user-supplied files, all behind one cache.

use crate::archive;
use crate::cache::{CacheDir, Entry, Lookup};
use crate::error::AssetError;
use crate::events::{AssetEvent, EventSink};
use crate::http::{Http, HttpConfig};
use crate::kernel::{extract_kernels, KernelSet};
use crate::npk::parse_sha256_file;
use crate::urls;
use crate::version::{parse_newest, Channel, Release, Version, VersionSpec};
use ni_bootp::BootArch;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

mod local;
mod packages;

pub use local::{LocalFile, LocalKind, MAX_SCRIPT_SIZE};

/// Cache directory name under `~/Library/Caches`.
pub const BUNDLE_ID: &str = "io.github.sepsig.mac-netinstall";

/// `~/Library/Caches/io.github.sepsig.mac-netinstall`, or `None` if `HOME`
/// is not set.
pub fn default_cache_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Library/Caches").join(BUNDLE_ID))
}

/// Cache file name of the boot kernel for `arch`.
pub fn kernel_file_name(arch: BootArch) -> String {
    format!("kernel-{arch}.elf")
}

/// Downloads, verifies and caches everything an install needs.
///
/// Cache-changing operations are serialized by an internal lock, so one
/// `Assets` can be shared (`Arc`) between tasks. Two processes sharing a
/// cache root are not coordinated.
pub struct Assets {
    root: PathBuf,
    http: Http,
    download_base: String,
    upgrade_base: String,
    max_download: u64,
    events: Option<EventSink>,
    lock: Mutex<()>,
}

/// Run blocking file work off the async threads; panics propagate.
async fn blocking<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

fn check_published(file: &str, published: &Option<String>, actual: &str) -> Result<(), AssetError> {
    match published {
        Some(expected) if expected != actual => Err(AssetError::ChecksumMismatch {
            file: file.into(),
            expected: expected.clone(),
            actual: actual.into(),
        }),
        _ => Ok(()),
    }
}

impl Assets {
    /// An asset manager caching under `root` (created on first write).
    pub fn new(root: impl Into<PathBuf>, cfg: HttpConfig) -> Result<Self, AssetError> {
        Ok(Self {
            root: root.into(),
            http: Http::new(&cfg)?,
            download_base: cfg.download_base.trim_end_matches('/').to_string(),
            upgrade_base: cfg.upgrade_base.trim_end_matches('/').to_string(),
            max_download: cfg.max_download_bytes,
            events: None,
            lock: Mutex::new(()),
        })
    }

    /// Report progress to `sink`.
    pub fn with_events(mut self, sink: EventSink) -> Self {
        self.events = Some(sink);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn emit(&self, event: AssetEvent) {
        if let Some(sink) = &self.events {
            sink(event);
        }
    }

    fn version_dir(&self, version: &Version) -> CacheDir {
        CacheDir::new(self.root.join(version.as_str()))
    }

    /// The newest version on `channel` (always asks the server).
    pub async fn latest(&self, channel: Channel) -> Result<Release, AssetError> {
        let url = urls::newest(&self.upgrade_base, channel);
        let body = self.http.text(&url).await?;
        parse_newest(&body).ok_or(AssetError::BadResponse {
            url,
            reason: "expected `<version> <timestamp>`",
        })
    }

    /// The version a profile asks for; exact versions need no network.
    pub async fn resolve(&self, spec: &VersionSpec) -> Result<Version, AssetError> {
        match spec {
            VersionSpec::Exact(v) => Ok(v.clone()),
            VersionSpec::Channel(c) => Ok(self.latest(*c).await?.version),
        }
    }

    /// The published SHA-256 of `file` at `url`. From 7.15 on a checksum
    /// must exist (`checksum_missing` otherwise); for older versions `None`
    /// means none is published.
    async fn published_checksum(
        &self,
        version: &Version,
        url: &str,
        file: &str,
    ) -> Result<Option<String>, AssetError> {
        let sum_url = urls::checksum(url);
        match self.http.optional_text(&sum_url).await? {
            None if version.has_published_checksums() => {
                Err(AssetError::ChecksumMissing { url: sum_url })
            }
            None => Ok(None),
            Some(body) => parse_sha256_file(&body, file)
                .map(Some)
                .ok_or(AssetError::BadResponse {
                    url: sum_url,
                    reason: "unexpected checksum file format",
                }),
        }
    }

    /// Boot kernels for every [`BootArch`], from the cache or carved out of
    /// `netinstall-<ver>.tar.gz` (which is not kept).
    pub async fn ensure_kernels(&self, version: &Version) -> Result<KernelSet, AssetError> {
        let _guard = self.lock.lock().await;
        let dir = self.version_dir(version);
        let tarball = urls::tarball_name(version);
        let cached = {
            let dir = dir.clone();
            blocking(move || load_kernels(&dir)).await
        };
        if let Some(set) = cached {
            self.emit(AssetEvent::CacheHit { file: tarball });
            return Ok(set);
        }
        let url = urls::file(&self.download_base, version, &tarball);
        let part = dir.path(&format!("{tarball}.part"));
        {
            let dir = dir.clone();
            blocking(move || dir.create()).await?;
        }
        let got = self
            .http
            .download(&url, &part, &tarball, &|e| self.emit(e))
            .await?;
        let published = match self.published_checksum(version, &url, &tarball).await {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        };
        blocking(move || {
            let result = (|| {
                check_published(&tarball, &published, &got.sha256)?;
                let file = std::fs::File::open(&part).map_err(AssetError::io("open", &part))?;
                let cli = archive::netinstall_cli_from_tar_gz(BufReader::new(file))?;
                Ok::<_, AssetError>(extract_kernels(&cli)?)
            })();
            let _ = std::fs::remove_file(&part);
            let set = result?;
            let mut index = dir.load();
            for (arch, image) in &set {
                let name = kernel_file_name(*arch);
                dir.write_atomic(&name, image)?;
                index
                    .files
                    .insert(name, Entry::new(image, url.clone(), published.is_some()));
            }
            dir.save(&index)?;
            Ok(set)
        })
        .await
    }
}

/// All kernels of a version from the cache, or `None` if any is missing or
/// damaged.
fn load_kernels(dir: &CacheDir) -> Option<KernelSet> {
    let index = dir.load();
    BootArch::ALL
        .into_iter()
        .map(|arch| match dir.lookup(&index, &kernel_file_name(arch)) {
            Lookup::Valid(data) => Some((arch, data)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    #[test]
    fn default_root_is_under_library_caches() {
        let root = default_cache_root().expect("HOME is set in tests");
        assert!(root.ends_with("Library/Caches/io.github.sepsig.mac-netinstall"));
    }

    #[tokio::test]
    async fn resolves_channels_and_exact_versions() {
        let srv = TestServer::start().await;
        mirror(&srv);
        srv.route("/up/NEWESTa7.testing", Reply::Body(b"<html>".to_vec()));
        let root = tempdir("resolve");
        let a = assets(&srv, &root);
        let release = a.latest(Channel::Stable).await.unwrap();
        assert_eq!(
            (release.version.as_str(), release.released_unix),
            (V, 1_793_623_193)
        );
        let spec = VersionSpec::Exact(Version::parse("7.24.6").unwrap());
        assert_eq!(a.resolve(&spec).await.unwrap().as_str(), "7.24.6");
        assert_eq!(srv.paths(), ["GET /up/NEWESTa7.stable"]);
        let err = a.latest(Channel::Testing).await.unwrap_err();
        assert_eq!(err.code(), "bad_response");
        let err = a.latest(Channel::LongTerm).await.unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[tokio::test]
    async fn kernels_are_downloaded_verified_carved_and_cached() {
        let srv = TestServer::start().await;
        let (tarball, _, _) = mirror(&srv);
        let root = tempdir("kernels");
        let (a, events) = recording(assets(&srv, &root));
        let set = a.ensure_kernels(&v()).await.unwrap();
        for (arch, image) in synthetic_kernels() {
            assert_eq!(set[&arch].as_ref(), image.as_slice(), "{arch}");
        }
        let dir = root.join(V);
        assert!(!dir.join(format!("netinstall-{V}.tar.gz")).exists());
        assert!(!dir.join(format!("netinstall-{V}.tar.gz.part")).exists());
        let index = CacheDir::new(dir.clone()).load();
        let entry = &index.files["kernel-e500.elf"];
        assert!(entry.published_checksum);
        assert!(entry
            .source
            .ends_with(&format!("/dl/{V}/netinstall-{V}.tar.gz")));
        assert_eq!(entry.size, set[&BootArch::E500].len() as u64);
        assert!(events
            .lock()
            .unwrap()
            .contains(&AssetEvent::DownloadFinished {
                file: format!("netinstall-{V}.tar.gz"),
                size: tarball.len() as u64,
            }));

        // Second call: served from the cache, nothing downloaded.
        let before = srv.paths().len();
        let again = a.ensure_kernels(&v()).await.unwrap();
        assert_eq!(again, set);
        assert_eq!(srv.paths().len(), before);
        assert_eq!(
            events.lock().unwrap().last(),
            Some(&AssetEvent::CacheHit {
                file: format!("netinstall-{V}.tar.gz")
            })
        );
    }

    #[tokio::test]
    async fn a_damaged_cached_kernel_triggers_a_new_download() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let root = tempdir("kernels-damaged");
        let a = assets(&srv, &root);
        let set = a.ensure_kernels(&v()).await.unwrap();
        std::fs::write(root.join(V).join("kernel-tile.elf"), b"garbage").unwrap();
        let before = srv.paths().len();
        assert_eq!(a.ensure_kernels(&v()).await.unwrap(), set);
        assert!(srv.paths()[before..].contains(&format!("GET /dl/{V}/netinstall-{V}.tar.gz")));
        let tile = std::fs::read(root.join(V).join("kernel-tile.elf")).unwrap();
        assert_eq!(tile, set[&BootArch::Tile]);
    }

    #[tokio::test]
    async fn checksum_mismatch_is_rejected_and_nothing_is_cached() {
        let srv = TestServer::start().await;
        mirror(&srv);
        srv.route(
            &format!("/dl/{V}/netinstall-{V}.tar.gz.sha256"),
            sum_file(b"something else", &format!("netinstall-{V}.tar.gz")),
        );
        let root = tempdir("kernels-mismatch");
        let err = assets(&srv, &root).ensure_kernels(&v()).await.unwrap_err();
        assert_eq!(err.code(), "checksum_mismatch");
        let leftovers: Vec<_> = std::fs::read_dir(root.join(V)).unwrap().collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[tokio::test]
    async fn malformed_checksum_file_is_an_error() {
        let srv = TestServer::start().await;
        mirror(&srv);
        srv.route(
            &format!("/dl/{V}/netinstall-{V}.tar.gz.sha256"),
            Reply::Body(b"<Error>NoSuchKey</Error>".to_vec()),
        );
        let root = tempdir("kernels-badsum");
        let err = assets(&srv, &root).ensure_kernels(&v()).await.unwrap_err();
        assert_eq!(err.code(), "bad_response");
    }

    #[tokio::test]
    async fn versions_before_7_15_work_without_published_checksums() {
        let srv = TestServer::start().await;
        let old = "7.14.3";
        mirror_version(&srv, old);
        srv.route(
            &format!("/dl/{old}/netinstall-{old}.tar.gz.sha256"),
            Reply::Status(404),
        );
        let root = tempdir("kernels-nosum");
        let version = Version::parse(old).unwrap();
        assets(&srv, &root).ensure_kernels(&version).await.unwrap();
        let index = CacheDir::new(root.join(old)).load();
        assert!(!index.files["kernel-arm.elf"].published_checksum);
    }

    #[tokio::test]
    async fn a_missing_checksum_is_an_error_from_7_15_on() {
        let srv = TestServer::start().await;
        mirror_version(&srv, "7.15");
        srv.route("/dl/7.15/netinstall-7.15.tar.gz.sha256", Reply::Status(404));
        let root = tempdir("kernels-sum-required");
        let version = Version::parse("7.15").unwrap();
        let err = assets(&srv, &root)
            .ensure_kernels(&version)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "checksum_missing", "{err:?}");
        assert!(!root.join("7.15").join("kernel-arm.elf").exists());
    }

    #[tokio::test]
    async fn tarball_problems_are_reported() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let few: Vec<_> = synthetic_kernels().into_iter().take(3).collect();
        let partial = tar_gz(&[("netinstall-cli", &crate::testutil::kernel_container(&few))]);
        let d = format!("/dl/{V}");
        let tarball = format!("netinstall-{V}.tar.gz");
        srv.route(
            &format!("{d}/{tarball}.sha256"),
            sum_file(&partial, &tarball),
        );
        srv.route(&format!("{d}/{tarball}"), Reply::Body(partial));
        let root = tempdir("kernels-partial");
        let a = assets(&srv, &root);
        assert_eq!(
            a.ensure_kernels(&v()).await.unwrap_err().code(),
            "kernel_missing"
        );
        let empty = tar_gz(&[]);
        srv.route(&format!("{d}/{tarball}.sha256"), sum_file(&empty, &tarball));
        srv.route(&format!("{d}/{tarball}"), Reply::Body(empty));
        assert_eq!(
            a.ensure_kernels(&v()).await.unwrap_err().code(),
            "archive_missing_cli"
        );
        let missing = Version::parse("7.99").unwrap();
        assert_eq!(
            a.ensure_kernels(&missing).await.unwrap_err().code(),
            "not_found"
        );
    }
}
