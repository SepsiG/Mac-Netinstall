//! User-supplied files and cache maintenance.

use super::{blocking, remove_stale_parts, Assets};
use crate::cache::{CacheDir, Entry, Lookup};
use crate::error::AssetError;
use crate::lock;
use crate::npk::check_npk;
use crate::version::Version;
use bytes::Bytes;
use std::path::Path;
use std::time::Duration;

/// Longest local file name (bytes) accepted: lock and part file names add
/// up to ~25 bytes, and APFS allows 255.
pub const MAX_LOCAL_NAME: usize = 200;
/// Largest local `.rsc` script accepted.
pub const MAX_SCRIPT_SIZE: u64 = 1 << 20;
/// Cache subdirectory for user-supplied files.
pub(crate) const LOCAL_DIR: &str = "local";

/// Kind of a user-supplied file, from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// `.npk`: a RouterOS package.
    Package,
    /// `.rsc`: a RouterOS configuration script.
    Script,
}

/// A registered user-supplied file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub name: String,
    pub kind: LocalKind,
    pub size: u64,
    pub sha256: String,
}

/// Kind and cache key of a local file name. Keys are lowercase, because
/// the cache usually lives on a case-insensitive APFS volume where
/// `Setup.rsc` and `setup.rsc` are the same file: names differing only by
/// case refer to the same local file. A name must be a plain file name
/// (no `/`, not hidden, no control characters, at most [`MAX_LOCAL_NAME`]
/// bytes) ending
/// in `.npk` or `.rsc`.
fn local_key(name: &str) -> Result<(String, LocalKind), AssetError> {
    let key = name.to_lowercase();
    let unsupported = || AssetError::UnsupportedLocalFile { name: name.into() };
    if key.starts_with('.')
        || key.contains('/')
        || key.chars().any(char::is_control)
        || key.len() > MAX_LOCAL_NAME
    {
        return Err(unsupported());
    }
    let kind = match key.rsplit_once('.') {
        Some((stem, "npk")) if !stem.is_empty() => LocalKind::Package,
        Some((stem, "rsc")) if !stem.is_empty() => LocalKind::Script,
        _ => return Err(unsupported()),
    };
    Ok((key, kind))
}

/// Why [`read_regular_file`] refused a file.
#[derive(Debug)]
pub enum ReadFileError {
    /// It could not be opened or read.
    Io(std::io::Error),
    /// It is a FIFO, device, directory, ...
    NotAFile,
    /// It has more than the allowed bytes.
    TooLarge,
}

/// Read a regular file (symlinks followed) of at most `max` bytes, opening
/// it once. FIFOs, devices and directories are refused without blocking:
/// the file is opened with `O_NONBLOCK` and the open file (not the path) is
/// checked before anything is read; the read itself stops after `max + 1`
/// bytes, so a file growing meanwhile is still caught.
pub fn read_regular_file(path: &Path, max: u64) -> Result<Vec<u8>, ReadFileError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)
        .map_err(ReadFileError::Io)?;
    let meta = file.metadata().map_err(ReadFileError::Io)?;
    if !meta.is_file() {
        return Err(ReadFileError::NotAFile);
    }
    if meta.len() > max {
        return Err(ReadFileError::TooLarge);
    }
    let mut data = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut data)
        .map_err(ReadFileError::Io)?;
    if data.len() as u64 > max {
        return Err(ReadFileError::TooLarge);
    }
    Ok(data)
}

/// [`read_regular_file`] with the local-file errors.
fn read_local(path: &Path, name: &str, max: u64) -> Result<Vec<u8>, AssetError> {
    read_regular_file(path, max).map_err(|e| match e {
        ReadFileError::Io(e) => AssetError::io("read", path)(e),
        ReadFileError::NotAFile => AssetError::LocalNotAFile { name: name.into() },
        ReadFileError::TooLarge => AssetError::LocalFileTooLarge {
            name: name.into(),
            max,
        },
    })
}

