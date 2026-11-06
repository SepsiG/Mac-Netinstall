//! Install profiles: what to put on a device, stored as JSON files.
//!
//! ```json
//! {
//!   "name": "hAP ax2 stable",
//!   "version": "stable",
//!   "extra_packages": ["wifi-qcom"],
//!   "config": { "mode": "script", "local_name": "setup.rsc" },
//!   "mode_script": "mode.rsc",
//!   "remove_branding": false,
//!   "license_key": null,
//!   "after_install": "reboot"
//! }
//! ```
//!
//! `version` is a channel (`stable`, `long-term`, `testing`,
//! `development`) or an exact version (`7.25.1`). Scripts name files
//! registered with `ni_assets::Assets::register_local_file`. The
//! configuration options are the official `netinstall-cli`'s (`-r`, `-e`,
//! `-s`, `-sm`, `-b`, `-k`, `--reboot`, `--shutdown`); see
//! `docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md`.
//!
//! Older profiles (Plans 4–6) load unchanged: `{"mode": "default"}` is
//! still the default configuration, `{"mode": "custom_script", ...}` reads
//! as `script`, and the fields added later take their defaults. They are
//! written in the new form the next time they are saved.

use crate::event::Failure;
use ni_assets::npk::valid_package_name;
use ni_assets::{VersionSpec, BUNDLE_ID, MAX_LOCAL_NAME};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// Longest profile name, in bytes.
pub const MAX_NAME: usize = 64;
/// Most extra packages in one profile.
pub const MAX_EXTRA_PACKAGES: usize = 32;

/// `VersionSpec` as a plain string: `"stable"` or `"7.25.1"`.
pub mod spec_str {
    use ni_assets::VersionSpec;
    use serde::{de, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(spec: &VersionSpec, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::spec_to_string(spec))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<VersionSpec, D::Error> {
        String::deserialize(d)?.parse().map_err(de::Error::custom)
    }
}

/// The text form of a [`VersionSpec`] (what `VersionSpec::from_str` parses).
pub fn spec_to_string(spec: &VersionSpec) -> String {
    match spec {
        VersionSpec::Channel(c) => c.as_str().to_string(),
        VersionSpec::Exact(v) => v.as_str().to_string(),
    }
}

/// What Netinstall does about the device configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ConfigMode {
    /// Keep the existing configuration (the official tool's default).
    Keep,
    /// RouterOS applies its default configuration (`-r`).
    #[default]
    Default,
    /// Start without any configuration (`-e`).
    Empty,
    /// Run this registered local `.rsc` file on first boot (`-s`),
    /// uploaded as `autorun.scr`. Plans 4–6 called it `custom_script`.
    #[serde(alias = "custom_script")]
    Script { local_name: String },
}

impl ConfigMode {
    /// `keep`, `default`, `empty` or `script` (as in the JSON).
    pub fn name(&self) -> &'static str {
        match self {
            ConfigMode::Keep => "keep",
            ConfigMode::Default => "default",
            ConfigMode::Empty => "empty",
            ConfigMode::Script { .. } => "script",
        }
    }
}

/// What the device does when the install is done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterInstall {
    /// Reboot (the official tool's default).
    #[default]
    Reboot,
    /// Reboot with the message "Installation successful" (`--reboot`).
    RebootMessage,
    /// Shut down (`--shutdown`).
    Shutdown,
}

impl From<AfterInstall> for ni_proto::AfterInstall {
    fn from(a: AfterInstall) -> Self {
        match a {
            AfterInstall::Reboot => ni_proto::AfterInstall::Reboot,
            AfterInstall::RebootMessage => ni_proto::AfterInstall::RebootWithMessage,
            AfterInstall::Shutdown => ni_proto::AfterInstall::Shutdown,
        }
    }
}

/// Longest license key, in bytes (after line breaks are removed).
pub const MAX_LICENSE_KEY: usize = ni_proto::message::MAX_LICENSE_KEY;

