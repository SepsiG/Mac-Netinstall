//! What is in the cache, file by file, and deleting parts of it.
//!
//! [`Assets::cached_inventory`] lists every version directory (its boot
//! kernels as one item, each package, anything else in its index), part
//! files left by interrupted downloads, and registered local files. It is
//! quick: sizes are compared with the index, and SHA-256 results of files
//! verified before (same size and modification time) are reused; other
//! files are `unchecked`. [`Assets::verify_cache`] hashes every file. Items are named by path-free ids
//! (`<version>/kernels`, `<version>/<file>`, `local/<file>`,
//! `partial/<dir>/<file>`), so no file system path reaches a UI.
//!
//! [`Assets::delete_cached`] removes one item, one version, everything
//! downloaded, or every damaged item. It takes each target's lock without
//! waiting (`busy` if a download holds it), updates the index under its
//! lock, removes the target's part files, and never follows a symlink out
//! of the cache root (a symlink is only unlinked).
//!
//! A version directory with nothing in it (no index entry, no part file;
//! e.g. after its last item was deleted) is never listed, and listings and
//! deletions remove it, unless a download is starting in it.

use super::{blocking, part_target, Assets, LOCAL_DIR};
use crate::cache::{CacheDir, Index, Lookup, DIR_LOCK, INDEX_FILE, LOCK_DIR, PART_SUFFIX};
use crate::error::AssetError;
use crate::lock::{self, FileLock};
use crate::urls;
use crate::version::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// A remembered SHA-256 check of one file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Check {
    size: u64,
    modified: SystemTime,
    valid: bool,
}

/// Checks by file path, shared by one [`Assets`].
pub(crate) type Checks = Arc<Mutex<HashMap<PathBuf, Check>>>;

/// Files hashed by inventories, per cache root (tests count them; per root
/// so tests running in parallel do not see each other's).
#[cfg(test)]
static HASHES: Mutex<std::collections::BTreeMap<PathBuf, u64>> =
    Mutex::new(std::collections::BTreeMap::new());

/// Count one hashed `file` (`<root>/<dir>/<file>`) for the tests.
fn count_hash(_file: &Path) {
    #[cfg(test)]
    if let Some(root) = _file.parent().and_then(Path::parent) {
        let mut counts = HASHES.lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry(root.to_path_buf()).or_default() += 1;
    }
}

#[cfg(test)]
fn hashes(root: &Path) -> u64 {
    let counts = HASHES.lock().unwrap_or_else(|e| e.into_inner());
    counts.get(root).copied().unwrap_or(0)
}

/// How a cached file checks out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Integrity {
    /// Matches its index entry, which matched MikroTik's published SHA-256.
    Verified,
    /// Matches its index entry; nothing was published to compare with
    /// (size and package header were checked when it was stored).
    Unpublished,
    /// The size matches; the SHA-256 was not checked yet (see
    /// [`Assets::verify_cache`]).
    Unchecked,
    /// Size or SHA-256 differs from the index entry: damaged or truncated.
    Corrupt,
    /// In the index, but the file is gone (or a kernel set is incomplete).
    Missing,
    /// A part file of an interrupted download.
    Partial,
}

impl Integrity {
    /// Corrupt, missing or partial: worth deleting.
    pub fn damaged(self) -> bool {
        matches!(
            self,
            Integrity::Corrupt | Integrity::Missing | Integrity::Partial
        )
    }

    /// The worse of two (for a kernel set).
    fn worst(self, other: Integrity) -> Integrity {
        let rank = |i: Integrity| match i {
            Integrity::Verified => 0,
            Integrity::Unpublished => 1,
            Integrity::Unchecked => 2,
            Integrity::Partial => 3,
            Integrity::Missing => 4,
            Integrity::Corrupt => 5,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// What an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// A version's boot kernel set (every `kernel-*.elf`).
    Kernels,
    /// A `.npk` package.
    Package,
    /// Anything else in a version's index.
    Other,
    /// A part file.
    Partial,
    /// A registered local file (`.rsc` / `.npk`).
    Local,
}

/// One entry of the inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedItem {
    /// Path-free id for [`DeleteScope::Item`].
    pub id: String,
    pub kind: ItemKind,
    /// File name (`kernels` for a kernel set, with `files` counting them).
    pub name: String,
    /// Package architecture (`arm64`, ...), for packages.
    pub arch: Option<String>,
    /// Bytes on disk.
    pub size: u64,
    pub integrity: Integrity,
    /// Files in the item (more than one for a kernel set).
    pub files: usize,
}

/// One version directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedVersion {
    pub version: String,
    pub size: u64,
    pub items: Vec<CachedItem>,
}

