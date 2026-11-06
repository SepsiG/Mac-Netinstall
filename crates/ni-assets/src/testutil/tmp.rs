//! Temporary directories that clean up after themselves.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// Set to `1` to keep the directories of failing tests for inspection.
pub const KEEP_ENV: &str = "NI_ASSETS_KEEP_TEST_DIRS";

/// A fresh, empty directory under the system temp dir, removed on drop
/// (kept only if the test panics and [`KEEP_ENV`] is `1`).
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let keep = std::thread::panicking() && std::env::var(KEEP_ENV).is_ok_and(|v| v == "1");
        if keep {
            eprintln!("keeping {}", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// See [`TempDir`].
pub fn tempdir(tag: &str) -> TempDir {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ni-assets-test-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dirs_are_removed_on_drop() {
        let dir = tempdir("guard");
        let path = dir.to_path_buf();
        std::fs::create_dir_all(path.join("a/b")).unwrap();
        std::fs::write(path.join("a/b/c"), b"x").unwrap();
        drop(dir);
        assert!(!path.exists());
    }
}