impl Assets {
    /// Copy a user-supplied `.npk` (checked like a download) or `.rsc`
    /// (at most [`MAX_SCRIPT_SIZE`]) into the cache, replacing an earlier
    /// file of the same name. The path must lead (after symlinks) to a
    /// regular file. The registered [`LocalFile::name`] is the file name in
    /// lowercase (see [`Assets::local_file`]).
    pub async fn register_local_file(&self, path: &Path) -> Result<LocalFile, AssetError> {
        let (file_name, name, kind, max) = self.local_target(path)?;
        let dir = CacheDir::new(self.root.join(LOCAL_DIR));
        let lock = lock::lock_target(dir.lock_path(&name)).await?;
        let path = path.to_path_buf();
        blocking(move || {
            let _lock = lock;
            let data = read_local(&path, &file_name, max)?;
            if kind == LocalKind::Package {
                check_npk(&file_name, &data)?;
            }
            let entry = Entry::new(&data, format!("file://{}", path.display()), false);
            dir.write_atomic(&name, &data)?;
            dir.update(|index| {
                index.files.insert(name.clone(), entry.clone());
            })?;
            Ok(LocalFile {
                name,
                kind,
                size: entry.size,
                sha256: entry.sha256,
            })
        })
        .await?
    }

    /// Check what [`Assets::register_local_file`] would register (name,
    /// kind, a regular file within the size limit, an `.npk`'s magic)
    /// without touching the cache.
    pub async fn check_local_file(&self, path: &Path) -> Result<LocalFile, AssetError> {
        let (file_name, name, kind, max) = self.local_target(path)?;
        let path = path.to_path_buf();
        blocking(move || {
            let data = read_local(&path, &file_name, max)?;
            if kind == LocalKind::Package {
                check_npk(&file_name, &data)?;
            }
            let entry = Entry::new(&data, format!("file://{}", path.display()), false);
            Ok(LocalFile {
                name,
                kind,
                size: entry.size,
                sha256: entry.sha256,
            })
        })
        .await?
    }