/// The whole cache.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    /// Oldest version first.
    pub versions: Vec<CachedVersion>,
    pub partial: Vec<CachedItem>,
    pub local: Vec<CachedItem>,
    /// Bytes of everything listed.
    pub total: u64,
}

impl Inventory {
    /// Every item, versions first.
    pub fn items(&self) -> impl Iterator<Item = &CachedItem> {
        self.versions
            .iter()
            .flat_map(|v| v.items.iter())
            .chain(&self.partial)
            .chain(&self.local)
    }
}

/// What [`Assets::delete_cached`] removes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum DeleteScope {
    /// One item by its id.
    Item { id: String },
    /// One version directory.
    Version { version: String },
    /// Every downloaded version and part file (local files are kept).
    All,
    /// Every corrupt, missing or partial item.
    Damaged,
}

fn is_kernel(name: &str) -> bool {
    name.starts_with("kernel-") && name.ends_with(".elf")
}

/// `wireless-7.25.1-arm64.npk` in version `7.25.1` → (`wireless`, `arm64`);
/// the v6 main package `routeros-powerpc-6.49.23.npk` → (`routeros`,
/// `ppc`).
fn package_parts<'a>(file: &'a str, version: &str) -> Option<(&'a str, &'a str)> {
    let stem = file.strip_suffix(".npk")?;
    if let Some(arch6) = stem
        .strip_suffix(version)
        .and_then(|r| r.strip_suffix('-'))
        .and_then(|r| r.strip_prefix("routeros-"))
    {
        let arch = if arch6 == "powerpc" { "ppc" } else { arch6 };
        return (!arch.is_empty()).then_some(("routeros", arch));
    }
    let (rest, arch) = stem.rsplit_once('-')?;
    let name = rest.strip_suffix(version)?.strip_suffix('-')?;
    Some((name, arch))
}

fn file_size(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).map_or(0, |m| if m.is_file() { m.len() } else { 0 })
}

/// How `name` of `dir` checks out against its index entry: sizes always;
/// SHA-256 when `verify` (remembered in `checks`), else a remembered
/// result for an unchanged file, else `Unchecked`.
fn integrity(
    checks: &Checks,
    verify: bool,
    dir: &CacheDir,
    index: &Index,
    name: &str,
) -> Integrity {
    let Some(entry) = index.files.get(name) else {
        return Integrity::Missing;
    };
    let path = dir.path(name);
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return Integrity::Missing;
    };
    if !meta.is_file() {
        return Integrity::Missing;
    }
    if meta.len() != entry.size {
        return Integrity::Corrupt;
    }
    let good = if entry.published_checksum {
        Integrity::Verified
    } else {
        Integrity::Unpublished
    };
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let mut memo = checks.lock().unwrap_or_else(|e| e.into_inner());
    if !verify {
        return match memo.get(&path) {
            Some(c) if c.size == entry.size && c.modified == modified => {
                if c.valid {
                    good
                } else {
                    Integrity::Corrupt
                }
            }
            _ => Integrity::Unchecked,
        };
    }
    count_hash(&path);
    let valid = matches!(dir.lookup(index, name), Lookup::Valid(_));
    memo.insert(
        path,
        Check {
            size: entry.size,
            modified,
            valid,
        },
    );
    if valid {
        good
    } else {
        Integrity::Corrupt
    }
}

