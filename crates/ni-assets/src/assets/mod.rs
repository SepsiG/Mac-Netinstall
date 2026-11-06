//! The asset manager: version resolution, boot kernels, packages and
//! user-supplied files, all behind one cache.
//!
//! # Concurrency
//!
//! There is no global lock. Each target (a package file, a version's kernel
//! set, a package list, a local file) is downloaded and written under its
//! own lock (an in-process async lock plus a `flock` on
//! `<root>/.locks/<dir>-<name>.lock`), shared by every `Assets` in this
//! process and by other processes using the same cache root. After taking a target's
//! lock the cache is checked again, so a target fetched meanwhile by
//! another holder is not downloaded twice. Cached reads take no lock and
//! never wait for downloads. The index is changed by short read-merge-write
//! updates ([`CacheDir::update`]). Blocking file work holds the target lock
//! itself, so cancelling a call never releases a lock while its file work
//! still runs; a cancelled download's part file is removed.

use crate::archive;
use crate::cache::{CacheDir, Entry, Lookup, LOCK_DIR, PART_SUFFIX};
use crate::error::AssetError;
use crate::events::{AssetEvent, EventSink};
use crate::http::{Downloaded, Http, HttpConfig};
use crate::kernel::{extract_kernels, KernelSet};
use crate::lock::{self, TargetLock};
use crate::npk::{parse_sha256_file, sha256_hex};
use crate::urls;
use crate::version::{parse_newest, Channel, Release, Version, VersionSpec};
use ni_bootp::BootArch;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

mod inventory;
mod local;
mod packages;

pub use inventory::{
    item_version, CachedItem, CachedVersion, DeleteScope, Integrity, Inventory, ItemKind,
};
pub(crate) use local::LOCAL_DIR;

pub use local::{LocalFile, LocalKind, MAX_LOCAL_NAME, MAX_SCRIPT_SIZE};

/// `<root>/channels.json`: the last successful resolution of each channel
/// (`{"stable": "7.25.1"}`), so an offline run can stay on the version
/// the channel meant when it was last online ([`Assets::remembered`]).
pub const CHANNELS_FILE: &str = "channels.json";
/// Larger channel files are ignored.
const MAX_CHANNELS_SIZE: u64 = 64 << 10;

/// Cache directory name under `~/Library/Caches`.
pub const BUNDLE_ID: &str = "io.github.sepsig.mac-netinstall";

/// Part files older than this, whose target is not locked, are removed by
/// [`Assets::new`] (left behind by a crash).
pub const STALE_PART_AGE: Duration = Duration::from_secs(60 * 60);

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
/// One `Assets` can be shared (`Arc`) between tasks, and several instances
/// (in one or more processes) can share a cache root; see the module docs
/// for how they coordinate.
pub struct Assets {
    root: PathBuf,
    http: Http,
    download_base: String,
    upgrade_base: String,
    max_download: u64,
    events: Option<EventSink>,
}

/// Run blocking file work off the async threads; panics propagate, a
/// runtime shutdown is [`AssetError::Cancelled`].
async fn blocking<T, F>(f: F) -> Result<T, AssetError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => Ok(v),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(_) => Err(AssetError::Cancelled),
    }
}

/// A part file that is removed when dropped, unless it was committed.
struct PartFile(Option<PathBuf>);

