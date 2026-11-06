//! `tracing` to a daily log file in `~/Library/Logs/<bundle id>/`.

use ni_assets::BUNDLE_ID;
use std::path::{Path, PathBuf};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// `~/Library/Logs/io.github.sepsig.mac-netinstall`, or `None` without `HOME`.
pub fn default_log_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Library/Logs").join(BUNDLE_ID))
}

/// Log to `dir/app.<date>.log` (7 files kept) with `filter` (`NI_LOG`
/// syntax, e.g. `info` or `ni_engine=debug`). Keep the guard alive.
pub fn init(dir: &Path, filter: &str) -> Result<WorkerGuard, String> {
    let filter = EnvFilter::try_new(filter).map_err(|e| e.to_string())?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("app")
        .filename_suffix("log")
        .max_log_files(7)
        .build(dir)
        .map_err(|e| e.to_string())?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
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
        let dir = ni_assets::testutil::tempdir("app-log");
        assert!(init(&dir, "=[").is_err());
    }
}