    /// File name, cache name, kind and size limit of a local file.
    fn local_target(&self, path: &Path) -> Result<(String, String, LocalKind, u64), AssetError> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let (name, kind) = local_key(&file_name)?;
        let max = match kind {
            LocalKind::Package => self.max_download,
            LocalKind::Script => MAX_SCRIPT_SIZE,
        };
        Ok((file_name, name, kind, max))
    }

    /// The contents of a registered local file, verified. `name` is
    /// matched without regard to case.
    pub async fn local_file(&self, name: &str) -> Result<Bytes, AssetError> {
        let (key, _) = local_key(name)?;
        let dir = CacheDir::new(self.root.join(LOCAL_DIR));
        match blocking(move || dir.lookup(&dir.load(), &key)).await? {
            Lookup::Valid(data) => Ok(data),
            Lookup::Missing => Err(AssetError::LocalMissing { name: name.into() }),
            Lookup::Corrupt => Err(AssetError::LocalCorrupt { name: name.into() }),
        }
    }

    /// Delete all downloaded versions (directories named like a version; a
    /// symlink named like a version is unlinked, its target left alone)
    /// and part files whose writer is gone; registered local files and the
    /// channel and version lists are kept.
    ///
    /// The cache may be shared with other processes (`ni-cli`, a second
    /// app): a version is deleted only while every lock its writers use is
    /// free (taken without waiting, as [`Assets::delete_cached`] does), so a
    /// version being downloaded or read for an install elsewhere is never
    /// removed under it. Returns the versions skipped because they were
    /// busy, sorted.
    pub async fn clear_downloads(&self) -> Result<Vec<String>, AssetError> {
        let root = self.root.clone();
        blocking(move || {
            let entries = match std::fs::read_dir(&root) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(AssetError::io("read", &root)(e)),
            };
            let mut skipped = Vec::new();
            for entry in entries.flatten() {
                let Some(version) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| Version::parse(n).ok())
                else {
                    continue;
                };
                match super::inventory::delete_version(&root, &version, false) {
                    Ok(_) => {}
                    Err(AssetError::Busy { .. }) => skipped.push(version.to_string()),
                    Err(e) => return Err(e),
                }
            }
            remove_stale_parts(&root, Duration::ZERO);
            skipped.sort();
            Ok(skipped)
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::PkgArch;
    use crate::npk::sha256_hex;
    use crate::testutil::*;

    #[tokio::test]
    async fn checking_a_local_file_does_not_register_it() {
        let dir = tempdir("local-check");
        let a = Assets::new(dir.join("cache"), crate::HttpConfig::default()).unwrap();
        let script = dir.join("Lab.rsc");
        std::fs::write(&script, b"/x\n").unwrap();
        let checked = a.check_local_file(&script).await.unwrap();
        assert_eq!((checked.name.as_str(), checked.size), ("lab.rsc", 3));
        assert!(a.local_file("lab.rsc").await.is_err(), "not registered");
        assert_eq!(a.register_local_file(&script).await.unwrap(), checked);
        let missing = a.check_local_file(&dir.join("none.rsc")).await;
        assert!(missing.is_err());
    }

    #[tokio::test]
    async fn local_files_are_registered_and_verified() {
        let srv = TestServer::start().await;
        let root = tempdir("local");
        let src = tempdir("local-src");
        let a = assets(&srv, &root);

        let pkg = src.join("custom-7.25.1-arm64.npk");
        std::fs::write(&pkg, npk(64, 3)).unwrap();
        let script = src.join("setup.rsc");
        std::fs::write(&script, b"/system identity set name=lab\n").unwrap();

        let reg = a.register_local_file(&pkg).await.unwrap();
        assert_eq!(
            (reg.name.as_str(), reg.kind, reg.size),
            ("custom-7.25.1-arm64.npk", LocalKind::Package, 64)
        );
        let reg = a.register_local_file(&script).await.unwrap();
        assert_eq!(reg.kind, LocalKind::Script);
        assert_eq!(reg.sha256, sha256_hex(b"/system identity set name=lab\n"));
        assert_eq!(
            a.local_file("setup.rsc").await.unwrap().as_ref(),
            b"/system identity set name=lab\n"
        );

        std::fs::write(root.join("local/setup.rsc"), b"tampered").unwrap();
        assert_eq!(
            a.local_file("setup.rsc").await.unwrap_err().code(),
            "local_corrupt"
        );
        assert_eq!(
            a.local_file("nope.rsc").await.unwrap_err().code(),
            "local_missing"
        );
        // Registering again repairs it.
        a.register_local_file(&script).await.unwrap();
        assert!(a.local_file("setup.rsc").await.is_ok());
        assert!(srv.paths().is_empty(), "no network for local files");
    }

    #[tokio::test]
    async fn bad_local_files_are_rejected() {
        let srv = TestServer::start().await;
        let src = tempdir("local-bad-src");
        let root = tempdir("local-bad");
        let a = assets(&srv, &root);
        let cases: [(&str, Vec<u8>, &str); 5] = [
            ("notes.txt", b"x".to_vec(), "unsupported_local_file"),
            (".npk", npk(8, 0), "unsupported_local_file"),
            ("broken.npk", b"PK\x03\x04".to_vec(), "not_npk"),
            (
                "big.rsc",
                vec![b'#'; MAX_SCRIPT_SIZE as usize + 1],
                "local_too_large",
            ),
            ("short.npk", npk(64, 1)[..60].to_vec(), "npk_length"),
        ];
        for (name, data, code) in cases {
            let path = src.join(name);
            std::fs::write(&path, data).unwrap();
            let err = a.register_local_file(&path).await.unwrap_err();
            assert_eq!(err.code(), code, "{name}");
        }
        let err = a
            .register_local_file(&src.join("missing.rsc"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "io");
    }

    #[tokio::test]
    async fn clear_downloads_keeps_local_files_and_other_directories() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let root = tempdir("clear");
        let a = assets(&srv, &root);
        a.ensure_packages(&v(), PkgArch::Arm64, &[]).await.unwrap();
        let script = root.join("x.rsc");
        std::fs::write(&script, b"#").unwrap();
        a.register_local_file(&script).await.unwrap();
        std::fs::create_dir_all(root.join("not-a-version")).unwrap();
        // A v6 download, a local package and the small UI caches.
        mirror_v6(&srv, V6, "arm", "arm", true);
        a.ensure_packages(&Version::parse(V6).unwrap(), PkgArch::Arm, &[])
            .await
            .unwrap();
        let npk = root.join("Mine.npk");
        std::fs::write(&npk, crate::testutil::npk_package("system", "arm", 200, 1)).unwrap();
        a.register_local_file(&npk).await.unwrap();
        a.resolve(&crate::VersionSpec::Channel(crate::Channel::Stable))
            .await
            .unwrap();
        std::fs::write(root.join(crate::VERSIONS_FILE), b"{}").unwrap();
        a.clear_downloads().await.unwrap();
        assert!(!root.join(V).exists());
        assert!(!root.join(V6).exists());
        assert!(root.join("local/x.rsc").exists());
        assert!(root.join("local/mine.npk").exists());
        assert!(a.local_file("mine.npk").await.is_ok());
        assert!(root.join(super::super::CHANNELS_FILE).exists());
        assert!(root.join(crate::VERSIONS_FILE).exists());
        assert!(root.join("not-a-version").exists());
        // Nothing to clear is fine too.
        assets(&srv, &root.join("absent"))
            .clear_downloads()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn only_regular_files_are_accepted() {
        let srv = TestServer::start().await;
        let root = tempdir("local-kinds");
        let src = tempdir("local-kinds-src");
        let a = assets(&srv, &root);
        let fifo = src.join("pipe.rsc");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
        let zero = src.join("zero.npk");
        std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
        let dir = src.join("folder.rsc");
        std::fs::create_dir(&dir).unwrap();
        for path in [&fifo, &zero, &dir] {
            let got = tokio::time::timeout(Duration::from_secs(5), a.register_local_file(path))
                .await
                .expect("must not block");
            assert_eq!(
                got.unwrap_err().code(),
                "local_not_file",
                "{}",
                path.display()
            );
        }
        // A symlink to a regular file is fine.
        let real = src.join("real.rsc");
        std::fs::write(&real, b"# ok").unwrap();
        let link = src.join("link.rsc");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(a.register_local_file(&link).await.unwrap().size, 4);
    }

    #[tokio::test]
    async fn local_names_are_validated_on_lookup() {
        let srv = TestServer::start().await;
        mirror(&srv);
        let root = tempdir("local-lookup");
        let a = assets(&srv, &root);
        a.ensure_packages(&v(), PkgArch::Arm64, &[]).await.unwrap();
        for bad in [
            format!("../{V}/routeros-{V}-arm64.npk"),
            "index.json".into(),
            ".hidden.rsc".into(),
            "a/b.rsc".into(),
        ] {
            let err = a.local_file(&bad).await.unwrap_err();
            assert_eq!(err.code(), "unsupported_local_file", "{bad}");
        }
    }

    #[tokio::test]
    async fn local_names_ignore_case() {
        let srv = TestServer::start().await;
        let root = tempdir("local-case");
        let src = tempdir("local-case-src");
        let a = assets(&srv, &root);
        let upper = src.join("Setup.RSC");
        std::fs::write(&upper, b"# one").unwrap();
        let reg = a.register_local_file(&upper).await.unwrap();
        assert_eq!(reg.name, "setup.rsc");
        let lower_dir = tempdir("local-case-src2");
        let lower = lower_dir.join("setup.rsc");
        std::fs::write(&lower, b"# two").unwrap();
        a.register_local_file(&lower).await.unwrap();
        assert_eq!(a.local_file("SETUP.rsc").await.unwrap().as_ref(), b"# two");
        assert_eq!(a.local_file("setup.rsc").await.unwrap().as_ref(), b"# two");
        let index = CacheDir::new(root.join("local")).load();
        assert_eq!(index.files.keys().collect::<Vec<_>>(), ["setup.rsc"]);
    }

    /// Another process (ni-cli, a second app) using a version: its
    /// directory is skipped and reported, never removed under it.
    #[tokio::test]
    async fn clear_downloads_skips_versions_another_process_holds() {
        let srv = TestServer::start().await;
        mirror(&srv);
        mirror_version(&srv, "7.24.6");
        mirror_v6(&srv, V6, "arm", "arm", true);
        let root = tempdir("clear-held");
        let a = assets(&srv, &root);
        for ver in [V, "7.24.6", V6] {
            let arch = if ver == V6 {
                PkgArch::Arm
            } else {
                PkgArch::Arm64
            };
            a.ensure_packages(&Version::parse(ver).unwrap(), arch, &[])
                .await
                .unwrap();
        }
        // A writer into 7.25.1 (the directory lock, held shared by
        // downloads) and one on a single file of 7.24.6.
        let dir = CacheDir::new(root.join(V));
        let dir_lock =
            crate::lock::lock_shared_blocking(&dir.lock_path(crate::cache::DIR_LOCK)).unwrap();
        let other = CacheDir::new(root.join("7.24.6"));
        let file_lock = crate::lock::try_lock_file(&other.lock_path("routeros-7.24.6-arm64.npk"))
            .unwrap()
            .unwrap();
        let skipped = a.clear_downloads().await.unwrap();
        assert_eq!(skipped, ["7.24.6", V]);
        assert!(root
            .join(V)
            .join(format!("routeros-{V}-arm64.npk"))
            .exists());
        assert!(root.join("7.24.6/routeros-7.24.6-arm64.npk").exists());
        assert!(!root.join(V6).exists(), "a free version is removed");
        drop((dir_lock, file_lock));
        assert_eq!(a.clear_downloads().await.unwrap(), Vec::<String>::new());
        assert!(!root.join(V).exists() && !root.join("7.24.6").exists());
    }

    #[tokio::test]
    async fn clear_downloads_unlinks_symlinked_versions_and_stray_parts() {
        let srv = TestServer::start().await;
        let root = tempdir("clear-links");
        let outside = tempdir("clear-links-target");
        std::fs::write(outside.join("keep.me"), b"precious").unwrap();
        std::fs::create_dir_all(&*root).unwrap();
        std::os::unix::fs::symlink(&*outside, root.join("7.24.6")).unwrap();
        let local = root.join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("x.rsc.77.0.part"), b"half").unwrap();
        std::fs::write(local.join("x.rsc"), b"#").unwrap();
        assets(&srv, &root).clear_downloads().await.unwrap();
        assert!(
            std::fs::symlink_metadata(root.join("7.24.6")).is_err(),
            "link removed"
        );
        assert_eq!(std::fs::read(outside.join("keep.me")).unwrap(), b"precious");
        assert!(!local.join("x.rsc.77.0.part").exists());
        assert!(local.join("x.rsc").exists());
    }

    #[tokio::test]
    async fn long_local_names_are_capped_with_a_clear_error() {
        let srv = TestServer::start().await;
        let root = tempdir("local-long");
        let src = tempdir("local-long-src");
        let a = assets(&srv, &root);
        let at_limit = format!("{}.rsc", "a".repeat(MAX_LOCAL_NAME - 4));
        assert_eq!(at_limit.len(), MAX_LOCAL_NAME);
        std::fs::write(src.join(&at_limit), b"#").unwrap();
        let reg = a.register_local_file(&src.join(&at_limit)).await.unwrap();
        assert_eq!(a.local_file(&reg.name).await.unwrap().as_ref(), b"#");
        for len in [MAX_LOCAL_NAME + 1, 244, 249, 255] {
            let name = format!("{}.rsc", "b".repeat(len - 4));
            std::fs::write(src.join(&name), b"#").unwrap();
            let err = a.register_local_file(&src.join(&name)).await.unwrap_err();
            assert_eq!(err.code(), "unsupported_local_file", "{len}: {err:?}");
            let err = a.local_file(&name).await.unwrap_err();
            assert_eq!(err.code(), "unsupported_local_file", "{len}");
        }
    }
}