/// A license key from a key file's contents (the official tool's `-k`):
/// line breaks removed. Error: `invalid_license_key` (`max`) unless the
/// rest is 1..=[`MAX_LICENSE_KEY`] printable ASCII characters.
pub fn license_key_from_file(data: &[u8]) -> Result<String, Failure> {
    let key: Vec<u8> = data
        .iter()
        .copied()
        .filter(|b| !matches!(b, b'\n' | b'\r'))
        .collect();
    let key = String::from_utf8(key).unwrap_or_default();
    check_license_key(&key)?;
    Ok(key)
}

fn check_license_key(key: &str) -> Result<(), Failure> {
    let printable = key.bytes().all(|b| (0x20..0x7f).contains(&b));
    if key.is_empty() || key.len() > MAX_LICENSE_KEY || !printable {
        return Err(Failure::new("invalid_license_key").with("max", MAX_LICENSE_KEY));
    }
    Ok(())
}

/// A reusable install recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(with = "spec_str")]
    pub version: VersionSpec,
    /// Packages besides `routeros`, e.g. `wifi-qcom`.
    #[serde(default)]
    pub extra_packages: Vec<String>,
    #[serde(default)]
    pub config: ConfigMode,
    /// The official tool's mode script (`-sm`): a registered local `.rsc`
    /// file, uploaded as `mode.scr`. Not with [`ConfigMode::Keep`].
    #[serde(default)]
    pub mode_script: Option<String>,
    /// The official tool's `-b` (what devices do with it is unverified).
    #[serde(default)]
    pub remove_branding: bool,
    /// Software license key ([`license_key_from_file`]).
    #[serde(default)]
    pub license_key: Option<String>,
    #[serde(default)]
    pub after_install: AfterInstall,
}

impl Profile {
    /// A profile installing just `routeros` of `version` with the default
    /// configuration.
    pub fn new(name: &str, version: VersionSpec) -> Self {
        Profile {
            name: name.to_string(),
            version,
            extra_packages: Vec::new(),
            config: ConfigMode::Default,
            mode_script: None,
            remove_branding: false,
            license_key: None,
            after_install: AfterInstall::Reboot,
        }
    }

    /// The registered local files the profile uses: its script, then its
    /// mode script.
    pub fn scripts(&self) -> Vec<&str> {
        let script = match &self.config {
            ConfigMode::Script { local_name } => Some(local_name.as_str()),
            _ => None,
        };
        script
            .into_iter()
            .chain(self.mode_script.as_deref())
            .collect()
    }

    /// Check the profile can be stored and used. Codes: `invalid_name`,
    /// `invalid_package_name`, `duplicate_package`, `too_many_packages`,
    /// `invalid_script_name` (`name`), `mode_script_with_keep` (the
    /// official tool refuses it too), `invalid_license_key` (`max`).
    pub fn validate(&self) -> Result<(), Failure> {
        if !valid_name(&self.name) {
            return Err(Failure::new("invalid_name").with("name", &self.name));
        }
        if self.extra_packages.len() > MAX_EXTRA_PACKAGES {
            return Err(Failure::new("too_many_packages").with("max", MAX_EXTRA_PACKAGES));
        }
        for (i, p) in self.extra_packages.iter().enumerate() {
            if !valid_package_name(p) || p == "routeros" {
                return Err(Failure::new("invalid_package_name").with("package", p));
            }
            if self.extra_packages[..i].contains(p) {
                return Err(Failure::new("duplicate_package").with("package", p));
            }
        }
        for local_name in self.scripts() {
            let ok = !local_name.is_empty()
                && local_name.len() <= MAX_LOCAL_NAME
                && local_name.ends_with(".rsc")
                && !local_name.contains(['/', '\\'])
                && !local_name.starts_with('.')
                && *local_name == local_name.to_lowercase();
            if !ok {
                return Err(Failure::new("invalid_script_name").with("name", local_name));
            }
        }
        if self.mode_script.is_some() && self.config == ConfigMode::Keep {
            return Err(Failure::new("mode_script_with_keep"));
        }
        if let Some(key) = &self.license_key {
            check_license_key(key)?;
        }
        Ok(())
    }
}

/// Names are also file names: 1-64 bytes of letters, digits, space, `.`,
/// `_`, `-`, `+`; not starting with `.` or space, not ending with space.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with(['.', ' '])
        && !name.ends_with(' ')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._-+".contains(c))
}

