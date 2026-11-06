//! `tracing` to a daily log file in `~/Library/Logs/<bundle id>/`.
//!
//! The terminal shows engine events (see `output`), not log lines; the
//! log file is for diagnosis and is what a bug report should attach.

use ni_assets::BUNDLE_ID;
use std::path::{Path, PathBuf};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Log files kept (one per day).
pub const KEEP_FILES: usize = 7;

/// `~/Library/Logs/io.github.sepsig.mac-netinstall`, or `None` without `HOME`.
pub fn default_log_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Library/Logs").join(BUNDLE_ID))
}

/// Log to `<dir>/ni-cli.<date>.log` with `filter` (an `EnvFilter`
/// directive such as `info` or `ni_engine=debug`). Keep the guard alive
/// until exit, or the last lines are lost. On failure (bad filter,
/// unwritable directory) nothing is logged and the reason is returned.
pub fn init(dir: &Path, filter: &str) -> Result<WorkerGuard, String> {
    let filter = EnvFilter::try_new(filter).map_err(|e| format!("invalid log filter: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("ni-cli")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false),
        )
        .try_init()
        .map_err(|e| e.to_string())?;
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dir_is_under_library_logs() {
        let dir = default_log_dir().unwrap();
        assert!(dir.ends_with("Library/Logs/io.github.sepsig.mac-netinstall"));
    }

    #[test]
    fn bad_filter_is_reported() {
        let dir = ni_assets::testutil::tempdir("log-filter");
        let err = init(&dir, "ni_engine=[").unwrap_err();
        assert!(err.starts_with("invalid log filter"), "{err}");
    }
}
