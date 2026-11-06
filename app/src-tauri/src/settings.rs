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

/// What the app remembers between runs. Missing fields take their
/// defaults, so older files keep working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `"en"` or `"hu"`; `None` follows the macOS language.
    pub language: Option<String>,
    pub last_interface: Option<String>,
    /// Also list Wi-Fi, bridges, tunnels, ... in the interface picker.
    pub show_all_interfaces: bool,
    /// Server IP to use while the interface has no address (no cable
    /// yet); default: the interface's address.
    pub server_ip: Option<Ipv4Addr>,
    pub netmask: Option<Ipv4Addr>,
    /// BOOTP pool (both or neither); default: the subnet.
    pub pool_start: Option<Ipv4Addr>,
    pub pool_end: Option<Ipv4Addr>,
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
            language: None,
            last_interface: None,
            show_all_interfaces: false,
            server_ip: None,
            netmask: None,
            pool_start: None,
            pool_end: None,
            max_parallel: 4,
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
        if let Some(mask) = self.netmask {
            let m = u32::from(mask);
            if m.leading_ones() + m.trailing_zeros() != 32 || m.leading_ones() < 8 {
                return Err(invalid("netmask"));
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
    "show_all_interfaces",
    "server_ip",
    "netmask",
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
        "show_all_interfaces" => get(obj, field, &mut s.show_all_interfaces),
        "server_ip" => get(obj, field, &mut s.server_ip),
        "netmask" => get(obj, field, &mut s.netmask),
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
}

fn io_failure(op: &str, e: std::io::Error) -> Failure {
    Failure::new("settings_io").with("op", op).with("detail", e)
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SettingsStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored settings ([`SettingsStore::load_checked`]).
    pub fn load(&self) -> Settings {
        self.load_checked().settings
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
        let known = |k: &str| FIELDS.contains(&k) || k == "pool_start" || k == "pool_end";
        loaded
            .problems
            .extend(obj.keys().filter(|k| !known(k)).cloned());
        if !loaded.problems.is_empty() {
            self.keep_backup(&data, &loaded.problems);
        }
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
        settings.validate()?;
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
    fn defaults_and_partial_files() {
        let s = Settings::default();
        assert_eq!((s.max_parallel, s.cooldown_secs), (4, 300));
        assert_eq!(s.download_timeout(), Duration::from_secs(1800));
        assert!(s.validate().is_ok());
        let partial: Settings = serde_json::from_value(json!({"language": "hu"})).unwrap();
        assert_eq!(partial.language.as_deref(), Some("hu"));
        assert_eq!(partial.max_parallel, 4);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["server_ip"], json!(null));
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
        s.netmask = Some(Ipv4Addr::new(255, 0, 255, 0));
        assert_eq!(field(&s).1.as_deref(), Some("netmask"));
        s.netmask = Some(Ipv4Addr::new(255, 255, 255, 0));
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
            server_ip: Some(Ipv4Addr::new(192, 168, 88, 1)),
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
        assert_eq!((s.max_parallel, s.cooldown_secs), (4, 300));
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
