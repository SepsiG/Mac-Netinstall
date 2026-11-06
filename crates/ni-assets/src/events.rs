//! Progress events.

use std::sync::Arc;

/// What the asset manager is doing, for progress display. File names are
/// cache file names (e.g. `routeros-7.25.1-arm64.npk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetEvent {
    /// A download began; `total` is the announced size, if any.
    DownloadStarted { file: String, total: Option<u64> },
    /// Bytes received so far (sent at most every [`PROGRESS_STEP`] bytes).
    DownloadProgress {
        file: String,
        done: u64,
        total: Option<u64>,
    },
    /// The download completed (not yet verified).
    DownloadFinished { file: String, size: u64 },
    /// A verified cached copy was used; nothing was downloaded.
    CacheHit { file: String },
}

/// Minimum number of bytes between two [`AssetEvent::DownloadProgress`] events.
pub const PROGRESS_STEP: u64 = 256 * 1024;

/// Receives events; must not block (forward to a channel if needed).
pub type EventSink = Arc<dyn Fn(AssetEvent) + Send + Sync>;
