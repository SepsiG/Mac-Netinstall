//! App settings: one JSON file in Application Support, written atomically.

use ni_engine::{check_cooldown, check_max_parallel, Failure, MIN_COOLDOWN};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Languages the UI is translated to.
pub const LANGUAGES: &[&str] = &["en", "hu"];
/// Shortest download timeout accepted (seconds).
pub const MIN_DOWNLOAD_TIMEOUT: u64 = 60;
/// Longest download timeout accepted (seconds; a day).
pub const MAX_DOWNLOAD_TIMEOUT: u64 = 24 * 60 * 60;
/// Version of the `settings.json` layout, written as `version`. Files
/// without it are older: their `max_parallel` of 4 was the default then
/// (now 1) and is migrated to 1 when loaded.
///
/// Version 2 dropped `server_ip`, `netmask` (the address now comes from the
/// adapter's manual IPv4 in macOS) and `show_all_interfaces` (only
/// interfaces that can netinstall are listed); older files lose them.
///
/// Version 3 stores the pool as automatic (both ends `null`) unless it was
/// chosen: an older pool equal to the automatic one (for the last
/// interface's manual IPv4) becomes automatic.
///
/// Version 4 adds `window` (the main window's position and size) and
/// `dialog_sizes` (each dialog's size, by id); older files have neither and
/// keep everything else.
///
/// Version 5 adds `log_panel_height` (the log's height as the user dragged
/// it; `null`: the default); older files keep everything else.
///
/// Version 6 adds `onboarding_done` (the permissions sheet was gone
/// through); older files get `false`, so it is shown once after the
/// update too (those users were never asked up front either).
///
/// Version 7 adds `keep_downloads` (keep downloaded RouterOS files after
/// quitting; older files get `false`, the new default: downloads are
/// deleted on quit) and `disclaimer_accepted_version` (the disclaimer text
/// version the user accepted; older files get `null`, so it is asked once).
pub const SETTINGS_VERSION: u32 = 7;

/// Smallest main window (also `minWidth` / `minHeight` in tauri.conf.json).
pub const MIN_WINDOW_WIDTH: u32 = 820;
pub const MIN_WINDOW_HEIGHT: u32 = 520;
/// Largest stored window or dialog side (points).
pub const MAX_SIDE: u32 = 16_384;
/// Largest stored window coordinate (points, either sign).
pub const MAX_COORDINATE: i32 = 100_000;
/// Smallest stored dialog side (points).
pub const MIN_DIALOG_SIDE: u32 = 120;
/// Most dialog sizes stored.
pub const MAX_DIALOGS: usize = 16;
/// Longest dialog id.
pub const MAX_DIALOG_ID: usize = 32;
/// Smallest stored log panel height (CSS px of its lines: about three;
/// `MIN_LOG_HEIGHT` in the frontend's `logPanel.ts`). The largest is
/// [`MAX_SIDE`].
pub const MIN_LOG_PANEL_HEIGHT: u32 = 64;

/// A storable log panel height.
pub fn valid_log_panel_height(height: u32) -> bool {
    (MIN_LOG_PANEL_HEIGHT..=MAX_SIDE).contains(&height)
}

/// The main window's position (top-left of its frame) and content size,
/// in points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WindowGeometry {
    pub fn is_valid(&self) -> bool {
        (MIN_WINDOW_WIDTH..=MAX_SIDE).contains(&self.width)
            && (MIN_WINDOW_HEIGHT..=MAX_SIDE).contains(&self.height)
            && (-MAX_COORDINATE..=MAX_COORDINATE).contains(&self.x)
            && (-MAX_COORDINATE..=MAX_COORDINATE).contains(&self.y)
    }
}

/// A dialog's size as the user left it, in CSS pixels (points).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogSize {
    pub width: u32,
    pub height: u32,
}

impl DialogSize {
    pub fn is_valid(&self) -> bool {
        (MIN_DIALOG_SIDE..=MAX_SIDE).contains(&self.width)
            && (MIN_DIALOG_SIDE..=MAX_SIDE).contains(&self.height)
    }
}

/// A dialog id: 1..=[`MAX_DIALOG_ID`] of `a-z`, `0-9`, `_`, starting with
/// a letter.
pub fn valid_dialog_id(id: &str) -> bool {
    id.len() <= MAX_DIALOG_ID
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The automatic pool for an interface (by BSD name), if it has a manual
/// IPv4 in macOS.
pub type AutoPool = std::sync::Arc<dyn Fn(&str) -> Option<(Ipv4Addr, Ipv4Addr)> + Send + Sync>;

fn system_auto_pool(iface: &str) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let (ip, mask) = ni_net::configured_ipv4(iface)?.manual()?;
    ni_engine::auto_pool(ip, mask).ok()
}
/// Keys of older versions that are dropped without complaint.
const RETIRED_FIELDS: &[&str] = &["server_ip", "netmask", "show_all_interfaces"];
/// The `max_parallel` default of unversioned files.
const OLD_DEFAULT_MAX_PARALLEL: usize = 4;