impl PartFile {
    fn path(&self) -> &Path {
        self.0.as_deref().expect("part file still owned")
    }
    /// The file was renamed away; nothing to remove.
    fn committed(mut self) {
        self.0 = None;
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A finished download, still under its part name, with its target lock.
struct Fetched {
    part: PartFile,
    got: Downloaded,
    published: Option<String>,
    url: String,
    _lock: Arc<TargetLock>,
}

impl Fetched {
    /// Read the part file back from disk and check it against the stream
    /// hash and the published checksum. Returns the bytes and their hash.
    fn verify(&self, file: &str) -> Result<(Vec<u8>, String), AssetError> {
        let path = self.part.path();
        let data = std::fs::read(path).map_err(AssetError::io("read", path))?;
        let actual = sha256_hex(&data);
        if data.len() as u64 != self.got.size || actual != self.got.sha256 {
            return Err(AssetError::DiskMismatch { file: file.into() });
        }
        if let Some(expected) = &self.published {
            if *expected != actual {
                return Err(AssetError::ChecksumMismatch {
                    file: file.into(),
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        Ok((data, actual))
    }
}

/// Target file of a part file name (`<target>.<pid>.<n>.part`).
fn part_target(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(PART_SUFFIX)?;
    let (rest, n) = stem.rsplit_once('.')?;
    let (target, pid) = rest.rsplit_once('.')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(n) && digits(pid) && !target.is_empty()).then_some(target)
}

/// The lock that guards writing `target` in the cache directory named
/// `dir_name`: kernel files are written under their version's tarball lock.
fn guarding_lock(dir_name: &str, target: &str) -> String {
    let is_kernel = target.starts_with("kernel-") && target.ends_with(".elf");
    match Version::parse(dir_name) {
        Ok(v) if is_kernel => urls::tarball_name(&v),
        _ => target.to_string(),
    }
}

/// Remove part files under `root/<dir>/` older than `age` whose guarding
/// lock is free. Errors are ignored: this is housekeeping.
pub(crate) fn remove_stale_parts(root: &Path, age: Duration) {
    let Ok(dirs) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for dir in dirs.flatten() {
        let Ok(kind) = dir.file_type() else { continue };
        if !kind.is_dir() || dir.file_name() == LOCK_DIR {
            continue;
        }
        let cache = CacheDir::new(dir.path());
        let dir_name = dir.file_name().to_string_lossy().into_owned();
        for entry in std::fs::read_dir(dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name();
            let Some(target) = name.to_str().and_then(part_target) else {
                continue;
            };
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| now.duration_since(t).is_ok_and(|d| d >= age));
            if !old || !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let guard = guarding_lock(&dir_name, target);
            if let Ok(Some(_free)) = lock::try_lock_file(&cache.lock_path(&guard)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

impl Assets {
    /// An asset manager caching under `root` (created on first write).
    ///
    /// Part files left behind by a crash (older than [`STALE_PART_AGE`]
    /// and not being written) are removed. Inside a tokio runtime this
    /// cleanup runs as a background blocking task, so `new` never does file
    /// work on an async thread; outside a runtime it runs before `new`
    /// returns. Either way it is best-effort and never fails `new`.
    pub fn new(root: impl Into<PathBuf>, cfg: HttpConfig) -> Result<Self, AssetError> {
        let root = root.into();
        let cleanup_root = root.clone();
        let cleanup = move || remove_stale_parts(&cleanup_root, STALE_PART_AGE);
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => drop(rt.spawn_blocking(cleanup)),
            Err(_) => cleanup(),
        }
        Ok(Self {
            root,
            http: Http::new(&cfg)?,
            download_base: cfg.download_base.trim_end_matches('/').to_string(),
            upgrade_base: cfg.upgrade_base.trim_end_matches('/').to_string(),
            max_download: cfg.max_download_bytes,
            events: None,
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
    /// A channel's answer is remembered in [`CHANNELS_FILE`]
    /// (best-effort: failing to write it does not fail the resolution).
    pub async fn resolve(&self, spec: &VersionSpec) -> Result<Version, AssetError> {
        match spec {
            VersionSpec::Exact(v) => Ok(v.clone()),
            VersionSpec::Channel(c) => {
                let version = self.latest(*c).await?.version;
                let (root, channel, v) = (self.root.clone(), *c, version.clone());
                let _ = blocking(move || remember_channel(&root, channel, &v)).await;
                Ok(version)
            }
        }
    }

    /// The version `channel` last resolved to (by any `Assets` on this
    /// cache root), if known. Needs no network.
    pub async fn remembered(&self, channel: Channel) -> Option<Version> {
        let root = self.root.clone();
        blocking(move || read_channels(&root).remove(channel.as_str()))
            .await
            .ok()
            .flatten()
            .and_then(|v| Version::parse(&v).ok())
    }

    /// The published SHA-256 of `file` at `url`. From 7.15 on a checksum
    /// must exist (`checksum_missing` otherwise); for older versions `None`
    /// means none is published. A present checksum is always enforced.
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

    /// Download `file` of `version` into a fresh part file of `dir` while
    /// holding `lock`, then fetch its published checksum. The part file is
    /// removed on any failure or cancellation.
    async fn fetch(
        &self,
        dir: &CacheDir,
        version: &Version,
        file: &str,
        lock: Arc<TargetLock>,
    ) -> Result<Fetched, AssetError> {
        {
            let dir = dir.clone();
            blocking(move || dir.create()).await??;
        }
        let url = urls::file(&self.download_base, version, file);
        let part = PartFile(Some(dir.part_path(file)));
        let got = self
            .http
            .download(&url, part.path(), file, &|e| self.emit(e))
            .await?;
        let published = self.published_checksum(version, &url, file).await?;
        Ok(Fetched {
            part,
            got,
            published,
            url,
            _lock: lock,
        })
    }

    /// Boot kernels for every [`BootArch`], from the cache or carved out of
    /// `netinstall-<ver>.tar.gz` (which is not kept).
    pub async fn ensure_kernels(&self, version: &Version) -> Result<KernelSet, AssetError> {
        let dir = self.version_dir(version);
        let tarball = urls::tarball_name(version);
        let cached = {
            let dir = dir.clone();
            blocking(move || load_kernels(&dir)).await?
        };
        if let Some(set) = cached {
            self.emit(AssetEvent::CacheHit { file: tarball });
            return Ok(set);
        }
        let lock = lock::lock_target(dir.lock_path(&tarball)).await?;
        let cached = {
            let dir = dir.clone();
            blocking(move || load_kernels(&dir)).await?
        };
        if let Some(set) = cached {
            self.emit(AssetEvent::CacheHit { file: tarball });
            return Ok(set);
        }
        let fetched = self.fetch(&dir, version, &tarball, lock).await?;
        blocking(move || {
            let (data, _) = fetched.verify(&tarball)?;
            let cli = archive::netinstall_cli_from_tar_gz(&data[..])?;
            drop(data);
            let set = extract_kernels(&cli)?;
            let mut entries = Vec::with_capacity(set.len());
            for (arch, image) in &set {
                let name = kernel_file_name(*arch);
                dir.write_atomic(&name, image)?;
                let entry = Entry::new(image, fetched.url.clone(), fetched.published.is_some());
                entries.push((name, entry));
            }
            dir.update(|index| index.files.extend(entries))?;
            drop(fetched);
            Ok(set)
        })
        .await?
    }
}

impl Assets {
    /// Versions whose complete, undamaged kernel set is in the cache,
    /// oldest first: what can still be booted without the network.
    pub async fn cached_versions(&self) -> Result<Vec<Version>, AssetError> {
        let root = self.root.clone();
        blocking(move || {
            let mut versions: Vec<Version> = std::fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| Version::parse(e.file_name().to_str()?).ok())
                .filter(|v| load_kernels(&CacheDir::new(root.join(v.as_str()))).is_some())
                .collect();
            versions.sort();
            versions
        })
        .await
    }
}

/// The channel file, or an empty map if it is missing or damaged.
fn read_channels(root: &Path) -> BTreeMap<String, String> {
    crate::cache::read_capped(&root.join(CHANNELS_FILE), MAX_CHANNELS_SIZE)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Record `channel` → `version`: read-merge-write under the file's lock,
/// written atomically (part file + rename).
fn remember_channel(root: &Path, channel: Channel, version: &Version) -> Result<(), AssetError> {
    let dir = CacheDir::new(root.to_path_buf());
    dir.create()?;
    let _lock = lock::lock_file_blocking(&root.join(LOCK_DIR).join("channels.json.lock"))?;
    let mut map = read_channels(root);
    if map.get(channel.as_str()).map(String::as_str) == Some(version.as_str()) {
        return Ok(());
    }
    map.insert(channel.as_str().to_string(), version.as_str().to_string());
    let json = serde_json::to_vec_pretty(&map).expect("map serializes");
    dir.write_atomic(CHANNELS_FILE, &json)
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
    use std::time::Duration;

    #[test]
    fn default_root_is_under_library_caches() {
        let root = default_cache_root().expect("HOME is set in tests");
        assert!(root.ends_with("Library/Caches/io.github.sepsig.mac-netinstall"));
    }

    /// The engine runs these in spawned tasks, so they must be `Send`.
    #[test]
    fn futures_are_send() {
        fn send<T: Send>(_: T) {}
        let root = tempdir("send");
        let a = Assets::new(&*root, HttpConfig::default()).unwrap();
        let spec = VersionSpec::Channel(Channel::Stable);
        send(a.resolve(&spec));
        send(a.ensure_kernels(&v()));
        send(a.cached_versions());
        send(a.available_packages(&v(), crate::PkgArch::Arm64));
        send(a.ensure_packages(&v(), crate::PkgArch::Arm64, &[]));
        send(a.register_local_file(Path::new("setup.rsc")));
        send(a.local_file("setup.rsc"));
        send(a.clear_downloads());
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
    async fn channel_resolutions_are_remembered() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let root = tempdir("remember");
        let a = assets(&srv, &root);
        assert_eq!(a.remembered(Channel::Stable).await, None);
        let stable = VersionSpec::Channel(Channel::Stable);
        assert_eq!(a.resolve(&stable).await.unwrap(), v());
        assert_eq!(a.remembered(Channel::Stable).await, Some(v()));
        assert_eq!(a.remembered(Channel::Testing).await, None);
        // Exact versions and failed resolutions record nothing.
        let exact = VersionSpec::Exact(Version::parse("7.24.6").unwrap());
        a.resolve(&exact).await.unwrap();
        a.resolve(&VersionSpec::Channel(Channel::Testing))
            .await
            .unwrap_err();
        assert_eq!(a.remembered(Channel::Testing).await, None);
        // Another instance on the same root (e.g. offline) sees it.
        let offline = Assets::new(&*root, HttpConfig::default()).unwrap();
        assert_eq!(offline.remembered(Channel::Stable).await, Some(v()));
        let text = std::fs::read_to_string(root.join(CHANNELS_FILE)).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            serde_json::json!({"stable": V})
        );
        // A damaged file is ignored, and replaced by the next resolution.
        std::fs::write(root.join(CHANNELS_FILE), b"{oops").unwrap();
        assert_eq!(a.remembered(Channel::Stable).await, None);
        a.resolve(&stable).await.unwrap();
        assert_eq!(a.remembered(Channel::Stable).await, Some(v()));
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

    #[tokio::test]
    async fn two_instances_carving_kernels_at_once_agree() {
        let srv = TestServer::start().await;
        let ver = v();
        let (tarball, _, _) = mirror(&srv);
        srv.route(
            &format!("/dl/{V}/netinstall-{V}.tar.gz"),
            Reply::Slow {
                body: tarball.clone(),
                chunk: (tarball.len() / 10).max(1),
                delay: Duration::from_millis(20),
            },
        );
        let root = tempdir("race-kernels");
        let (a, b) = (assets(&srv, &root), assets(&srv, &root));
        let late_b = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            b.ensure_kernels(&ver).await
        };
        let (ra, rb) = tokio::join!(a.ensure_kernels(&ver), late_b);
        let (ra, rb) = (ra.unwrap(), rb.unwrap());
        assert_eq!(ra, rb);
        for (arch, image) in synthetic_kernels() {
            assert_eq!(ra[&arch].as_ref(), image.as_slice(), "{arch}");
            let on_disk = std::fs::read(root.join(V).join(kernel_file_name(arch))).unwrap();
            assert_eq!(on_disk, image, "{arch}");
        }
        let downloads = srv
            .paths()
            .iter()
            .filter(|p| p.ends_with(".tar.gz"))
            .count();
        assert_eq!(downloads, 1);
        assert_eq!(parts(&root), Vec::<String>::new());
    }

    #[test]
    fn part_targets_are_parsed_from_unique_names() {
        assert_eq!(part_target("a.npk.12.3.part"), Some("a.npk"));
        assert_eq!(part_target("index.json.1.0.part"), Some("index.json"));
        assert_eq!(part_target("a.npk.part"), None);
        assert_eq!(part_target("a.npk.x.3.part"), None);
        assert_eq!(part_target(".1.2.part"), None);
    }

    #[tokio::test]
    async fn stale_part_files_from_a_crash_are_removed_at_startup() {
        let srv = TestServer::start().await;
        let root = tempdir("stale");
        let dir = CacheDir::new(root.join(V));
        dir.create().unwrap();
        let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 3600);
        let make = |name: &str, old: bool| {
            let path = dir.path(name);
            let f = std::fs::File::create(&path).unwrap();
            if old {
                f.set_modified(two_hours_ago).unwrap();
            }
            path
        };
        let stale = make("routeros-7.25.1-arm64.npk.4242.0.part", true);
        let fresh = make("wireless-7.25.1-arm64.npk.4242.1.part", false);
        let busy = make("container-7.25.1-arm64.npk.4242.2.part", true);
        let other = make("notes.txt", true);
        let _held = lock::try_lock_file(&dir.lock_path("container-7.25.1-arm64.npk"))
            .unwrap()
            .unwrap();
        let _a = assets(&srv, &root);
        // The cleanup runs in the background inside a runtime.
        for _ in 0..200 {
            if !stale.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!stale.exists(), "old and unlocked: removed");
        assert!(fresh.exists(), "may still be written");
        assert!(busy.exists(), "its target is locked");
        assert!(other.exists(), "not a part file");
    }

    #[test]
    fn outside_a_runtime_the_cleanup_runs_before_new_returns() {
        let root = tempdir("stale-sync");
        let dir = CacheDir::new(root.join(V));
        dir.create().unwrap();
        let part = dir.path("a.npk.1.0.part");
        let f = std::fs::File::create(&part).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(2 * 3600))
            .unwrap();
        Assets::new(&*root, HttpConfig::default()).unwrap();
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn cached_versions_lists_complete_kernel_sets_oldest_first() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_version(&srv, "7.9");
        mirror_version(&srv, "7.24.6");
        let root = tempdir("cached-versions");
        let a = assets(&srv, &root);
        assert_eq!(a.cached_versions().await.unwrap(), Vec::<Version>::new());
        for ver in [V, "7.9", "7.24.6"] {
            a.ensure_kernels(&Version::parse(ver).unwrap())
                .await
                .unwrap();
        }
        std::fs::write(root.join("7.24.6/kernel-tile.elf"), b"bad").unwrap();
        std::fs::create_dir_all(root.join("7.20")).unwrap();
        std::fs::create_dir_all(root.join("not-a-version")).unwrap();
        let got = a.cached_versions().await.unwrap();
        let got: Vec<&str> = got.iter().map(Version::as_str).collect();
        assert_eq!(got, ["7.9", V]);
    }

    #[test]
    fn kernel_parts_follow_the_tarball_lock() {
        let root = tempdir("kernel-parts");
        let dir = CacheDir::new(root.join(V));
        dir.create().unwrap();
        let part = dir.path("kernel-arm.elf.4242.0.part");
        std::fs::write(&part, b"half").unwrap();
        let tarball = format!("netinstall-{V}.tar.gz");
        let held = lock::try_lock_file(&dir.lock_path(&tarball))
            .unwrap()
            .unwrap();
        remove_stale_parts(&root, Duration::ZERO);
        assert!(part.exists(), "written under the tarball lock");
        drop(held);
        remove_stale_parts(&root, Duration::ZERO);
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn a_present_checksum_is_enforced_before_7_15_too() {
        let srv = TestServer::start().await;
        let old = "7.14.3";
        mirror_version(&srv, old);
        let tarball = format!("netinstall-{old}.tar.gz");
        srv.route(
            &format!("/dl/{old}/{tarball}.sha256"),
            sum_file(b"other bytes", &tarball),
        );
        let root = tempdir("old-sum-enforced");
        let err = assets(&srv, &root)
            .ensure_kernels(&Version::parse(old).unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "checksum_mismatch");
    }
}
