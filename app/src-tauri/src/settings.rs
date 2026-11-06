//! App settings: one JSON file in Application Support, written atomically.

use ni_engine::{check_cooldown, check_max_parallel, Failure, MIN_COOLDOWN};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
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
}

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

    /// The stored settings; defaults if there is no file, or if it cannot
    /// be read or parsed (a broken file must not lock the user out; it is
    /// replaced on the next save).
    pub fn load(&self) -> Settings {
        let Ok(data) = std::fs::read(&self.path) else {
            return Settings::default();
        };
        match serde_json::from_slice::<Settings>(&data) {
            Ok(s) if s.validate().is_ok() => s,
            Ok(_) | Err(_) => {
                tracing::warn!(path = %self.path.display(), "ignoring invalid settings file");
                Settings::default()
            }
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
        let tmp = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
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