fn version_inventory(
    root: &Path,
    version: &Version,
    checks: &Checks,
    verify: bool,
) -> CachedVersion {
    let v = version.as_str();
    let dir = CacheDir::new(root.join(v));
    let index = dir.load();
    let mut items = Vec::new();
    let kernels: Vec<&String> = index.files.keys().filter(|n| is_kernel(n)).collect();
    if !kernels.is_empty() {
        let mut status = Integrity::Verified;
        let mut size = 0;
        for name in &kernels {
            status = status.worst(integrity(checks, verify, &dir, &index, name));
            size += file_size(&dir.path(name));
        }
        items.push(CachedItem {
            id: format!("{v}/kernels"),
            kind: ItemKind::Kernels,
            name: "kernels".into(),
            arch: None,
            size,
            integrity: status,
            files: kernels.len(),
        });
    }
    for name in index.files.keys().filter(|n| !is_kernel(n)) {
        let (kind, arch) = match package_parts(name, v) {
            Some((_, arch)) => (ItemKind::Package, Some(arch.to_string())),
            None => (ItemKind::Other, None),
        };
        items.push(CachedItem {
            id: format!("{v}/{name}"),
            kind,
            name: name.clone(),
            arch,
            size: file_size(&dir.path(name)),
            integrity: integrity(checks, verify, &dir, &index, name),
            files: 1,
        });
    }
    CachedVersion {
        version: v.to_string(),
        size: items.iter().map(|i| i.size).sum(),
        items,
    }
}

/// Real directories under `root` (symlinks and files skipped).
fn subdirs(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .filter(|(n, _)| n != LOCK_DIR)
        .collect();
    out.sort();
    out
}

/// The version directories under `root`, oldest first.
fn version_dirs(root: &Path) -> Vec<Version> {
    let mut versions: Vec<Version> = subdirs(root)
        .iter()
        .filter_map(|(n, _)| Version::parse(n).ok())
        .collect();
    versions.sort();
    versions
}

/// Whether `dir` holds part files.
fn has_parts(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(PART_SUFFIX) && part_target(n).is_some())
        })
}

/// Remove `version`'s directory if nothing is in it; a directory being
/// downloaded into (`busy`) is left alone. Returns whether it was removed.
fn remove_if_empty(root: &Path, version: &Version) -> Result<bool, AssetError> {
    match delete_version(root, version, true) {
        Ok(None) => Ok(false),
        Ok(Some(_)) => Ok(true),
        Err(AssetError::Busy { .. }) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Everything in the cache. With `prune`, empty version directories are
/// removed on the way (errors ignored: they are not listed either way).
fn inventory(root: &Path, checks: &Checks, verify: bool, prune: bool) -> Inventory {
    let mut inv = Inventory::default();
    for v in &version_dirs(root) {
        let listed = version_inventory(root, v, checks, verify);
        if !listed.items.is_empty() {
            inv.versions.push(listed);
        } else if prune {
            let _ = remove_if_empty(root, v);
        }
    }
    for (dir_name, path) in subdirs(root) {
        for entry in std::fs::read_dir(&path).into_iter().flatten().flatten() {
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            if !name.ends_with(PART_SUFFIX) || part_target(&name).is_none() {
                continue;
            }
            inv.partial.push(CachedItem {
                id: format!("partial/{dir_name}/{name}"),
                kind: ItemKind::Partial,
                name: name.clone(),
                arch: None,
                size: file_size(&entry.path()),
                integrity: Integrity::Partial,
                files: 1,
            });
        }
    }
    let local = CacheDir::new(root.join(LOCAL_DIR));
    let index = local.load();
    for name in index.files.keys() {
        inv.local.push(CachedItem {
            id: format!("{LOCAL_DIR}/{name}"),
            kind: ItemKind::Local,
            name: name.clone(),
            arch: None,
            size: file_size(&local.path(name)),
            integrity: integrity(checks, verify, &local, &index, name),
            files: 1,
        });
    }
    inv.total = inv.items().map(|i| i.size).sum();
    inv
}

/// A single file name (no separators, no `..`, not the index).
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && name != INDEX_FILE
}

fn invalid(id: &str) -> AssetError {
    AssetError::InvalidItem { id: id.to_string() }
}

fn busy(id: &str) -> AssetError {
    AssetError::Busy { id: id.to_string() }
}

/// Take `lock_name`'s lock in `dir` without waiting.
fn try_hold(dir: &CacheDir, lock_name: &str, id: &str) -> Result<FileLock, AssetError> {
    lock::try_lock_file(&dir.lock_path(lock_name))?.ok_or_else(|| busy(id))
}

/// Remove a file or symlink (never what a symlink points to); a missing
/// one is fine. Returns the bytes freed.
fn remove(path: &Path) -> Result<u64, AssetError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            let size = if m.is_file() { m.len() } else { 0 };
            if m.is_dir() {
                std::fs::remove_dir_all(path).map_err(AssetError::io("remove", path))?;
            } else {
                std::fs::remove_file(path).map_err(AssetError::io("remove", path))?;
            }
            Ok(size)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(AssetError::io("remove", path)(e)),
    }
}

