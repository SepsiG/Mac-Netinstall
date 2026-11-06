//! On-disk cache: one directory per version (plus `local/`), each with a
//! JSON index of the files it holds.
//!
//! ```text
//! <root>/7.25.1/index.json
//! <root>/7.25.1/kernel-arm64.elf
//! <root>/7.25.1/routeros-7.25.1-arm64.npk
//! <root>/local/index.json
//! <root>/local/my-config.rsc
//! ```
//!
//! Files are written to a unique `<name>.<pid>.<n>.part` in the same
//! directory (opened with `create_new`), synced, verified, then renamed and
//! the directory synced, so a crash never leaves a half-written file under
//! its final name; stale part files are cleaned up by
//! [`crate::Assets::new`]. A file whose size or SHA-256 no longer matches
//! its index entry is treated as missing. A missing or unreadable index is
//! treated as empty. The index is changed only by read-merge-write under a
//! lock ([`CacheDir::update`]), and lock files live in `<root>/.locks/`
//! ([`CacheDir::lock_path`]).

use crate::error::AssetError;
use crate::lock;
use crate::npk::sha256_hex;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Index file name inside each cache directory.
pub const INDEX_FILE: &str = "index.json";

/// What we know about one cached file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub size: u64,
    /// Lowercase hex SHA-256 of the cached file.
    pub sha256: String,
    /// Where it came from: a download URL, or `file://<path>` for local files.
    pub source: String,
    /// `true` if the downloaded file matched the checksum MikroTik publishes
    /// (`<url>.sha256`); for kernels, the checksum of the tarball they were
    /// carved from.
    pub published_checksum: bool,
    /// Seconds since the Unix epoch.
    pub fetched_at: u64,
}

impl Entry {
    pub fn new(data: &[u8], source: impl Into<String>, published_checksum: bool) -> Self {
        Self {
            size: data.len() as u64,
            sha256: sha256_hex(data),
            source: source.into(),
            published_checksum,
            fetched_at: unix_now(),
        }
    }
}

/// Contents of `index.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub files: BTreeMap<String, Entry>,
    /// Extra package names per package architecture.
    #[serde(default)]
    pub package_lists: BTreeMap<String, Vec<String>>,
}

/// Result of looking up a cached file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Present and matching its index entry.
    Valid(Bytes),
    /// Not in the index, or the file is gone.
    Missing,
    /// The file differs from its index entry (size or SHA-256).
    Corrupt,
}

/// Largest `index.json` read (real ones are a few KiB).
const MAX_INDEX_SIZE: u64 = 16 << 20;
/// Largest cached file read back (the netinstall tarball is ~46 MB).
const MAX_CACHED_FILE: u64 = 1 << 30;
/// Directory under the cache root that holds lock files.
pub const LOCK_DIR: &str = ".locks";
/// Lock name (see [`CacheDir::lock_path`]) of a whole cache directory:
/// everything that writes into it holds it shared; deleting the directory
/// takes it exclusively.
pub const DIR_LOCK: &str = "_dir";
/// Suffix of temporary files.
pub const PART_SUFFIX: &str = ".part";

/// One cache directory.
#[derive(Debug, Clone)]
pub struct CacheDir {
    dir: PathBuf,
}

/// Read at most `max` bytes of `path`; `None` if it cannot be read or is
/// longer.
pub(crate) fn read_capped(path: &Path, max: u64) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut data = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut data)
        .ok()?;
    (data.len() as u64 <= max).then_some(data)
}