/// `~/Library/Application Support/io.github.sepsig.mac-netinstall/profiles`,
/// or `None` if `HOME` is not set.
pub fn default_profiles_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support")
            .join(BUNDLE_ID)
            .join("profiles"),
    )
}

/// Profiles as `<root>/<lowercase name>.json`, one file each. File names
/// are lowercase because APFS is case-insensitive: `Lab` and `lab` are the
/// same profile.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    root: PathBuf,
}

/// Result of [`ProfileStore::list`].
#[derive(Debug, Default)]
pub struct Listing {
    /// Valid profiles, sorted by name (case-insensitive).
    pub profiles: Vec<Profile>,
    /// Files that could not be read, parsed or validated, with the reason.
    pub broken: Vec<(PathBuf, Failure)>,
}

fn io_failure(op: &str, path: &Path, e: io::Error) -> Failure {
    Failure::new("profile_io")
        .with("op", op)
        .with("path", path.display())
        .with("detail", e)
}

impl ProfileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ProfileStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, name: &str) -> Result<PathBuf, Failure> {
        if !valid_name(name) {
            return Err(Failure::new("invalid_name").with("name", name));
        }
        Ok(self.root.join(format!("{}.json", name.to_lowercase())))
    }

    /// Load one profile. `profile_not_found` if there is none; a stored
    /// file that fails validation is `invalid_profile`.
    pub fn load(&self, name: &str) -> Result<Profile, Failure> {
        let path = self.path(name)?;
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(Failure::new("profile_not_found").with("name", name))
            }
            Err(e) => return Err(io_failure("read", &path, e)),
        };
        parse(&data, &path)
    }

    /// Validate and write (replacing a profile of the same name, whatever
    /// its case). Written to a temporary file first, then renamed.
    pub fn save(&self, profile: &Profile) -> Result<(), Failure> {
        profile.validate()?;
        let path = self.path(&profile.name)?;
        std::fs::create_dir_all(&self.root).map_err(|e| io_failure("mkdir", &self.root, e))?;
        let mut json = serde_json::to_vec_pretty(profile).expect("profiles serialize");
        json.push(b'\n');
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, &json).map_err(|e| io_failure("write", &tmp, e))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io_failure("rename", &path, e)
        })
    }

    /// Remove a profile; `profile_not_found` if there is none.
    pub fn delete(&self, name: &str) -> Result<(), Failure> {
        let path = self.path(name)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(Failure::new("profile_not_found").with("name", name))
            }
            Err(e) => Err(io_failure("remove", &path, e)),
        }
    }

    /// All `*.json` files in the root (a missing root is empty).
    pub fn list(&self) -> Listing {
        let mut out = Listing::default();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let loaded = std::fs::read(&path)
                .map_err(|e| io_failure("read", &path, e))
                .and_then(|data| parse(&data, &path));
            match loaded {
                Ok(p) => out.profiles.push(p),
                Err(f) => out.broken.push((path, f)),
            }
        }
        out.profiles.sort_by_key(|p| p.name.to_lowercase());
        out.broken.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