/// Part files of `target` in `dir` (stale or not: the caller holds the
/// target's lock, so nobody is writing them).
fn remove_parts_of(dir: &CacheDir, target: &str) -> Result<u64, AssetError> {
    let mut freed = 0;
    let Some(base) = dir.path(INDEX_FILE).parent().map(Path::to_path_buf) else {
        return Ok(0);
    };
    for entry in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let name = entry.file_name();
        if name.to_str().and_then(part_target) == Some(target) {
            freed += remove(&entry.path())?;
        }
    }
    Ok(freed)
}

/// Delete file `name` of `dir` (holding `lock_name`), its index entry and
/// part files.
fn delete_file(dir: &CacheDir, name: &str, lock_name: &str, id: &str) -> Result<u64, AssetError> {
    let _held = try_hold(dir, lock_name, id)?;
    let mut freed = remove(&dir.path(name))?;
    freed += remove_parts_of(dir, name)?;
    dir.update(|index| {
        index.files.remove(name);
    })?;
    Ok(freed)
}

/// Delete a version's kernel set (under the tarball's lock).
fn delete_kernels(root: &Path, version: &Version, id: &str) -> Result<u64, AssetError> {
    let dir = CacheDir::new(root.join(version.as_str()));
    let tarball = urls::tarball_name(version);
    let _held = try_hold(&dir, &tarball, id)?;
    let names: Vec<String> = dir
        .load()
        .files
        .keys()
        .filter(|n| is_kernel(n))
        .cloned()
        .collect();
    let mut freed = 0;
    for name in &names {
        freed += remove(&dir.path(name))?;
        freed += remove_parts_of(&dir, name)?;
    }
    freed += remove_parts_of(&dir, &tarball)?;
    dir.update(|index| index.files.retain(|n, _| !is_kernel(n)))?;
    Ok(freed)
}

/// Delete a whole version directory, holding every lock its writers use.
/// With `only_if_empty`, nothing is deleted (`None`) unless the directory
/// has no index entry and no part file (checked under the locks).
fn delete_version(
    root: &Path,
    version: &Version,
    only_if_empty: bool,
) -> Result<Option<u64>, AssetError> {
    let id = version.as_str();
    let path = root.join(id);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(0)),
        Err(e) => return Err(AssetError::io("remove", &path)(e)),
    };
    if !meta.is_dir() {
        if only_if_empty {
            return Ok(None);
        }
        return remove(&path).map(Some); // a symlink (or stray file): unlink only
    }
    let dir = CacheDir::new(path.clone());
    let index = dir.load();
    // Every download into the directory holds this shared: a download
    // running (or starting) now makes this `busy`, never half-deleted.
    let mut held = vec![try_hold(&dir, DIR_LOCK, id)?];
    held.push(try_hold(&dir, &urls::tarball_name(version), id)?);
    for name in index.files.keys().filter(|n| !is_kernel(n)) {
        held.push(try_hold(&dir, name, id)?);
    }
    let _index_lock = try_hold(&dir, INDEX_FILE, id)?;
    let listed = version_inventory(root, version, &Checks::default(), false);
    if only_if_empty && (!listed.items.is_empty() || has_parts(&path)) {
        return Ok(None);
    }
    std::fs::remove_dir_all(&path).map_err(AssetError::io("remove", &path))?;
    drop(held);
    Ok(Some(listed.size))
}

/// Delete the item `id`.
fn delete_item(root: &Path, id: &str) -> Result<u64, AssetError> {
    let mut parts = id.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("partial"), Some(dir_name), Some(file), None) => {
            if !plain_name(dir_name) || dir_name == LOCK_DIR || !plain_name(file) {
                return Err(invalid(id));
            }
            let Some(target) = part_target(file) else {
                return Err(invalid(id));
            };
            let dir = CacheDir::new(root.join(dir_name));
            let lock_name = match Version::parse(dir_name) {
                Ok(v) if is_kernel(target) => urls::tarball_name(&v),
                _ => target.to_string(),
            };
            let _held = try_hold(&dir, &lock_name, id)?;
            remove(&dir.path(file))
        }
        (Some(LOCAL_DIR), Some(file), None, None) if plain_name(file) => {
            let dir = CacheDir::new(root.join(LOCAL_DIR));
            if !dir.load().files.contains_key(file) {
                return Err(invalid(id));
            }
            delete_file(&dir, file, file, id)
        }
        (Some(v), Some(file), None, None) if plain_name(file) => {
            let version = Version::parse(v).map_err(|_| invalid(id))?;
            if file == "kernels" {
                return delete_kernels(root, &version, id);
            }
            let dir = CacheDir::new(root.join(version.as_str()));
            if !dir.load().files.contains_key(file) || is_kernel(file) {
                return Err(invalid(id));
            }
            delete_file(&dir, file, file, id)
        }
        _ => Err(invalid(id)),
    }
}

