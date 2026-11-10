//! The update notice's state: what the last check of GitHub found
//! (`update-check.json` next to the settings), and the views the window
//! gets. The checking and downloading itself is `ni-update`; `AppCore`
//! ties it to the settings.
//!
//! The window never gets a URL: it sees versions, notes and sizes, and
//! asks Rust to download the DMG or open the release page, which is
//! checked against [`ni_update::RELEASES_PAGE`] first.

use ni_update::{is_newer, Release, SemVer, MAX_NOTES_CHARS, RELEASES_PAGE};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// The event carrying download progress ([`UpdateProgress`]).
pub const UPDATE_PROGRESS: &str = "update-progress";

/// The running app's version.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The page opened when no release page is known.
pub fn fallback_page() -> String {
    format!("{RELEASES_PAGE}latest")
}

/// Seconds since the Unix epoch.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What the last check found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    /// When GitHub was last asked (successfully, or rate limited).
    pub last_check_unix: Option<u64>,
    /// The latest release then (`None`: none, or a prerelease).
    pub latest: Option<Release>,
}

impl UpdateState {
    /// The stored state; a missing or unusable file is an empty state, and
    /// a release whose version is not `X.Y.Z` is dropped.
    pub fn load(path: &Path) -> UpdateState {
        let Ok(data) = std::fs::read(path) else {
            return UpdateState::default();
        };
        let mut state: UpdateState = serde_json::from_slice(&data).unwrap_or_default();
        state.latest = state
            .latest
            .filter(|r| SemVer::parse(&r.version).is_some() && !r.version.starts_with('v'));
        if let Some(r) = &mut state.latest {
            if r.notes.chars().count() > MAX_NOTES_CHARS + 1 {
                r.notes = r.notes.chars().take(MAX_NOTES_CHARS).collect();
            }
        }
        state
    }

    /// Store it (a temporary file renamed over the old one); failures are
    /// only logged: the next check simply happens sooner.
    pub fn save(&self, path: &Path) {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
            let json = serde_json::to_vec_pretty(self).expect("update state serializes");
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, path).inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })
        };
        if let Err(e) = write() {
            tracing::warn!("cannot store the update check: {e}");
        }
    }
}

/// A release as the window sees it (no URLs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseView {
    pub version: String,
    /// Markdown source; the window shows it as text.
    pub notes: String,
    pub published_at: Option<String>,
    /// The DMG's size in bytes (as GitHub states it).
    pub size: u64,
}

/// The update notice and the About section's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateView {
    /// The running version.
    pub current: String,
    /// The latest release found (any version).
    pub latest: Option<ReleaseView>,
    /// `latest` is newer than `current` and not skipped: announce it.
    pub available: bool,
    pub last_checked_unix: Option<u64>,
    /// The release the user skipped.
    pub skipped: Option<String>,
    /// Automatic checks are on.
    pub auto: bool,
}

/// The view of `state` for the running `current` version.
pub fn view(state: &UpdateState, current: &str, auto: bool, skipped: Option<&str>) -> UpdateView {
    let available = state.latest.as_ref().is_some_and(|r| {
        is_newer(&r.version, current) && skipped.is_none_or(|s| is_newer(&r.version, s))
    });
    UpdateView {
        current: current.to_string(),
        latest: state.latest.as_ref().map(|r| ReleaseView {
            version: r.version.clone(),
            notes: r.notes.clone(),
            published_at: r.published_at.clone(),
            size: r.dmg.size,
        }),
        available,
        last_checked_unix: state.last_check_unix,
        skipped: skipped.map(str::to_string),
        auto,
    }
}

/// A finished download, for the window: the file name (in Downloads) and
/// whether the DMG could be opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DownloadedView {
    pub file_name: String,
    pub opened: bool,
}

/// Download progress (`update-progress` event).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UpdateProgress {
    pub done: u64,
    pub total: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_update::Asset;

    fn release(v: &str) -> Release {
        let asset = |name: String| Asset {
            url: format!("https://github.com/SepsiG/Mac-Netinstall/releases/download/v{v}/{name}"),
            name,
            size: 10,
        };
        Release {
            version: v.into(),
            tag: format!("v{v}"),
            page_url: format!("{RELEASES_PAGE}tag/v{v}"),
            notes: "notes".into(),
            published_at: None,
            dmg: asset(Release::dmg_name(v)),
            checksum: asset(format!("{}.sha256", Release::dmg_name(v))),
        }
    }

    #[test]
    fn only_newer_unskipped_releases_are_available() {
        let state = UpdateState {
            last_check_unix: Some(5),
            latest: Some(release("0.2.0")),
        };
        assert!(view(&state, "0.1.2", true, None).available);
        assert!(!view(&state, "0.2.0", true, None).available);
        assert!(!view(&state, "0.3.0", true, None).available);
        assert!(!view(&state, "0.1.2", true, Some("0.2.0")).available);
        assert!(view(&state, "0.1.2", true, Some("0.1.9")).available);
        assert!(!view(&UpdateState::default(), "0.1.2", true, None).available);
    }

    #[test]
    fn a_broken_state_file_is_an_empty_state() {
        let dir = ni_assets::testutil::tempdir("update-state");
        let path = dir.join("update-check.json");
        assert_eq!(UpdateState::load(&path), UpdateState::default());
        std::fs::write(&path, b"{oops").unwrap();
        assert_eq!(UpdateState::load(&path), UpdateState::default());
        let mut bad = UpdateState {
            last_check_unix: Some(7),
            latest: Some(release("0.2.0")),
        };
        bad.latest.as_mut().unwrap().version = "../../x".into();
        bad.save(&path);
        let back = UpdateState::load(&path);
        assert_eq!((back.last_check_unix, back.latest), (Some(7), None));
        let good = UpdateState {
            last_check_unix: Some(8),
            latest: Some(release("0.2.0")),
        };
        good.save(&path);
        assert_eq!(UpdateState::load(&path), good);
    }
}