fn parse(data: &[u8], path: &Path) -> Result<Profile, Failure> {
    let profile: Profile = serde_json::from_slice(data).map_err(|e| {
        Failure::new("invalid_profile")
            .with("path", path.display())
            .with("detail", e)
    })?;
    profile.validate().map_err(|f| {
        Failure::new("invalid_profile")
            .with("path", path.display())
            .with("detail", f)
    })?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if stem != profile.name.to_lowercase() {
        return Err(Failure::new("invalid_profile")
            .with("path", path.display())
            .with("detail", "file name does not match the profile name"));
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_assets::testutil::tempdir;
    use ni_assets::{Channel, Version};
    use serde_json::json;

    fn sample() -> Profile {
        Profile {
            name: "hAP ax2 stable".into(),
            version: VersionSpec::Channel(Channel::Stable),
            extra_packages: vec!["wifi-qcom".into()],
            config: ConfigMode::Script {
                local_name: "setup.rsc".into(),
            },
            mode_script: Some("mode.rsc".into()),
            remove_branding: true,
            license_key: Some("KEY-1".into()),
            after_install: AfterInstall::Shutdown,
        }
    }

    #[test]
    fn json_format() {
        let v = serde_json::to_value(sample()).unwrap();
        assert_eq!(
            v,
            json!({"name": "hAP ax2 stable", "version": "stable",
                   "extra_packages": ["wifi-qcom"],
                   "config": {"mode": "script", "local_name": "setup.rsc"},
                   "mode_script": "mode.rsc", "remove_branding": true,
                   "license_key": "KEY-1", "after_install": "shutdown"})
        );
        let minimal: Profile =
            serde_json::from_value(json!({"name": "x", "version": "7.25.1"})).unwrap();
        assert_eq!(
            minimal,
            Profile::new("x", VersionSpec::Exact(Version::parse("7.25.1").unwrap()))
        );
        assert!(
            serde_json::from_value::<Profile>(json!({"name": "x", "version": "6.49"})).is_err()
        );
        for (mode, want) in [
            ("keep", ConfigMode::Keep),
            ("default", ConfigMode::Default),
            ("empty", ConfigMode::Empty),
        ] {
            let p: Profile = serde_json::from_value(
                json!({"name": "x", "version": "7.1", "config": {"mode": mode}}),
            )
            .unwrap();
            assert_eq!((p.config.name(), &p.config), (mode, &want));
        }
        let after: Vec<AfterInstall> = ["reboot", "reboot_message", "shutdown"]
            .iter()
            .map(|a| serde_json::from_value(json!(a)).unwrap())
            .collect();
        assert_eq!(
            after,
            [
                AfterInstall::Reboot,
                AfterInstall::RebootMessage,
                AfterInstall::Shutdown
            ]
        );
    }

    /// A profile file written by Plans 4–6.
    #[test]
    fn old_profiles_load_and_are_saved_in_the_new_form() {
        let dir = tempdir("profiles-old");
        let store = ProfileStore::new(dir.to_path_buf());
        std::fs::write(
            dir.join("lab.json"),
            br#"{"name": "Lab", "version": "stable", "extra_packages": [],
                "config": {"mode": "custom_script", "local_name": "lab.rsc"}}"#,
        )
        .unwrap();
        let p = store.load("Lab").unwrap();
        assert_eq!(
            p.config,
            ConfigMode::Script {
                local_name: "lab.rsc".into()
            }
        );
        assert_eq!(
            (
                p.mode_script.as_deref(),
                p.remove_branding,
                p.license_key.as_deref()
            ),
            (None, false, None)
        );
        assert_eq!(p.after_install, AfterInstall::Reboot);
        store.save(&p).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("lab.json")).unwrap()).unwrap();
        assert_eq!(
            v["config"],
            json!({"mode": "script", "local_name": "lab.rsc"})
        );
        assert_eq!(v["after_install"], "reboot");
    }

    #[test]
    fn license_keys_come_from_key_files() {
        let file = b"-----BEGIN MIKROTIK SOFTWARE KEY------------\r\nAAAA\nBBBB==\n-----END\n";
        assert_eq!(
            license_key_from_file(file).unwrap(),
            "-----BEGIN MIKROTIK SOFTWARE KEY------------AAAABBBB==-----END"
        );
        for bad in [
            &b""[..],
            b"\n\r\n",
            b"key\0x",
            "kulcs\u{e9}".as_bytes(),
            &[b'k'; MAX_LICENSE_KEY + 1],
        ] {
            let e = license_key_from_file(bad).unwrap_err();
            assert_eq!(e.code, "invalid_license_key", "{bad:?}");
            assert_eq!(e.param("max"), Some("4096"));
        }
        assert!(license_key_from_file(&[b'k'; MAX_LICENSE_KEY]).is_ok());
    }

    #[test]
    fn validation_codes() {
        assert_eq!(sample().validate(), Ok(()));
        let code = |p: Profile| p.validate().unwrap_err().code;
        for bad in [
            "",
            ".hidden",
            " lead",
            "trail ",
            "a/b",
            "ü",
            &"x".repeat(65),
        ] {
            let mut p = sample();
            p.name = bad.into();
            assert_eq!(code(p), "invalid_name", "{bad:?}");
        }
        let mut p = sample();
        p.extra_packages = vec!["Wifi".into()];
        assert_eq!(code(p), "invalid_package_name");
        let mut p = sample();
        p.extra_packages = vec!["routeros".into()];
        assert_eq!(code(p), "invalid_package_name");
        let mut p = sample();
        p.extra_packages = vec!["a".into(), "a".into()];
        assert_eq!(code(p), "duplicate_package");
        let mut p = sample();
        p.extra_packages = (0..33).map(|i| format!("p{i}")).collect();
        assert_eq!(code(p), "too_many_packages");
        for bad in ["", "setup.txt", "Setup.rsc", "../x.rsc", ".rsc"] {
            let mut p = sample();
            p.config = ConfigMode::Script {
                local_name: bad.into(),
            };
            assert_eq!(code(p), "invalid_script_name", "{bad:?}");
            let mut p = sample();
            p.mode_script = Some(bad.into());
            assert_eq!(code(p), "invalid_script_name", "mode script {bad:?}");
        }
        let mut p = sample();
        p.config = ConfigMode::Keep;
        assert_eq!(code(p.clone()), "mode_script_with_keep");
        p.mode_script = None;
        assert_eq!(p.validate(), Ok(()));
        for mode in [ConfigMode::Default, ConfigMode::Empty] {
            let mut p = sample();
            p.config = mode;
            assert_eq!(p.validate(), Ok(()), "a mode script with -r / -e");
        }
        let mut p = sample();
        p.license_key = Some("two\nlines".into());
        assert_eq!(code(p), "invalid_license_key");
        assert_eq!(sample().scripts(), ["setup.rsc", "mode.rsc"]);
    }

    #[test]
    fn store_save_load_list_delete() {
        let dir = tempdir("profiles");
        let store = ProfileStore::new(dir.join("profiles"));
        assert!(store.list().profiles.is_empty(), "missing root is empty");
        store.save(&sample()).unwrap();
        let mut other = Profile::new("Lab", VersionSpec::Channel(Channel::LongTerm));
        store.save(&other).unwrap();
        assert!(dir.join("profiles/hap ax2 stable.json").is_file());
        assert_eq!(store.load("HAP AX2 STABLE").unwrap(), sample());
        other.extra_packages.push("container".into());
        store.save(&other).unwrap();
        let names: Vec<String> = store.list().profiles.into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["hAP ax2 stable", "Lab"]);
        assert_eq!(store.load("lab").unwrap().extra_packages, ["container"]);
        store.delete("LAB").unwrap();
        assert_eq!(store.load("Lab").unwrap_err().code, "profile_not_found");
        assert_eq!(store.delete("Lab").unwrap_err().code, "profile_not_found");
        assert_eq!(store.load("../etc").unwrap_err().code, "invalid_name");
        let leftovers: Vec<_> = std::fs::read_dir(store.root())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "no temporary files left: {leftovers:?}");
    }

    #[test]
    fn broken_files_are_listed_separately() {
        let dir = tempdir("profiles-broken");
        let store = ProfileStore::new(dir.to_path_buf());
        store.save(&sample()).unwrap();
        std::fs::write(dir.join("garbage.json"), b"{nope").unwrap();
        std::fs::write(
            dir.join("renamed.json"),
            br#"{"name": "other", "version": "stable"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), b"ignored").unwrap();
        let listing = store.list();
        assert_eq!(listing.profiles, [sample()]);
        let broken: Vec<(String, String)> = listing
            .broken
            .iter()
            .map(|(p, f)| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                (name, f.code.clone())
            })
            .collect();
        assert_eq!(
            broken,
            [
                ("garbage.json".to_string(), "invalid_profile".to_string()),
                ("renamed.json".to_string(), "invalid_profile".to_string())
            ]
        );
        assert_eq!(store.load("garbage").unwrap_err().code, "invalid_profile");
    }

    #[test]
    fn default_root_is_in_application_support() {
        let root = default_profiles_root().unwrap();
        assert!(
            root.ends_with("Library/Application Support/io.github.sepsig.mac-netinstall/profiles")
        );
    }
}