/// The version an item id belongs to (`None` for local and part files of
/// other directories).
pub fn item_version(id: &str) -> Option<Version> {
    let mut parts = id.split('/');
    match parts.next()? {
        "partial" => Version::parse(parts.next()?).ok(),
        first => Version::parse(first).ok(),
    }
}

impl Assets {
    /// Everything in the cache, quickly (see the module docs).
    pub async fn cached_inventory(&self) -> Result<Inventory, AssetError> {
        let (root, checks) = (self.root.clone(), self.checks.clone());
        blocking(move || inventory(&root, &checks, false, true)).await
    }

    /// Everything in the cache, every file's SHA-256 checked against its
    /// index entry (and remembered for later listings).
    pub async fn verify_cache(&self) -> Result<Inventory, AssetError> {
        let (root, checks) = (self.root.clone(), self.checks.clone());
        blocking(move || inventory(&root, &checks, true, true)).await
    }

    /// Delete `scope`; returns the bytes freed. Errors: `invalid_item`
    /// (`id`: not an item of this cache), `busy` (`id`: being downloaded
    /// right now), `io`. With several targets, the ones before an error
    /// are deleted.
    pub async fn delete_cached(&self, scope: DeleteScope) -> Result<u64, AssetError> {
        let (root, checks) = (self.root.clone(), self.checks.clone());
        blocking(move || -> Result<u64, AssetError> {
            match scope {
                DeleteScope::Item { id } => {
                    let freed = delete_item(&root, &id)?;
                    if let Some(v) = item_version(&id) {
                        remove_if_empty(&root, &v)?;
                    }
                    Ok(freed)
                }
                DeleteScope::Version { version } => {
                    let v = Version::parse(&version).map_err(|_| invalid(&version))?;
                    Ok(delete_version(&root, &v, false)?.unwrap_or(0))
                }
                DeleteScope::All => {
                    let inv = inventory(&root, &checks, false, false);
                    let mut freed = 0;
                    for version in version_dirs(&root) {
                        freed += delete_version(&root, &version, false)?.unwrap_or(0);
                    }
                    for item in &inv.partial {
                        freed += delete_item(&root, &item.id)?;
                    }
                    Ok(freed)
                }
                DeleteScope::Damaged => {
                    let inv = inventory(&root, &checks, false, false);
                    let mut freed = 0;
                    for item in inv.items().filter(|i| i.integrity.damaged()) {
                        freed += delete_item(&root, &item.id)?;
                    }
                    for version in version_dirs(&root) {
                        remove_if_empty(&root, &version)?;
                    }
                    Ok(freed)
                }
            }
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::PkgArch;

    #[test]
    fn package_file_names_give_name_and_arch() {
        assert_eq!(
            package_parts("wireless-7.25.1-arm64.npk", "7.25.1"),
            Some(("wireless", "arm64"))
        );
        assert_eq!(
            package_parts("routeros-powerpc-6.49.23.npk", "6.49.23"),
            Some(("routeros", "ppc"))
        );
        assert_eq!(
            package_parts("routeros-mipsbe-6.49.23.npk", "6.49.23"),
            Some(("routeros", "mipsbe"))
        );
        assert_eq!(
            package_parts("calea-6.49.23-ppc.npk", "6.49.23"),
            Some(("calea", "ppc"))
        );
        assert_eq!(package_parts("routeros--6.49.23.npk", "6.49.23"), None);
        assert_eq!(package_parts("notes.txt", "6.49.23"), None);
    }
    use crate::testutil::*;

    async fn filled(name: &str) -> (TestServer, TempDir, Assets) {
        let srv = TestServer::start().await;
        mirror(&srv);
        let root = tempdir(name);
        let a = assets(&srv, &root);
        a.ensure_kernels(&v()).await.unwrap();
        a.ensure_packages(&v(), PkgArch::Arm64, &["wireless".to_string()])
            .await
            .unwrap();
        (srv, root, a)
    }

    fn item<'a>(inv: &'a Inventory, id: &str) -> &'a CachedItem {
        inv.items()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("{id} in {inv:#?}"))
    }