/// What the app remembers between runs. Missing fields take their
/// defaults, so older files keep working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Layout version ([`SETTINGS_VERSION`]); always written as the current
    /// one.
    pub version: u32,
    /// `"en"` or `"hu"`; `None` follows the macOS language.
    pub language: Option<String>,
    pub last_interface: Option<String>,
    /// BOOTP pool (both or neither); default: the subnet of the adapter's
    /// address (the server's own address is skipped).
    pub pool_start: Option<Ipv4Addr>,
    pub pool_end: Option<Ipv4Addr>,
    /// Devices flashed at the same time; 1 unless chosen in Settings.
    pub max_parallel: usize,
    pub cooldown_secs: u64,
    pub auto_repeat: bool,
    pub last_profile: Option<String>,
    /// Longest time one download may take.
    pub download_timeout_secs: u64,
    /// The main window as it was left; restored at start (its position
    /// only if that is on a connected display).
    pub window: Option<WindowGeometry>,
    /// Each dialog's size as the user left it, by dialog id.
    pub dialog_sizes: std::collections::BTreeMap<String, DialogSize>,
    /// The log panel's height (CSS px of its lines) as the user left it;
    /// `None`: the default.
    pub log_panel_height: Option<u32>,
    /// The permissions sheet (Local Network, firewall) was gone through
    /// at least once; until then it opens at launch.
    pub onboarding_done: bool,
    /// Keep downloaded RouterOS files (kernels, packages) after quitting.
    /// `false` (default): they are deleted on quit and leftovers at the
    /// next launch; registered local files and the small version lists
    /// are always kept.
    pub keep_downloads: bool,
    /// The version of the disclaimer the user accepted
    /// ([`ni_engine::DISCLAIMER_VERSION`]); starting needs the current one.
    pub disclaimer_accepted_version: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            language: None,
            last_interface: None,
            pool_start: None,
            pool_end: None,
            max_parallel: 1,
            cooldown_secs: 300,
            auto_repeat: false,
            last_profile: None,
            download_timeout_secs: 30 * 60,
            window: None,
            dialog_sizes: Default::default(),
            log_panel_height: None,
            onboarding_done: false,
            keep_downloads: false,
            disclaimer_accepted_version: None,
        }
    }
}

fn invalid(field: &str) -> Failure {
    Failure::new("invalid_setting").with("field", field)
}

impl Settings {
    /// Codes: `invalid_setting` (`field`, and `min` / `max` where
    /// bounded), `invalid_max_parallel`, `invalid_cooldown`.
    pub fn validate(&self) -> Result<(), Failure> {
        if let Some(lang) = &self.language {
            if !LANGUAGES.contains(&lang.as_str()) {
                return Err(invalid("language"));
            }
        }
        match (self.pool_start, self.pool_end) {
            (None, None) => {}
            (Some(a), Some(b)) if u32::from(a) <= u32::from(b) => {}
            _ => return Err(invalid("pool")),
        }
        check_max_parallel(self.max_parallel)?;
        check_cooldown(Duration::from_secs(self.cooldown_secs), MIN_COOLDOWN)?;
        if !(MIN_DOWNLOAD_TIMEOUT..=MAX_DOWNLOAD_TIMEOUT).contains(&self.download_timeout_secs) {
            return Err(invalid("download_timeout")
                .with("min", MIN_DOWNLOAD_TIMEOUT)
                .with("max", MAX_DOWNLOAD_TIMEOUT));
        }
        if self.window.is_some_and(|w| !w.is_valid()) {
            return Err(invalid("window"));
        }
        let sizes_ok = self.dialog_sizes.len() <= MAX_DIALOGS
            && self
                .dialog_sizes
                .iter()
                .all(|(id, size)| valid_dialog_id(id) && size.is_valid());
        if !sizes_ok {
            return Err(invalid("dialog_sizes"));
        }
        if self
            .disclaimer_accepted_version
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 16)
        {
            return Err(invalid("disclaimer_accepted_version"));
        }
        if self
            .log_panel_height
            .is_some_and(|h| !valid_log_panel_height(h))
        {
            return Err(invalid("log_panel_height")
                .with("min", MIN_LOG_PANEL_HEIGHT)
                .with("max", MAX_SIDE));
        }
        Ok(())
    }

    pub fn download_timeout(&self) -> Duration {
        Duration::from_secs(self.download_timeout_secs)
    }

    /// `edit` (from the settings dialog) with what the app remembers by
    /// itself kept from `self`: the last interface, profile and
    /// auto-repeat, the window, the dialog sizes, the log panel's height
    /// whether onboarding was done and the accepted disclaimer. A dialog
    /// opened before
    /// a start must not undo what the start remembered.
    pub fn merged_edit(&self, edit: Settings) -> Settings {
        Settings {
            last_interface: self.last_interface.clone(),
            last_profile: self.last_profile.clone(),
            auto_repeat: self.auto_repeat,
            window: self.window,
            dialog_sizes: self.dialog_sizes.clone(),
            log_panel_height: self.log_panel_height,
            onboarding_done: self.onboarding_done,
            disclaimer_accepted_version: self.disclaimer_accepted_version.clone(),
            ..edit
        }
    }
}