impl CacheDir {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Create the directory (and parents) if needed.
    pub fn create(&self) -> Result<(), AssetError> {
        std::fs::create_dir_all(&self.dir).map_err(AssetError::io("create", &self.dir))
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// A temporary file name for `name` that no other writer uses:
    /// `<name>.<pid>.<counter>.part`.
    pub fn part_path(&self, name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        self.path(&format!("{name}.{}.{n}{PART_SUFFIX}", std::process::id()))
    }

    /// Lock file for target `name` of this directory:
    /// `<parent>/.locks/<dir name>-<name>.lock`, outside the directory so
    /// deleting the directory never deletes a lock in use.
    pub fn lock_path(&self, name: &str) -> PathBuf {
        let dir_name = self
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parent = self.dir.parent().unwrap_or(&self.dir);
        parent
            .join(LOCK_DIR)
            .join(format!("{dir_name}-{name}.lock"))
    }

    /// The index, or an empty one if it is missing, unreadable or larger
    /// than 16 MiB.
    pub fn load(&self) -> Index {
        read_capped(&self.path(INDEX_FILE), MAX_INDEX_SIZE)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Replace the index. Prefer [`CacheDir::update`], which does not lose
    /// concurrent changes.
    pub fn save(&self, index: &Index) -> Result<(), AssetError> {
        let json = serde_json::to_vec_pretty(index).expect("index serializes");
        self.write_atomic(INDEX_FILE, &json)
    }

    /// Read-modify-write the index under its lock (blocks the thread for
    /// the short critical section), so concurrent writers in this and
    /// other processes never drop each other's entries.
    pub fn update(&self, change: impl FnOnce(&mut Index)) -> Result<(), AssetError> {
        let _dir = lock::lock_shared_blocking(&self.lock_path(DIR_LOCK))?;
        let _lock = lock::lock_file_blocking(&self.lock_path(INDEX_FILE))?;
        let mut index = self.load();
        change(&mut index);
        self.save(&index)
    }

    /// Write `data` as `name` via a unique part file + rename.
    pub fn write_atomic(&self, name: &str, data: &[u8]) -> Result<(), AssetError> {
        self.create()?;
        let part = self.part_path(name);
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&part)?;
            f.write_all(data)?;
            f.sync_all()
        };
        if let Err(e) = write() {
            let _ = std::fs::remove_file(&part);
            return Err(AssetError::io("write", &part)(e));
        }
        self.commit(&part, name).inspect_err(|_| {
            let _ = std::fs::remove_file(&part);
        })
    }

    /// Rename a finished `part` file to `name`, then sync the directory so
    /// the rename survives a power loss.
    pub fn commit(&self, part: &Path, name: &str) -> Result<(), AssetError> {
        let dest = self.path(name);
        std::fs::rename(part, &dest).map_err(AssetError::io("rename", &dest))?;
        std::fs::File::open(&self.dir)
            .and_then(|d| d.sync_all())
            .map_err(AssetError::io("sync", &self.dir))
    }

