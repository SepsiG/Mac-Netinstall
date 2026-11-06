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
//! Files are written to `<name>.part` in the same directory, synced, then
//! renamed, so a crash never leaves a half-written file under its final
//! name. A file whose size or SHA-256 no longer matches its index entry is
//! treated as missing. A missing or unreadable index is treated as empty.

use crate::error::AssetError;
use crate::npk::sha256_hex;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
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

/// One cache directory.
#[derive(Debug, Clone)]
pub struct CacheDir {
    dir: PathBuf,
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

    /// The index, or an empty one if it is missing or unreadable.
    pub fn load(&self) -> Index {
        std::fs::read(self.path(INDEX_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, index: &Index) -> Result<(), AssetError> {
        let json = serde_json::to_vec_pretty(index).expect("index serializes");
        self.write_atomic(INDEX_FILE, &json)
    }

    /// Write `data` as `name` via `name.part` + rename.
    pub fn write_atomic(&self, name: &str, data: &[u8]) -> Result<(), AssetError> {
        self.create()?;
        let part = self.path(&format!("{name}.part"));
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::File::create(&part)?;
            f.write_all(data)?;
            f.sync_all()
        };
        if let Err(e) = write() {
            let _ = std::fs::remove_file(&part);
            return Err(AssetError::io("write", &part)(e));
        }
        self.commit(&part, name)
    }

    /// Rename a finished `part` file to `name`.
    pub fn commit(&self, part: &Path, name: &str) -> Result<(), AssetError> {
        let dest = self.path(name);
        std::fs::rename(part, &dest).map_err(AssetError::io("rename", &dest))
    }

    /// Read `name` and check it against its index entry.
    pub fn lookup(&self, index: &Index, name: &str) -> Lookup {
        let Some(entry) = index.files.get(name) else {
            return Lookup::Missing;
        };
        let Ok(data) = std::fs::read(self.path(name)) else {
            return Lookup::Missing;
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
        assert!(!cache.path("a.bin.part").exists());
        assert!(!cache.path("index.json.part").exists());
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