    #[tokio::test]
    async fn the_inventory_lists_kernels_packages_and_their_state() {
        let (_srv, root, a) = filled("inv-list").await;
        let inv = a.verify_cache().await.unwrap();
        assert_eq!(inv.versions.len(), 1);
        let ver = &inv.versions[0];
        assert_eq!(ver.version, "7.25.1");
        let kernels = item(&inv, "7.25.1/kernels");
        assert_eq!((kernels.kind, kernels.files), (ItemKind::Kernels, 8));
        assert_eq!(kernels.integrity, Integrity::Verified);
        let main = item(&inv, "7.25.1/routeros-7.25.1-arm64.npk");
        assert_eq!(main.kind, ItemKind::Package);
        assert_eq!(main.arch.as_deref(), Some("arm64"));
        assert!(main.size > 0);
        assert!(!main.integrity.damaged());
        assert_eq!(inv.total, ver.size);
        // No paths anywhere.
        let json = serde_json::to_string(&inv).unwrap();
        assert!(!json.contains(root.to_str().unwrap()), "{json}");
    }

    #[tokio::test]
    async fn truncated_changed_missing_and_partial_files_are_flagged() {
        let (_srv, root, a) = filled("inv-damage").await;
        let dir = root.join("7.25.1");
        // Truncated.
        let main = dir.join("routeros-7.25.1-arm64.npk");
        let data = std::fs::read(&main).unwrap();
        std::fs::write(&main, &data[..data.len() / 2]).unwrap();
        // Same size, other bytes: a SHA-256 mismatch.
        let wireless = dir.join("wireless-7.25.1-arm64.npk");
        let mut w = std::fs::read(&wireless).unwrap();
        let last = w.len() - 1;
        w[last] ^= 0xff;
        std::fs::write(&wireless, &w).unwrap();
        // A kernel gone.
        std::fs::remove_file(dir.join("kernel-arm64.elf")).unwrap();
        // A leftover part file.
        std::fs::write(dir.join("container-7.25.1-arm64.npk.999.0.part"), b"xx").unwrap();
        // A quick listing sees the size change and the missing file...
        let quick = a.cached_inventory().await.unwrap();
        assert_eq!(
            item(&quick, "7.25.1/routeros-7.25.1-arm64.npk").integrity,
            Integrity::Corrupt
        );
        assert_eq!(item(&quick, "7.25.1/kernels").integrity, Integrity::Missing);
        // ... verifying reads every file (the same-size change too).
        let inv = a.verify_cache().await.unwrap();
        assert_eq!(
            item(&inv, "7.25.1/routeros-7.25.1-arm64.npk").integrity,
            Integrity::Corrupt
        );
        assert_eq!(
            item(&inv, "7.25.1/wireless-7.25.1-arm64.npk").integrity,
            Integrity::Corrupt
        );
        assert_eq!(item(&inv, "7.25.1/kernels").integrity, Integrity::Missing);
        let part = item(&inv, "partial/7.25.1/container-7.25.1-arm64.npk.999.0.part");
        assert_eq!((part.integrity, part.size), (Integrity::Partial, 2));
        // Delete the damaged ones only.
        a.delete_cached(DeleteScope::Damaged).await.unwrap();
        let inv = a.cached_inventory().await.unwrap();
        assert!(inv.items().all(|i| !i.integrity.damaged()), "{inv:#?}");
        assert!(inv.partial.is_empty());
        assert!(!dir.join("routeros-7.25.1-arm64.npk").exists());
        assert!(!dir.join("kernel-arm64.elf").exists());
        assert!(!dir.join("kernel-mmips.elf").exists(), "the whole set");
    }