    /// Read `name` and check it against its index entry (reads at most
    /// the entry's size + 1 bytes).
    pub fn lookup(&self, index: &Index, name: &str) -> Lookup {
        let Some(entry) = index.files.get(name) else {
            return Lookup::Missing;
        };
        let path = self.path(name);
        if !path.is_file() {
            return Lookup::Missing;
        }
        let Some(data) = read_capped(&path, entry.size.min(MAX_CACHED_FILE)) else {
            return Lookup::Corrupt;
        };
        if data.len() as u64 != entry.size || sha256_hex(&data) != entry.sha256 {
            return Lookup::Corrupt;
        }
        Lookup::Valid(Bytes::from(data))
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;

    #[test]
    fn stores_and_verifies_files() {
        let tmp = tempdir("cache");
        let cache = CacheDir::new(tmp.join("7.25.1"));
        let mut index = cache.load();
        assert_eq!(index, Index::default());
        cache.write_atomic("a.bin", b"hello").unwrap();
        index
            .files
            .insert("a.bin".into(), Entry::new(b"hello", "http://x/a.bin", true));
        cache.save(&index).unwrap();

        let index = cache.load();
        assert_eq!(index.files["a.bin"].size, 5);
        assert_eq!(
            cache.lookup(&index, "a.bin"),
            Lookup::Valid(Bytes::from_static(b"hello"))
        );
        assert_eq!(cache.lookup(&index, "b.bin"), Lookup::Missing);
        let names: Vec<_> = std::fs::read_dir(tmp.join("7.25.1"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "no part files left: {names:?}");
    }

    #[test]
    fn detects_changed_and_deleted_files() {
        let tmp = tempdir("cache");
        let cache = CacheDir::new(tmp.to_path_buf());
        let mut index = Index::default();
        cache.write_atomic("a.bin", b"hello").unwrap();
        index
            .files
            .insert("a.bin".into(), Entry::new(b"hello", "x", false));
        std::fs::write(cache.path("a.bin"), b"jello").unwrap();
        assert_eq!(cache.lookup(&index, "a.bin"), Lookup::Corrupt);
        std::fs::write(cache.path("a.bin"), b"hello!").unwrap();
        assert_eq!(cache.lookup(&index, "a.bin"), Lookup::Corrupt);
        std::fs::remove_file(cache.path("a.bin")).unwrap();
        assert_eq!(cache.lookup(&index, "a.bin"), Lookup::Missing);
    }

    #[test]
    fn concurrent_updates_keep_every_entry() {
        let tmp = tempdir("cache-update");
        let cache = CacheDir::new(tmp.join("7.25.1"));
        std::thread::scope(|s| {
            for t in 0..8 {
                let cache = &cache;
                s.spawn(move || {
                    for i in 0..10 {
                        cache
                            .update(|index| {
                                let name = format!("f{t}-{i}");
                                index.files.insert(name, Entry::new(b"x", "s", false));
                            })
                            .unwrap();
                    }
                });
            }
        });
        assert_eq!(cache.load().files.len(), 80);
    }

    #[test]
    fn lookup_reads_no_more_than_the_entry_size() {
        let tmp = tempdir("cache-cap");
        let cache = CacheDir::new(tmp.to_path_buf());
        cache.write_atomic("a.bin", &vec![1; 1000]).unwrap();
        let mut index = Index::default();
        index
            .files
            .insert("a.bin".into(), Entry::new(&[1; 10], "x", false));
        assert_eq!(cache.lookup(&index, "a.bin"), Lookup::Corrupt);
        std::fs::create_dir(cache.path("d.bin")).unwrap();
        index
            .files
            .insert("d.bin".into(), Entry::new(b"", "x", false));
        assert_eq!(cache.lookup(&index, "d.bin"), Lookup::Missing);
    }

    #[test]
    fn part_names_are_unique_and_locks_live_outside_the_directory() {
        let cache = CacheDir::new(PathBuf::from("/c/7.25.1"));
        assert_ne!(cache.part_path("a.npk"), cache.part_path("a.npk"));
        let part = cache.part_path("a.npk");
        let name = part.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with(&format!("a.npk.{}.", std::process::id())),
            "{name}"
        );
        assert!(name.ends_with(".part"));
        assert_eq!(
            cache.lock_path("a.npk"),
            PathBuf::from("/c/.locks/7.25.1-a.npk.lock")
        );
    }

    #[test]
    fn unreadable_index_counts_as_empty() {
        let tmp = tempdir("cache");
        let cache = CacheDir::new(tmp.to_path_buf());
        cache.write_atomic(INDEX_FILE, b"{not json").unwrap();
        assert_eq!(cache.load(), Index::default());
    }

    #[test]
    fn index_json_shape_is_stable() {
        let mut index = Index::default();
        index.files.insert(
            "a.npk".into(),
            Entry {
                size: 1,
                sha256: "ab".into(),
                source: "https://x/a.npk".into(),
                published_checksum: true,
                fetched_at: 7,
            },
        );
        index
            .package_lists
            .insert("arm".into(), vec!["wireless".into()]);
        let json: serde_json::Value = serde_json::to_value(&index).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "files": {"a.npk": {"size": 1, "sha256": "ab", "source": "https://x/a.npk",
                                     "published_checksum": true, "fetched_at": 7}},
                "package_lists": {"arm": ["wireless"]}
            })
        );
    }
}
