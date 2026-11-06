//! Per-target locks shared by every [`crate::Assets`] in this process and
//! by other processes using the same cache root.
//!
//! A target (one cached file, a version's kernel set, an index) has a lock
//! file `<root>/.locks/<dir>-<name>.lock` (see [`crate::cache::CacheDir::lock_path`]).
//! Holding a target means holding both an in-process async mutex for that
//! path and an exclusive advisory `flock` on the lock file. The async mutex
//! lets tasks of this process wait without polling; the `flock` coordinates
//! with other processes (BSD `flock` locks belong to the open file, so two
//! opens in one process also exclude each other). Lock files are never
//! deleted: removing one while another process waits on it would let two
//! holders in.

use crate::error::AssetError;
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;
use tokio::sync::OwnedMutexGuard;

type AsyncLock = tokio::sync::Mutex<()>;

static IN_PROCESS: LazyLock<Mutex<HashMap<PathBuf, Weak<AsyncLock>>>> =
    LazyLock::new(Mutex::default);

/// An exclusive `flock` on a lock file; released on drop.
pub(crate) type FileLock = Flock<File>;

/// A held target: the in-process mutex and the file lock. Share it (`Arc`)
/// with blocking work it protects, so it is released only when the last
/// user is done, even if the async caller was cancelled.
pub(crate) struct TargetLock {
    _file: FileLock,
    _local: OwnedMutexGuard<()>,
}

fn in_process(path: &Path) -> Arc<AsyncLock> {
    let mut map = IN_PROCESS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(lock) = map.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    map.retain(|_, w| w.strong_count() > 0);
    let lock = Arc::new(AsyncLock::new(()));
    map.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

fn open_lock_file(path: &Path) -> Result<File, AssetError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(AssetError::io("create", parent))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(AssetError::io("lock", path))
}

fn lock_error(path: &Path, errno: Errno) -> AssetError {
    AssetError::io("lock", path)(std::io::Error::from(errno))
}

/// Take the file lock if it is free; `None` if someone else holds it.
pub(crate) fn try_lock_file(path: &Path) -> Result<Option<FileLock>, AssetError> {
    let file = open_lock_file(path)?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(Some(lock)),
        Err((_, Errno::EWOULDBLOCK)) => Ok(None),
        Err((_, e)) => Err(lock_error(path, e)),
    }
}

/// Take a *shared* lock on `path`, blocking the thread while someone holds
/// it exclusively (only briefly: deleting a version directory).
pub(crate) fn lock_shared_blocking(path: &Path) -> Result<FileLock, AssetError> {
    let file = open_lock_file(path)?;
    Flock::lock(file, FlockArg::LockShared).map_err(|(_, e)| lock_error(path, e))
}

/// Take a shared lock on `path`, polling while it is held exclusively.
pub(crate) async fn lock_shared(path: PathBuf) -> Result<FileLock, AssetError> {
    let mut wait = Duration::from_millis(10);
    loop {
        let file = open_lock_file(&path)?;
        match Flock::lock(file, FlockArg::LockSharedNonblock) {
            Ok(lock) => return Ok(lock),
            Err((_, Errno::EWOULDBLOCK)) => {}
            Err((_, e)) => return Err(lock_error(&path, e)),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_millis(200));
    }
}

/// Take the file lock, blocking the thread until it is free. Only for
/// short critical sections inside blocking work (index updates).
pub(crate) fn lock_file_blocking(path: &Path) -> Result<FileLock, AssetError> {
    let file = open_lock_file(path)?;
    Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, e)| lock_error(path, e))
}

/// Take the target at `path`: first the in-process mutex, then the file
/// lock (polled, so no thread is blocked while another process holds it).
/// Cancel-safe: dropping the future releases whatever was taken.
pub(crate) async fn lock_target(path: PathBuf) -> Result<Arc<TargetLock>, AssetError> {
    let local = in_process(&path).lock_owned().await;
    let mut wait = Duration::from_millis(10);
    loop {
        // Non-blocking syscalls (open + flock LOCK_NB): fine on this thread.
        if let Some(file) = try_lock_file(&path)? {
            return Ok(Arc::new(TargetLock {
                _file: file,
                _local: local,
            }));
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;

    #[tokio::test]
    async fn targets_exclude_each_other_until_released() {
        let dir = tempdir("lock");
        let path = dir.join(".locks/x.lock");
        let first = lock_target(path.clone()).await.unwrap();
        assert!(try_lock_file(&path).unwrap().is_none());
        let second = tokio::time::timeout(Duration::from_millis(100), lock_target(path.clone()));
        assert!(second.await.is_err(), "second holder must wait");
        drop(first);
        let again = tokio::time::timeout(Duration::from_secs(2), lock_target(path.clone()));
        assert!(again.await.is_ok());
    }

    #[tokio::test]
    async fn a_file_lock_held_elsewhere_is_waited_for() {
        let dir = tempdir("lock-file");
        let path = dir.join("y.lock");
        let held = lock_file_blocking(&path).unwrap();
        let wait = tokio::time::timeout(Duration::from_millis(100), lock_target(path.clone()));
        assert!(wait.await.is_err());
        drop(held);
        let got = tokio::time::timeout(Duration::from_secs(2), lock_target(path.clone()));
        assert!(got.await.is_ok());
    }
}
