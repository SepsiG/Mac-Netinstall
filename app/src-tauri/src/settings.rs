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
pub const SETTINGS_VERSION: u32 = 2;
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
        Ok(())
    }

    pub fn download_timeout(&self) -> Duration {
        Duration::from_secs(self.download_timeout_secs)
    }

    /// `edit` (from the settings dialog) with what the app remembers by
    /// itself kept from `self`: the last interface, profile and
    /// auto-repeat. A dialog opened before a start must not undo what the
    /// start remembered.
    pub fn merged_edit(&self, edit: Settings) -> Settings {
        Settings {
            last_interface: self.last_interface.clone(),
            last_profile: self.last_profile.clone(),
            auto_repeat: self.auto_repeat,
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
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
    /// The version of a file written by a newer app (0: none): it is read
    /// but never overwritten (see [`SettingsStore::save`]).
    newer: std::sync::Arc<AtomicU64>,
}

fn io_failure(op: &str, e: std::io::Error) -> Failure {
    Failure::new("settings_io").with("op", op).with("detail", e)
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SettingsStore {
            path: path.into(),
            newer: Default::default(),
        }
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
        assert_eq!(stored["version"], 2);
        for gone in ["server_ip", "netmask", "show_all_interfaces"] {
            assert!(stored.get(gone).is_none(), "{gone} is still stored");
        }
        assert!(!SettingsStore::new(&path).backup_path().exists());
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
