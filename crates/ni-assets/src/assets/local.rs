//! User-supplied files and cache maintenance.

use super::{blocking, Assets};
use crate::cache::{CacheDir, Entry, Lookup};
use crate::error::AssetError;
use crate::lock;
use crate::npk::check_npk;
use crate::version::Version;
use bytes::Bytes;
use std::path::Path;

/// Largest local `.rsc` script accepted.
pub const MAX_SCRIPT_SIZE: u64 = 1 << 20;
/// Cache subdirectory for user-supplied files.
const LOCAL_DIR: &str = "local";

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

impl Assets {
    /// Copy a user-supplied `.npk` (checked like a download) or `.rsc`
    /// (at most [`MAX_SCRIPT_SIZE`]) into the cache under its file name,
    /// replacing an earlier file of the same name.
    pub async fn register_local_file(&self, path: &Path) -> Result<LocalFile, AssetError> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let kind = match name.rsplit_once('.') {
            Some((stem, "npk")) if !stem.is_empty() => LocalKind::Package,
            Some((stem, "rsc")) if !stem.is_empty() => LocalKind::Script,
            _ => return Err(AssetError::UnsupportedLocalFile { name }),
        };
        if name.starts_with('.') || name.contains('\n') || name.len() > 255 {
            return Err(AssetError::UnsupportedLocalFile { name });
        }
        let max = match kind {
            LocalKind::Package => self.max_download,
            LocalKind::Script => MAX_SCRIPT_SIZE,
        };
        let dir = CacheDir::new(self.root.join(LOCAL_DIR));
        let lock = lock::lock_target(dir.lock_path(&name)).await?;
        let path = path.to_path_buf();
        blocking(move || {
            let _lock = lock;
            let len = std::fs::metadata(&path)
                .map_err(AssetError::io("read", &path))?
                .len();
            if len > max {
                return Err(AssetError::LocalFileTooLarge { name, max });
            }
            let data = std::fs::read(&path).map_err(AssetError::io("read", &path))?;
            if kind == LocalKind::Package {
                check_npk(&name, &data)?;
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

    /// The contents of a registered local file, verified.
    pub async fn local_file(&self, name: &str) -> Result<Bytes, AssetError> {
        let dir = CacheDir::new(self.root.join(LOCAL_DIR));
        let key = name.to_string();
        match blocking(move || dir.lookup(&dir.load(), &key)).await? {
            Lookup::Valid(data) => Ok(data),
            Lookup::Missing => Err(AssetError::LocalMissing { name: name.into() }),
            Lookup::Corrupt => Err(AssetError::LocalCorrupt { name: name.into() }),
        }
    }

    /// Delete all downloaded versions (directories named like a version);
    /// registered local files are kept.
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
                if is_version && path.is_dir() {
                    std::fs::remove_dir_all(&path).map_err(AssetError::io("remove", &path))?;
                }
            }
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
}