/// The fields of `settings.json`, in the order they are checked when
/// loading. `pool` stands for `pool_start` + `pool_end` (both or neither).
const FIELDS: &[&str] = &[
    "language",
    "last_interface",
    "pool",
    "max_parallel",
    "cooldown_secs",
    "auto_repeat",
    "last_profile",
    "download_timeout_secs",
    "window",
    "dialog_sizes",
    "log_panel_height",
    "onboarding_done",
    "keep_downloads",
    "disclaimer_accepted_version",
];

/// Set `field` of `s` from the file's JSON `obj`; `false` if it has the
/// wrong type (`s` is then unchanged). A missing field is left as is.
fn set_field(
    s: &mut Settings,
    field: &str,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    use serde_json::{from_value, Value};
    fn get<T: serde::de::DeserializeOwned>(
        obj: &serde_json::Map<String, Value>,
        key: &str,
        slot: &mut T,
    ) -> bool {
        match obj.get(key) {
            None => true,
            Some(v) => match from_value(v.clone()) {
                Ok(x) => {
                    *slot = x;
                    true
                }
                Err(_) => false,
            },
        }
    }
    match field {
        "language" => get(obj, field, &mut s.language),
        "last_interface" => get(obj, field, &mut s.last_interface),
        "pool" => {
            let (mut a, mut b) = (s.pool_start, s.pool_end);
            let ok = get(obj, "pool_start", &mut a) && get(obj, "pool_end", &mut b);
            if ok {
                (s.pool_start, s.pool_end) = (a, b);
            }
            ok
        }
        "max_parallel" => get(obj, field, &mut s.max_parallel),
        "cooldown_secs" => get(obj, field, &mut s.cooldown_secs),
        "auto_repeat" => get(obj, field, &mut s.auto_repeat),
        "last_profile" => get(obj, field, &mut s.last_profile),
        "download_timeout_secs" => get(obj, field, &mut s.download_timeout_secs),
        "window" => get(obj, field, &mut s.window),
        "dialog_sizes" => get(obj, field, &mut s.dialog_sizes),
        "log_panel_height" => get(obj, field, &mut s.log_panel_height),
        "onboarding_done" => get(obj, field, &mut s.onboarding_done),
        "keep_downloads" => get(obj, field, &mut s.keep_downloads),
        "disclaimer_accepted_version" => get(obj, field, &mut s.disclaimer_accepted_version),
        _ => unreachable!("unknown settings field {field}"),
    }
}

/// What [`SettingsStore::load_checked`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub settings: Settings,
    /// Fields that were invalid (and took their defaults) or unknown, or
    /// `unparsable`; empty for a good file (or none).
    pub problems: Vec<String>,
}

/// Distinguishes the temporary files of concurrent saves in one process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `settings.json` in a directory.
#[derive(Clone)]
pub struct SettingsStore {
    path: PathBuf,
    /// The version of a file written by a newer app (0: none): it is read
    /// but never overwritten (see [`SettingsStore::save`]).
    newer: std::sync::Arc<AtomicU64>,
    /// For migrating stored pools (see [`SETTINGS_VERSION`]).
    auto_pool: AutoPool,
}

impl std::fmt::Debug for SettingsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsStore")
            .field("path", &self.path)
            .finish()
    }
}

