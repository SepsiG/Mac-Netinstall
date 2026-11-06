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

/// Read a regular file (symlinks followed) of at most `max` bytes. FIFOs,
/// devices and directories are refused without blocking: the file is
/// opened with `O_NONBLOCK` and checked before anything is read.
fn read_local(path: &Path, name: &str, max: u64) -> Result<Vec<u8>, AssetError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)
        .map_err(AssetError::io("read", path))?;
    let meta = file.metadata().map_err(AssetError::io("read", path))?;
    if !meta.is_file() {
        return Err(AssetError::LocalNotAFile { name: name.into() });
    }
    if meta.len() > max {
        return Err(AssetError::LocalFileTooLarge {
            name: name.into(),
            max,
        });
    }
    let mut data = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut data)
        .map_err(AssetError::io("read", path))?;
    if data.len() as u64 > max {
        return Err(AssetError::LocalFileTooLarge {
            name: name.into(),
            max,
        });
    }
    Ok(data)
}

impl Assets {
    /// Copy a user-supplied `.npk` (checked like a download) or `.rsc`
    /// (at most [`MAX_SCRIPT_SIZE`]) into the cache, replacing an earlier
    /// file of the same name. The path must lead (after symlinks) to a
    /// regular file. The registered [`LocalFile::name`] is the file name in
    /// lowercase (see [`Assets::local_file`]).
    pub async fn register_local_file(&self, path: &Path) -> Result<LocalFile, AssetError> {
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
    /// and part files whose writer is gone; registered local files are
    /// kept. A download running at the same time may fail.
    pub async fn clear_downloads(&self) -> Result<(), AssetError> {
        let root = self.root.clone();
        blocking(move || {
            let entries = match std::fs::read_dir(&root) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(AssetError::io("read", &root)(e)),
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let is_version = entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| Version::parse(n).is_ok());
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if !is_version {
                    continue;
                }
                if kind.is_symlink() {
                    std::fs::remove_file(&path).map_err(AssetError::io("remove", &path))?;
                } else if kind.is_dir() {
                    std::fs::remove_dir_all(&path).map_err(AssetError::io("remove", &path))?;
                }
            }
            remove_stale_parts(&root, Duration::ZERO);
            Ok(())
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
        a.clear_downloads().await.unwrap();
        assert!(!root.join(V).exists());
        assert!(root.join("local/x.rsc").exists());
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