    #[tokio::test]
    async fn listings_reuse_verification_results_of_unchanged_files() {
        let (_srv, root, a) = filled("inv-memo").await;
        let before = hashes(&root);
        let verified = a.verify_cache().await.unwrap();
        assert!(hashes(&root) > before, "verify reads the files");
        assert!(verified
            .items()
            .all(|i| i.integrity != Integrity::Unchecked));
        let after_verify = hashes(&root);
        let quick = a.cached_inventory().await.unwrap();
        assert_eq!(
            hashes(&root),
            after_verify,
            "unchanged files are not hashed again"
        );
        assert_eq!(quick, verified);
        // Deleting the damaged ones does not hash everything again either.
        a.delete_cached(DeleteScope::Damaged).await.unwrap();
        assert_eq!(hashes(&root), after_verify);
        // A fresh manager has nothing remembered: unchecked, not hashed.
        let fresh = assets(&_srv, &root);
        let listed = fresh.cached_inventory().await.unwrap();
        assert_eq!(hashes(&root), after_verify);
        assert_eq!(
            item(&listed, "7.25.1/wireless-7.25.1-arm64.npk").integrity,
            Integrity::Unchecked
        );
        assert!(!Integrity::Unchecked.damaged());
    }

    #[tokio::test]
    async fn a_version_being_downloaded_into_cannot_be_deleted() {
        let srv = TestServer::start().await;
        let (_, _, wireless) = mirror(&srv);
        let root = tempdir("inv-race");
        let a = std::sync::Arc::new(assets(&srv, &root));
        a.ensure_kernels(&v()).await.unwrap();
        srv.route(
            "/dl/7.25.1/wireless-7.25.1-arm64.npk",
            crate::testutil::Reply::Slow {
                body: wireless,
                chunk: 256,
                delay: std::time::Duration::from_millis(40),
            },
        );
        let downloading = {
            let a = a.clone();
            tokio::spawn(async move {
                a.ensure_packages(&v(), PkgArch::Arm64, &["wireless".to_string()])
                    .await
            })
        };
        // Wait for its part file: the download writes into the version.
        let dir = root.join("7.25.1");
        let started = async {
            loop {
                let part = std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with("wireless-"));
                if part {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), started)
            .await
            .expect("download started");
        let err = a
            .delete_cached(DeleteScope::Version {
                version: "7.25.1".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), "busy");
        downloading.await.unwrap().unwrap();
        a.delete_cached(DeleteScope::Version {
            version: "7.25.1".into(),
        })
        .await
        .unwrap();
        assert!(!dir.exists());
        assert!(root.join(".locks").is_dir(), "the lock directory is kept");
    }

    #[tokio::test]
    async fn items_versions_and_everything_can_be_deleted() {
        let (_srv, root, a) = filled("inv-delete").await;
        let id = "7.25.1/wireless-7.25.1-arm64.npk";
        let size = item(&a.cached_inventory().await.unwrap(), id).size;
        assert_eq!(
            a.delete_cached(DeleteScope::Item { id: id.into() })
                .await
                .unwrap(),
            size
        );
        let inv = a.cached_inventory().await.unwrap();
        assert!(inv.items().all(|i| i.id != id));
        assert!(!root
            .join("7.25.1")
            .join("wireless-7.25.1-arm64.npk")
            .exists());
        assert!(!CacheDir::new(root.join("7.25.1"))
            .load()
            .files
            .contains_key("wireless-7.25.1-arm64.npk"));
        // The kernel set.
        a.delete_cached(DeleteScope::Item {
            id: "7.25.1/kernels".into(),
        })
        .await
        .unwrap();
        assert!(a.cached_versions().await.unwrap().is_empty());
        // The version.
        a.delete_cached(DeleteScope::Version {
            version: "7.25.1".into(),
        })
        .await
        .unwrap();
        assert!(!root.join("7.25.1").exists());
        // Everything (again, after a new download) keeps local files.
        a.ensure_kernels(&v()).await.unwrap();
        let src = tempdir("inv-delete-src");
        std::fs::write(src.join("setup.rsc"), b"/system identity set name=x\n").unwrap();
        a.register_local_file(&src.join("setup.rsc")).await.unwrap();
        a.delete_cached(DeleteScope::All).await.unwrap();
        let inv = a.cached_inventory().await.unwrap();
        assert!(inv.versions.is_empty());
        assert_eq!(inv.local.len(), 1);
        a.delete_cached(DeleteScope::Item {
            id: "local/setup.rsc".into(),
        })
        .await
        .unwrap();
        assert!(a.cached_inventory().await.unwrap().local.is_empty());
    }