fn io_failure(op: &str, e: std::io::Error) -> Failure {
    Failure::new("settings_io").with("op", op).with("detail", e)
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SettingsStore {
            path: path.into(),
            newer: Default::default(),
            auto_pool: std::sync::Arc::new(system_auto_pool),
        }
    }

    /// Use `f` instead of macOS's configuration to find automatic pools
    /// (tests).
    pub fn with_auto_pool(
        mut self,
        f: impl Fn(&str) -> Option<(Ipv4Addr, Ipv4Addr)> + Send + Sync + 'static,
    ) -> Self {
        self.auto_pool = std::sync::Arc::new(f);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored settings ([`SettingsStore::load_checked`]).
    pub fn load(&self) -> Settings {
        self.load_checked().settings
    }

    /// The file was written by a newer app and is left alone: saving
    /// fails with `settings_newer` (`version`, `supported`).
    pub fn is_read_only(&self) -> bool {
        self.newer.load(Ordering::Relaxed) != 0
    }

    /// Where a file that could not be used completely is kept
    /// (`settings.json.bad`).
    pub fn backup_path(&self) -> PathBuf {
        self.path.with_extension("json.bad")
    }

    /// The stored settings, checked field by field: an invalid field (or
    /// one of the wrong type) takes its default, the others are kept; a
    /// file that is not a JSON object gives the defaults. A broken file
    /// must not lock the user out. Whenever something was dropped (an
    /// unparsable file, invalid or unknown fields, e.g. from a newer
    /// version), the file is first copied to `settings.json.bad`, since
    /// the next save replaces it.
    pub fn load_checked(&self) -> Loaded {
        let mut loaded = Loaded {
            settings: Settings::default(),
            problems: Vec::new(),
        };
        let Ok(data) = std::fs::read(&self.path) else {
            return loaded;
        };
        let obj = match serde_json::from_slice::<serde_json::Value>(&data) {
            Ok(serde_json::Value::Object(obj)) => obj,
            _ => {
                loaded.problems.push("unparsable".into());
                self.keep_backup(&data, &loaded.problems);
                return loaded;
            }
        };
        let s = &mut loaded.settings;
        for field in FIELDS {
            let mut trial = s.clone();
            if set_field(&mut trial, field, &obj) && trial.validate().is_ok() {
                *s = trial;
            } else {
                loaded.problems.push(field.to_string());
            }
        }
        let version = obj.get("version").and_then(|v| v.as_u64());
        if let Some(v) = version.filter(|v| *v > u64::from(SETTINGS_VERSION)) {
            // From a newer app: use what we understand, change nothing.
            tracing::warn!(
                version = v,
                "settings file from a newer app; it will not be changed"
            );
            self.newer.store(v, Ordering::Relaxed);
            loaded.settings.version = SETTINGS_VERSION;
            return loaded;
        }
        let known = |k: &str| {
            FIELDS.contains(&k)
                || ["pool_start", "pool_end", "version"].contains(&k)
                || RETIRED_FIELDS.contains(&k)
        };
        loaded
            .problems
            .extend(obj.keys().filter(|k| !known(k)).cloned());
        if !loaded.problems.is_empty() {
            self.keep_backup(&data, &loaded.problems);
        }
        if version.is_none_or(|v| v < u64::from(SETTINGS_VERSION)) {
            // Older than versioned settings: 4 was the default then, not a
            // choice.
            if version.is_none() && loaded.settings.max_parallel == OLD_DEFAULT_MAX_PARALLEL {
                loaded.settings.max_parallel = Settings::default().max_parallel;
            }
            // Before version 3 a pool could not be "automatic" by name: one
            // equal to the automatic pool was not a choice.
            let s = &mut loaded.settings;
            let auto = s
                .last_interface
                .as_deref()
                .and_then(|i| (self.auto_pool)(i));
            if let (Some(range), Some(start), Some(end)) = (auto, s.pool_start, s.pool_end) {
                if range == (start, end) {
                    (s.pool_start, s.pool_end) = (None, None);
                }
            }
            // Retired fields are simply not written again. Store the
            // migrated file at once.
            tracing::info!("migrating settings to version {SETTINGS_VERSION}");
            if let Err(f) = self.save(&loaded.settings) {
                tracing::warn!("cannot store migrated settings: {f}");
            }
        }
        loaded.settings.version = SETTINGS_VERSION;
        loaded
    }

    fn keep_backup(&self, data: &[u8], problems: &[String]) {
        let backup = self.backup_path();
        tracing::warn!(
            path = %self.path.display(),
            ?problems,
            backup = %backup.display(),
            "settings file not fully usable; keeping a copy"
        );
        if let Err(e) = std::fs::write(&backup, data) {
            tracing::warn!("cannot keep a copy of the settings file: {e}");
        }
    }

    /// Validate and write: a temporary file next to it, flushed to disk,
    /// then renamed over the old one (never a half-written file).
    pub fn save(&self, settings: &Settings) -> Result<(), Failure> {
        let newer = self.newer.load(Ordering::Relaxed);
        if newer != 0 {
            return Err(Failure::new("settings_newer")
                .with("version", newer)
                .with("supported", SETTINGS_VERSION));
        }
        settings.validate()?;
        let settings = &Settings {
            version: SETTINGS_VERSION,
            ..settings.clone()
        };
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| io_failure("mkdir", e))?;
        let mut json = serde_json::to_vec_pretty(settings).expect("settings serialize");
        json.push(b'\n');
        let tmp = self.path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io_failure("write", e)
        })?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io_failure("rename", e)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_assets::testutil::tempdir;
    use serde_json::json;

    #[test]
    fn old_files_with_the_old_default_flash_one_at_a_time() {
        let dir = tempdir("settings-migrate");
        let path = dir.join("settings.json");
        // Written before settings had a version: 4 was the default then.
        std::fs::write(
            &path,
            r#"{"max_parallel": 4, "language": "hu", "cooldown_secs": 120}"#,
        )
        .unwrap();
        let store = SettingsStore::new(&path);
        let loaded = store.load_checked();
        assert_eq!(loaded.settings.max_parallel, 1);
        assert_eq!(loaded.settings.version, SETTINGS_VERSION);
        assert_eq!(loaded.settings.language.as_deref(), Some("hu"));
        assert_eq!(loaded.settings.cooldown_secs, 120);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        // Written back, versioned, at once.
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["max_parallel"], 1);
        assert_eq!(stored["version"], SETTINGS_VERSION);
        assert_eq!(stored["cooldown_secs"], 120);
        // Another old value was a choice: kept.
        std::fs::write(&path, r#"{"max_parallel": 3}"#).unwrap();
        assert_eq!(store.load().max_parallel, 3);
    }

    #[test]
    fn version_1_files_lose_server_ip_netmask_and_show_all() {
        let dir = tempdir("settings-v1");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"version": 1, "max_parallel": 4, "server_ip": "192.168.88.1",
                "netmask": "255.255.255.0", "show_all_interfaces": true,
                "pool_start": "192.168.88.10", "pool_end": "192.168.88.20"}"#,
        )
        .unwrap();
        let loaded = SettingsStore::new(&path).load_checked();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(
            loaded.settings.max_parallel, 4,
            "chosen in a versioned file"
        );
        assert_eq!(
            loaded.settings.pool_start,
            Some(Ipv4Addr::new(192, 168, 88, 10))
        );
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["version"], SETTINGS_VERSION);
        for gone in ["server_ip", "netmask", "show_all_interfaces"] {
            assert!(stored.get(gone).is_none(), "{gone} is still stored");
        }
        assert!(!SettingsStore::new(&path).backup_path().exists());
    }

    #[test]
    fn a_stored_pool_equal_to_the_automatic_one_becomes_automatic() {
        let dir = tempdir("settings-v2-pool");
        let path = dir.join("settings.json");
        let auto = |iface: &str| {
            (iface == "en6")
                .then_some((Ipv4Addr::new(172, 30, 0, 3), Ipv4Addr::new(172, 30, 3, 254)))
        };
        let file = |start: &str, end: &str| {
            format!(
                r#"{{"version": 2, "last_interface": "en6", "pool_start": "{start}", "pool_end": "{end}"}}"#
            )
        };
        std::fs::write(&path, file("172.30.0.3", "172.30.3.254")).unwrap();
        let store = SettingsStore::new(&path).with_auto_pool(auto);
        let s = store.load();
        assert_eq!((s.pool_start, s.pool_end), (None, None), "automatic");
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["version"], SETTINGS_VERSION);
        assert_eq!(stored["pool_start"], json!(null));
        // A different range was a choice: kept.
        std::fs::write(&path, file("172.30.1.10", "172.30.1.20")).unwrap();
        let s = SettingsStore::new(&path).with_auto_pool(auto).load();
        assert_eq!(s.pool_start, Some(Ipv4Addr::new(172, 30, 1, 10)));
        // And never touched again once versioned.
        let s = SettingsStore::new(&path).with_auto_pool(auto).load();
        assert_eq!(s.pool_end, Some(Ipv4Addr::new(172, 30, 1, 20)));
    }

    #[test]
    fn a_file_from_a_newer_app_is_read_but_never_overwritten() {
        let dir = tempdir("settings-newer");
        let path = dir.join("settings.json");
        let text = r#"{"version": 99, "language": "hu", "max_parallel": 3, "hologram": true}"#;
        std::fs::write(&path, text).unwrap();
        let store = SettingsStore::new(&path);
        let loaded = store.load_checked();
        assert_eq!(loaded.settings.language.as_deref(), Some("hu"));
        assert_eq!(loaded.settings.max_parallel, 3);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert!(store.is_read_only());
        let err = store.save(&loaded.settings).unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("version")),
            ("settings_newer", Some("99"))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "untouched");
        assert!(!store.backup_path().exists());
    }

    #[test]
    fn an_explicit_four_in_a_versioned_file_stays() {
        let dir = tempdir("settings-four");
        let store = SettingsStore::new(dir.join("settings.json"));
        let s = Settings {
            max_parallel: 4,
            ..Settings::default()
        };
        store.save(&s).unwrap();
        assert_eq!(store.load().max_parallel, 4);
        assert_eq!(store.load().max_parallel, 4, "and again");
    }

    #[test]
    fn defaults_and_partial_files() {
        let s = Settings::default();
        assert_eq!((s.max_parallel, s.cooldown_secs), (1, 300));
        assert_eq!(s.version, SETTINGS_VERSION);
        assert_eq!(s.download_timeout(), Duration::from_secs(1800));
        assert!(s.validate().is_ok());
        let partial: Settings = serde_json::from_value(json!({"language": "hu"})).unwrap();
        assert_eq!(partial.language.as_deref(), Some("hu"));
        assert_eq!(partial.max_parallel, 1);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["pool_start"], json!(null));
        assert!(v.get("server_ip").is_none(), "comes from macOS now");
        assert_eq!(v["download_timeout_secs"], 1800);
    }

    #[test]
    fn validation() {
        let field = |s: &Settings| {
            let f = s.validate().unwrap_err();
            (f.code.clone(), f.param("field").map(String::from))
        };
        let mut s = Settings {
            language: Some("de".into()),
            ..Settings::default()
        };
        assert_eq!(
            field(&s),
            ("invalid_setting".into(), Some("language".into()))
        );
        s.language = Some("hu".into());
        s.pool_start = Some(Ipv4Addr::new(192, 168, 88, 100));
        assert_eq!(field(&s).1.as_deref(), Some("pool"));
        s.pool_end = Some(Ipv4Addr::new(192, 168, 88, 50));
        assert_eq!(field(&s).1.as_deref(), Some("pool"));
        s.pool_end = Some(Ipv4Addr::new(192, 168, 88, 150));
        assert!(s.validate().is_ok());
        s.max_parallel = 0;
        assert_eq!(field(&s).0, "invalid_max_parallel");
        s.max_parallel = 2;
        s.cooldown_secs = 10;
        assert_eq!(field(&s).0, "invalid_cooldown");
        for huge in [86_401, u64::MAX] {
            s.cooldown_secs = huge;
            let f = s.validate().unwrap_err();
            assert_eq!(
                (f.code.as_str(), f.param("max_secs")),
                ("invalid_cooldown", Some("86400"))
            );
        }
        s.cooldown_secs = 30;
        s.download_timeout_secs = 59;
        let f = s.validate().unwrap_err();
        assert_eq!(
            (f.param("field"), f.param("min")),
            (Some("download_timeout"), Some("60"))
        );
    }

    #[test]
    fn window_and_dialog_sizes_are_validated() {
        let field = |s: &Settings| s.validate().unwrap_err().param("field").map(String::from);
        let mut s = Settings {
            window: Some(WindowGeometry {
                x: -1200,
                y: 40,
                width: 1040,
                height: 680,
            }),
            ..Settings::default()
        };
        s.dialog_sizes.insert(
            "profile".into(),
            DialogSize {
                width: 640,
                height: 480,
            },
        );
        assert!(s.validate().is_ok());
        let ok = s.clone();
        for bad in [
            WindowGeometry {
                width: MIN_WINDOW_WIDTH - 1,
                ..ok.window.unwrap()
            },
            WindowGeometry {
                height: MIN_WINDOW_HEIGHT - 1,
                ..ok.window.unwrap()
            },
            WindowGeometry {
                width: MAX_SIDE + 1,
                ..ok.window.unwrap()
            },
            WindowGeometry {
                x: MAX_COORDINATE + 1,
                ..ok.window.unwrap()
            },
            WindowGeometry {
                y: -MAX_COORDINATE - 1,
                ..ok.window.unwrap()
            },
        ] {
            s.window = Some(bad);
            assert_eq!(field(&s).as_deref(), Some("window"), "{bad:?}");
        }
        s.window = ok.window;
        for (id, w, h) in [
            ("profile", MIN_DIALOG_SIDE - 1, 400),
            ("profile", 400, MAX_SIDE + 1),
            ("", 400, 400),
            ("Profile", 400, 400),
            ("../x", 400, 400),
            (&"x".repeat(33), 400, 400),
        ] {
            let mut t = ok.clone();
            t.dialog_sizes.insert(
                id.to_string(),
                DialogSize {
                    width: w,
                    height: h,
                },
            );
            assert_eq!(field(&t).as_deref(), Some("dialog_sizes"), "{id} {w}x{h}");
        }
        let mut many = ok.clone();
        for i in 0..=MAX_DIALOGS {
            many.dialog_sizes.insert(
                format!("d{i}"),
                DialogSize {
                    width: 400,
                    height: 400,
                },
            );
        }
        assert_eq!(field(&many).as_deref(), Some("dialog_sizes"));
    }

    #[test]
    fn version_3_files_gain_window_and_dialog_sizes() {
        let dir = tempdir("settings-v3");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"version": 3, "language": "hu", "max_parallel": 2}"#,
        )
        .unwrap();
        let loaded = SettingsStore::new(&path).load_checked();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.settings.window, None);
        assert!(loaded.settings.dialog_sizes.is_empty());
        assert_eq!(loaded.settings.max_parallel, 2);
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["version"], SETTINGS_VERSION);
        assert_eq!(stored["window"], json!(null));
        assert_eq!(stored["dialog_sizes"], json!({}));
        // Invalid sizes fall back alone; the rest is kept.
        std::fs::write(
            &path,
            json!({"version": 4, "language": "hu",
                "window": {"x": 0, "y": 0, "width": 10, "height": 10},
                "dialog_sizes": {"profile": {"width": 9, "height": 9}}})
            .to_string(),
        )
        .unwrap();
        let loaded = SettingsStore::new(&path).load_checked();
        assert_eq!(loaded.problems, ["window", "dialog_sizes"]);
        assert_eq!(loaded.settings.language.as_deref(), Some("hu"));
        assert_eq!(loaded.settings.window, None);
        assert!(loaded.settings.dialog_sizes.is_empty());
    }

    #[test]
    fn log_panel_height_is_validated() {
        let with = |h: u32| Settings {
            log_panel_height: Some(h),
            ..Settings::default()
        };
        assert!(Settings::default().validate().is_ok(), "none: the default");
        for ok in [MIN_LOG_PANEL_HEIGHT, 150, MAX_SIDE] {
            assert!(with(ok).validate().is_ok(), "{ok}");
        }
        for bad in [0, MIN_LOG_PANEL_HEIGHT - 1, MAX_SIDE + 1] {
            let err = with(bad).validate().unwrap_err();
            assert_eq!(
                (
                    err.code.as_str(),
                    err.param("field"),
                    err.param("min"),
                    err.param("max")
                ),
                (
                    "invalid_setting",
                    Some("log_panel_height"),
                    Some(MIN_LOG_PANEL_HEIGHT.to_string().as_str()),
                    Some(MAX_SIDE.to_string().as_str())
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn version_4_files_gain_log_panel_height() {
        let dir = tempdir("settings-v4");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            json!({"version": 4, "language": "hu",
                "dialog_sizes": {"profile": {"width": 640, "height": 480}}})
            .to_string(),
        )
        .unwrap();
        let store = SettingsStore::new(&path);
        let loaded = store.load_checked();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.settings.log_panel_height, None);
        assert_eq!(loaded.settings.language.as_deref(), Some("hu"));
        assert_eq!(loaded.settings.dialog_sizes.len(), 1);
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["version"], SETTINGS_VERSION);
        assert_eq!(stored["log_panel_height"], json!(null));
        assert_eq!(stored["dialog_sizes"]["profile"]["width"], 640);
        // A stored height round-trips.
        let mut s = loaded.settings.clone();
        s.log_panel_height = Some(220);
        store.save(&s).unwrap();
        assert_eq!(store.load().log_panel_height, Some(220));
        // A bad one falls back alone; the rest is kept.
        for bad in [json!(10), json!(MAX_SIDE + 1), json!("tall"), json!(-5)] {
            std::fs::write(
                &path,
                json!({"version": 5, "language": "hu", "log_panel_height": bad}).to_string(),
            )
            .unwrap();
            let loaded = SettingsStore::new(&path).load_checked();
            assert_eq!(loaded.problems, ["log_panel_height"], "{bad}");
            assert_eq!(loaded.settings.log_panel_height, None);
            assert_eq!(loaded.settings.language.as_deref(), Some("hu"));
        }
    }

    #[test]
    fn version_5_files_gain_onboarding_done() {
        let dir = tempdir("settings-v5");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            json!({"version": 5, "language": "hu", "log_panel_height": 200}).to_string(),
        )
        .unwrap();
        let store = SettingsStore::new(&path);
        let loaded = store.load_checked();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert!(!loaded.settings.onboarding_done, "not asked yet");
        assert_eq!(loaded.settings.log_panel_height, Some(200));
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["version"], SETTINGS_VERSION);
        assert_eq!(stored["onboarding_done"], json!(false));
        // Done round-trips; a wrong type falls back alone.
        let mut s = loaded.settings.clone();
        s.onboarding_done = true;
        store.save(&s).unwrap();
        assert!(store.load().onboarding_done);
        std::fs::write(
            &path,
            json!({"version": 6, "language": "hu", "onboarding_done": "yes"}).to_string(),
        )
        .unwrap();
        let loaded = SettingsStore::new(&path).load_checked();
        assert_eq!(loaded.problems, ["onboarding_done"]);
        assert!(!loaded.settings.onboarding_done);
        assert_eq!(loaded.settings.language.as_deref(), Some("hu"));
    }

    #[test]
    fn version_6_files_gain_keep_downloads_and_the_disclaimer() {
        let dir = tempdir("settings-v6");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            json!({"version": 6, "language": "hu", "onboarding_done": true}).to_string(),
        )
        .unwrap();
        let store = SettingsStore::new(&path);
        let loaded = store.load_checked();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert!(
            !loaded.settings.keep_downloads,
            "deleted on quit by default"
        );
        assert_eq!(loaded.settings.disclaimer_accepted_version, None);
        assert!(loaded.settings.onboarding_done);
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["version"], 7);
        assert_eq!(stored["keep_downloads"], json!(false));
        assert_eq!(stored["disclaimer_accepted_version"], json!(null));
        let mut s = loaded.settings.clone();
        s.keep_downloads = true;
        s.disclaimer_accepted_version = Some("1".into());
        store.save(&s).unwrap();
        let back = store.load();
        assert!(back.keep_downloads);
        assert_eq!(back.disclaimer_accepted_version.as_deref(), Some("1"));
        // Wrong types fall back alone.
        std::fs::write(
            &path,
            json!({"version": 7, "language": "hu", "keep_downloads": "yes",
                   "disclaimer_accepted_version": ""})
            .to_string(),
        )
        .unwrap();
        let loaded = SettingsStore::new(&path).load_checked();
        assert_eq!(
            loaded.problems,
            ["keep_downloads", "disclaimer_accepted_version"]
        );
        assert_eq!(loaded.settings.language.as_deref(), Some("hu"));
        // The settings dialog cannot accept (or revoke) the disclaimer, but
        // sets keep_downloads.
        let current = Settings {
            disclaimer_accepted_version: Some("1".into()),
            ..Settings::default()
        };
        let merged = current.merged_edit(Settings {
            keep_downloads: true,
            ..Settings::default()
        });
        assert!(merged.keep_downloads);
        assert_eq!(merged.disclaimer_accepted_version.as_deref(), Some("1"));
    }

    #[test]
    fn the_settings_dialog_keeps_the_window_and_dialog_sizes() {
        let mut current = Settings {
            window: Some(WindowGeometry {
                x: 10,
                y: 20,
                width: 1100,
                height: 700,
            }),
            ..Settings::default()
        };
        current.dialog_sizes.insert(
            "settings".into(),
            DialogSize {
                width: 600,
                height: 500,
            },
        );
        current.log_panel_height = Some(300);
        current.onboarding_done = true;
        let merged = current.merged_edit(Settings::default());
        assert_eq!(merged.log_panel_height, Some(300));
        assert!(merged.onboarding_done);
        assert_eq!(merged.window, current.window);
        assert_eq!(merged.dialog_sizes, current.dialog_sizes);
    }

    #[test]
    fn save_and_load_round_trip_atomically() {
        let dir = tempdir("settings");
        let store = SettingsStore::new(dir.join("sub/settings.json"));
        assert_eq!(store.load(), Settings::default(), "no file: defaults");
        let s = Settings {
            language: Some("hu".into()),
            last_interface: Some("en7".into()),
            pool_start: Some(Ipv4Addr::new(192, 168, 88, 10)),
            pool_end: Some(Ipv4Addr::new(192, 168, 88, 20)),
            auto_repeat: true,
            ..Settings::default()
        };
        store.save(&s).unwrap();
        assert_eq!(store.load(), s);
        let names: Vec<_> = std::fs::read_dir(dir.join("sub"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["settings.json"], "no temporary file left");
        let bad = Settings {
            max_parallel: 0,
            ..s.clone()
        };
        assert_eq!(store.save(&bad).unwrap_err().code, "invalid_max_parallel");
        assert_eq!(store.load(), s, "an invalid save changes nothing");
    }

    fn backup(path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path.with_extension("json.bad")).ok()
    }

    #[test]
    fn concurrent_saves_do_not_collide() {
        let dir = tempdir("settings-concurrent");
        let store = std::sync::Arc::new(SettingsStore::new(dir.join("settings.json")));
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for i in 0..25 {
                        let s = Settings {
                            max_parallel: 1 + (t + i) % 8,
                            ..Settings::default()
                        };
                        store.save(&s).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["settings.json"], "no temporary file left");
        assert!(store.load_checked().problems.is_empty());
    }

    #[test]
    fn an_unparsable_file_is_backed_up_before_anything_else() {
        let dir = tempdir("settings-unparsable");
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{not json").unwrap();
        let store = SettingsStore::new(&path);
        let loaded = store.load_checked();
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(loaded.problems, ["unparsable"]);
        assert_eq!(backup(&path).as_deref(), Some(&b"{not json"[..]));
        store.save(&Settings::default()).unwrap();
        assert_eq!(
            backup(&path).as_deref(),
            Some(&b"{not json"[..]),
            "the backup survives the next save"
        );
    }

    #[test]
    fn invalid_fields_fall_back_one_by_one() {
        let dir = tempdir("settings-fields");
        let path = dir.join("settings.json");
        let text = json!({
            "language": "de",
            "last_interface": "en7",
            "max_parallel": 0,
            "cooldown_secs": "soon",
            "pool_start": "192.168.88.10",
            "download_timeout_secs": 900,
            "auto_repeat": true,
            "future_option": 1
        })
        .to_string();
        std::fs::write(&path, &text).unwrap();
        let loaded = SettingsStore::new(&path).load_checked();
        let s = loaded.settings;
        assert_eq!(s.language, None);
        assert_eq!(s.last_interface.as_deref(), Some("en7"));
        assert_eq!((s.max_parallel, s.cooldown_secs), (1, 300));
        assert_eq!((s.pool_start, s.pool_end), (None, None));
        assert_eq!((s.download_timeout_secs, s.auto_repeat), (900, true));
        assert_eq!(
            loaded.problems,
            [
                "language",
                "pool",
                "max_parallel",
                "cooldown_secs",
                "future_option"
            ]
        );
        assert_eq!(backup(&path), Some(text.into_bytes()), "data was dropped");
    }

    #[test]
    fn a_valid_file_makes_no_backup() {
        let dir = tempdir("settings-valid");
        let path = dir.join("settings.json");
        let store = SettingsStore::new(&path);
        store.save(&Settings::default()).unwrap();
        assert!(store.load_checked().problems.is_empty());
        assert_eq!(backup(&path), None);
    }

    #[test]
    fn a_broken_file_gives_defaults() {
        let dir = tempdir("settings-broken");
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(SettingsStore::new(&path).load(), Settings::default());
        std::fs::write(&path, br#"{"max_parallel": 0}"#).unwrap();
        assert_eq!(SettingsStore::new(&path).load(), Settings::default());
    }

    #[test]
    fn an_unwritable_location_is_settings_io() {
        let dir = tempdir("settings-ro");
        std::fs::write(dir.join("file"), b"x").unwrap();
        let store = SettingsStore::new(dir.join("file/settings.json"));
        let err = store.save(&Settings::default()).unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("op")),
            ("settings_io", Some("mkdir"))
        );
    }
}