    #[tokio::test]
    async fn empty_version_directories_are_neither_listed_nor_kept() {
        let (_srv, root, a) = filled("inv-empty").await;
        // A directory left empty by an earlier version of the app.
        let stale = root.join("7.24.6");
        std::fs::create_dir_all(&stale).unwrap();
        CacheDir::new(stale.clone()).update(|_| {}).unwrap();
        let inv = a.cached_inventory().await.unwrap();
        let listed: Vec<&str> = inv.versions.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(listed, ["7.25.1"]);
        assert!(!stale.exists(), "removed by the listing");
        // Deleting the last items of a version removes its directory.
        for id in [
            "7.25.1/kernels",
            "7.25.1/routeros-7.25.1-arm64.npk",
            "7.25.1/wireless-7.25.1-arm64.npk",
        ] {
            assert!(root.join("7.25.1").exists(), "{id}");
            a.delete_cached(DeleteScope::Item { id: id.into() })
                .await
                .unwrap();
        }
        assert!(!root.join("7.25.1").exists());
        assert!(a.cached_inventory().await.unwrap().versions.is_empty());
        // One being downloaded into is left alone.
        std::fs::create_dir_all(&stale).unwrap();
        let dir = CacheDir::new(stale.clone());
        let held = lock::try_lock_file(&dir.lock_path(DIR_LOCK))
            .unwrap()
            .unwrap();
        assert!(a.cached_inventory().await.unwrap().versions.is_empty());
        assert!(stale.exists(), "busy: kept, but not listed");
        drop(held);
        a.delete_cached(DeleteScope::All).await.unwrap();
        assert!(!stale.exists(), "everything includes empty directories");
    }

    #[tokio::test]
    async fn a_target_being_written_is_busy() {
        let (_srv, root, a) = filled("inv-busy").await;
        let dir = CacheDir::new(root.join("7.25.1"));
        let _held = lock::try_lock_file(&dir.lock_path("wireless-7.25.1-arm64.npk"))
            .unwrap()
            .unwrap();
        let id = "7.25.1/wireless-7.25.1-arm64.npk";
        let err = a
            .delete_cached(DeleteScope::Item { id: id.into() })
            .await
            .unwrap_err();
        assert_eq!(err.code(), "busy");
        assert!(root.join(id).exists(), "nothing deleted");
        let err = a
            .delete_cached(DeleteScope::Version {
                version: "7.25.1".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), "busy");
        let tar = lock::try_lock_file(&dir.lock_path(&urls::tarball_name(&v())))
            .unwrap()
            .unwrap();
        let err = a
            .delete_cached(DeleteScope::Item {
                id: "7.25.1/kernels".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), "busy");
        drop(tar);
    }

    #[tokio::test]
    async fn ids_cannot_escape_the_cache_and_symlinks_are_only_unlinked() {
        let (_srv, root, a) = filled("inv-escape").await;
        for id in [
            "../x",
            "7.25.1/../../etc",
            "7.25.1/index.json",
            "local/../7.25.1",
            "partial/.locks/x.1.2.part",
            "partial/7.25.1/not-a-part",
            "nonsense",
            "7.25.1/kernel-arm64.elf",
            "7.25.1/missing.npk",
        ] {
            let err = a
                .delete_cached(DeleteScope::Item { id: id.into() })
                .await
                .unwrap_err();
            assert_eq!(err.code(), "invalid_item", "{id}");
        }
        // A version that is a symlink to elsewhere: unlinked, target kept.
        let outside = tempdir("inv-escape-outside");
        std::fs::write(outside.join("keep"), b"x").unwrap();
        std::os::unix::fs::symlink(&*outside, root.join("7.24.6")).unwrap();
        a.delete_cached(DeleteScope::Version {
            version: "7.24.6".into(),
        })
        .await
        .unwrap();
        assert!(!root.join("7.24.6").exists());
        assert!(outside.join("keep").exists());
    }

    #[test]
    fn package_names_and_versions_of_ids() {
        assert_eq!(
            package_parts("wireless-7.25.1-arm64.npk", "7.25.1"),
            Some(("wireless", "arm64"))
        );
        assert_eq!(package_parts("other.bin", "7.25.1"), None);
        assert_eq!(item_version("7.25.1/kernels"), Some(v()));
        assert_eq!(item_version("partial/7.25.1/x.1.2.part"), Some(v()));
        assert_eq!(item_version("local/setup.rsc"), None);
    }
}
