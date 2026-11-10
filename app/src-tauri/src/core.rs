//! Everything the commands do, without Tauri: an [`AppCore`] owns the
//! engine, the profile store and the settings. The `#[tauri::command]`
//! functions in `commands.rs` only unwrap their arguments and call it, so
//! this is tested directly (see `tests/bridge.rs`).

use crate::settings::{Settings, SettingsStore};
use crate::views::{
    without_paths, BrokenProfile, CacheView, ChannelView, InterfaceView, LicenseKeyView,
    PackageFileView, PackagesView, ProfilesView, ScriptView, StartRequest, VersionsView,
};
use ni_assets::{
    default_cache_root, item_version, AssetError, Channel, DeleteScope, HttpConfig, LocalKind,
    PkgArch, Version, VersionSpec,
};
use ni_engine::{
    default_profiles_root, license_key_from_file, preflight, EngineConfig, EngineHandle, Failure,
    PreflightReport, Profile, ProfileStore, Snapshot, StartInfo,
};
use ni_proto::Mac;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::watch;

/// Largest key file read (MikroTik key files are a few hundred bytes).
pub const MAX_KEY_FILE: u64 = 64 * 1024;
/// How long the stored version list is used before it is fetched again.
pub const VERSION_LIST_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Where the app keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Downloads (`~/Library/Caches/<bundle id>`).
    pub cache: PathBuf,
    /// Profiles (`~/Library/Application Support/<bundle id>/profiles`).
    pub profiles: PathBuf,
    /// `~/Library/Application Support/<bundle id>/settings.json`.
    pub settings: PathBuf,
}

impl Paths {
    /// The standard locations; `no_home` without `HOME`.
    pub fn for_user() -> Result<Paths, Failure> {
        let (Some(cache), Some(profiles)) = (default_cache_root(), default_profiles_root()) else {
            return Err(Failure::new("no_home"));
        };
        let settings = profiles.parent().map_or_else(
            || PathBuf::from("settings.json"),
            |p| p.join("settings.json"),
        );
        Ok(Paths {
            cache,
            profiles,
            settings,
        })
    }

    /// Everything under `root` (tests).
    pub fn under(root: &Path) -> Paths {
        Paths {
            cache: root.join("cache"),
            profiles: root.join("profiles"),
            settings: root.join("settings.json"),
        }
    }
}

/// Changes every [`EngineConfig`] the app builds (tests use it for
/// loopback ports and fast timeouts).
pub type ConfigHook = Arc<dyn Fn(&mut EngineConfig) + Send + Sync>;

/// `invalid_mac` (`mac`) unless `s` is a MAC address.
pub fn parse_mac(s: &str) -> Result<Mac, Failure> {
    s.parse()
        .map_err(|_| Failure::new("invalid_mac").with("mac", s))
}

/// The interfaces, for the picker. Error: `interface_list_failed`.
pub fn list_interfaces() -> Result<Vec<InterfaceView>, Failure> {
    let ifaces = ni_net::list_interfaces()
        .map_err(|e| Failure::new("interface_list_failed").with("detail", e))?;
    let routed = ni_net::default_route_interfaces().unwrap_or_default();
    Ok(ifaces
        .iter()
        .map(|i| InterfaceView::new(i, &routed))
        .collect())
}

/// `path` with the home folder shown as `~`.
pub fn display_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn asset_cause(code: &str, e: &AssetError) -> Failure {
    Failure::new(code).with("cause", e.code())
}

/// The app's state behind the commands.
pub struct AppCore {
    paths: Paths,
    http: HttpConfig,
    engine: watch::Sender<EngineHandle>,
    /// Download timeout of the running engine task's asset manager.
    engine_timeout: Mutex<Duration>,
    profiles: ProfileStore,
    store: SettingsStore,
    settings: Mutex<Settings>,
    /// Held while the engine task may be replaced and until a start on it
    /// is done, so a concurrent preflight cannot replace (orphan) the
    /// engine a start is starting.
    engine_swap: tokio::sync::Mutex<()>,
    hook: Option<ConfigHook>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AppCore {
    /// Load the settings and spawn the engine (inside a tokio runtime).
    /// `http` gives the mirror; its download timeout comes from the
    /// settings. Error: `assets_failed`.
    pub fn new(paths: Paths, http: HttpConfig) -> Result<AppCore, Failure> {
        let store = SettingsStore::new(&paths.settings);
        let settings = store.load();
        let timeout = settings.download_timeout();
        let engine = EngineHandle::spawn(
            &paths.cache,
            HttpConfig {
                download_timeout: timeout,
                ..http.clone()
            },
        )?;
        Ok(AppCore {
            profiles: ProfileStore::new(&paths.profiles),
            paths,
            http,
            engine: watch::Sender::new(engine),
            engine_timeout: Mutex::new(timeout),
            store,
            settings: Mutex::new(settings),
            engine_swap: tokio::sync::Mutex::new(()),
            hook: None,
        })
    }

    /// Apply `hook` to every engine configuration (tests).
    pub fn with_config_hook(mut self, hook: ConfigHook) -> Self {
        self.hook = Some(hook);
        self
    }

    /// The current engine task.
    pub fn engine(&self) -> EngineHandle {
        self.engine.borrow().clone()
    }

    /// Follows engine replacements (a new download timeout needs a new
    /// engine task); the event forwarder re-subscribes on each change.
    pub fn watch_engine(&self) -> watch::Receiver<EngineHandle> {
        self.engine.subscribe()
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    // ------------------------------------------------------------ settings

    pub fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    /// Validate, store and apply the settings dialog's values. What the app
    /// remembers by itself (last interface, profile, auto-repeat) is kept
    /// from the current state ([`Settings::merged_edit`]), so a dialog
    /// opened earlier cannot undo it. Returns the stored settings. While
    /// running, the concurrency limit and the cooldown take effect at once;
    /// a new download timeout applies from the next start.
    pub async fn set_settings(&self, edit: Settings) -> Result<Settings, Failure> {
        let s = {
            // Held across the save, so the file and the state cannot
            // disagree with a concurrent `remember`.
            let mut current = lock(&self.settings);
            let s = current.merged_edit(edit);
            self.store.save(&s)?;
            *current = s.clone();
            s
        };
        let engine = self.engine();
        if engine.snapshot().await?.running {
            engine.set_max_parallel(s.max_parallel).await?;
            engine
                .set_cooldown(Duration::from_secs(s.cooldown_secs))
                .await?;
        }
        Ok(s)
    }

    /// Remember the main window's geometry (ignored unless valid).
    pub fn remember_window(&self, g: crate::settings::WindowGeometry) {
        if g.is_valid() && self.settings().window != Some(g) {
            self.remember(|s| s.window = Some(g));
        }
    }

    /// Remember a dialog's size. Error: `invalid_setting` (`field`:
    /// `dialog_sizes`) for a bad id or size, or a new id past
    /// [`crate::settings::MAX_DIALOGS`].
    pub fn set_dialog_size(&self, id: &str, width: u32, height: u32) -> Result<(), Failure> {
        let size = crate::settings::DialogSize { width, height };
        let mut s = lock(&self.settings);
        let mut next = s.clone();
        next.dialog_sizes.insert(id.to_string(), size);
        if !crate::settings::valid_dialog_id(id) || next.validate().is_err() {
            return Err(Failure::new("invalid_setting").with("field", "dialog_sizes"));
        }
        if let Err(f) = self.store.save(&next) {
            tracing::warn!("cannot store settings: {f}");
        }
        *s = next;
        Ok(())
    }

    /// Remember the log panel's height. Error: `invalid_setting` (`field`:
    /// `log_panel_height`, `min`, `max`) outside
    /// [`crate::settings::MIN_LOG_PANEL_HEIGHT`]..=[`crate::settings::MAX_SIDE`].
    pub fn set_log_panel_height(&self, height: u32) -> Result<(), Failure> {
        let mut s = lock(&self.settings);
        let next = Settings {
            log_panel_height: Some(height),
            ..s.clone()
        };
        next.validate()?;
        if let Err(f) = self.store.save(&next) {
            tracing::warn!("cannot store settings: {f}");
        }
        *s = next;
        Ok(())
    }

    /// Remember that the permissions sheet was gone through (or not).
    pub fn set_onboarding_done(&self, done: bool) {
        if self.settings().onboarding_done != done {
            self.remember(|s| s.onboarding_done = done);
        }
    }

    /// Change a few fields and store them; a failure to store is only
    /// logged (remembering the last interface must not fail a start).
    fn remember(&self, change: impl FnOnce(&mut Settings)) {
        let mut s = lock(&self.settings);
        change(&mut s);
        if let Err(f) = self.store.save(&s) {
            tracing::warn!("cannot store settings: {f}");
        }
    }

    /// Replace the engine task if the download timeout changed and it is
    /// not running. The caller holds `engine_swap`.
    async fn ensure_engine(
        &self,
        _swap: &tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<EngineHandle, Failure> {
        let wanted = self.settings().download_timeout();
        let engine = self.engine();
        if *lock(&self.engine_timeout) == wanted || engine.snapshot().await?.running {
            return Ok(engine);
        }
        let fresh = EngineHandle::spawn(
            &self.paths.cache,
            HttpConfig {
                download_timeout: wanted,
                ..self.http.clone()
            },
        )?;
        *lock(&self.engine_timeout) = wanted;
        self.engine.send_replace(fresh.clone());
        tracing::info!(
            secs = wanted.as_secs(),
            "engine restarted for a new download timeout"
        );
        Ok(fresh)
    }

    // ------------------------------------------------------------ engine

    fn load_profile(&self, name: &str) -> Result<Profile, Failure> {
        self.profiles.load(name).map_err(without_paths)
    }

    /// The engine configuration for `req` and the current settings.
    pub fn engine_config(&self, req: &StartRequest) -> Result<EngineConfig, Failure> {
        let s = self.settings();
        let mut cfg = EngineConfig::new(&req.interface);
        // Server IP and netmask: the adapter's manual IPv4 in macOS.
        cfg.pool = s.pool_start.zip(s.pool_end);
        cfg.max_parallel = s.max_parallel;
        cfg.cooldown = Duration::from_secs(s.cooldown_secs);
        cfg.profile = req
            .profile
            .as_deref()
            .map(|n| self.load_profile(n))
            .transpose()?;
        cfg.auto_repeat = req.auto_repeat;
        if let Some(hook) = &self.hook {
            hook(&mut cfg);
        }
        Ok(cfg)
    }

    /// Run the preflight checks for `req`.
    pub async fn preflight(&self, req: &StartRequest) -> Result<PreflightReport, Failure> {
        let cfg = self.engine_config(req)?;
        let engine = {
            let swap = self.engine_swap.lock().await;
            self.ensure_engine(&swap).await?
        };
        Ok(preflight::run(&cfg, Some(engine.assets()), cfg.profile.as_ref()).await)
    }

    /// The quick, local preflight (interface, link, configured address):
    /// no port is bound and nothing is downloaded, so the window can repeat
    /// it every few seconds.
    pub async fn check_interface(&self, req: &StartRequest) -> Result<PreflightReport, Failure> {
        let cfg = self.engine_config(req)?;
        tokio::task::spawn_blocking(move || preflight::run_local(&cfg))
            .await
            .map_err(|e| Failure::new("interface_list_failed").with("detail", e))
    }

    /// Accept the current disclaimer ([`ni_engine::DISCLAIMER_VERSION`]):
    /// starting needs it.
    pub fn accept_disclaimer(&self) {
        self.remember(|s| {
            s.disclaimer_accepted_version = Some(ni_engine::DISCLAIMER_VERSION.to_string())
        });
    }

    /// Delete the downloaded RouterOS files unless the settings keep them
    /// (`keep_downloads`) or the engine is running (it may need them).
    /// Registered local files and the version lists stay. Best-effort:
    /// returns whether anything was attempted.
    pub async fn purge_downloads_if_wanted(&self) -> bool {
        if self.settings().keep_downloads {
            return false;
        }
        let engine = self.engine();
        if engine.snapshot().await.is_ok_and(|s| s.running) {
            return false;
        }
        match engine.assets().clear_downloads().await {
            Ok(()) => tracing::info!("downloaded RouterOS files deleted"),
            Err(e) => tracing::warn!("cannot delete downloaded files: {e}"),
        }
        true
    }

    /// Start serving; remembers the interface, profile and auto-repeat.
    /// Error: `disclaimer_not_accepted` until [`AppCore::accept_disclaimer`].
    pub async fn start(&self, req: &StartRequest) -> Result<StartInfo, Failure> {
        if !ni_engine::disclaimer::is_accepted(
            self.settings().disclaimer_accepted_version.as_deref(),
        ) {
            return Err(ni_engine::disclaimer::not_accepted());
        }
        let cfg = self.engine_config(req)?;
        let swap = self.engine_swap.lock().await;
        let engine = self.ensure_engine(&swap).await?;
        let info = engine.start(cfg).await?;
        drop(swap);
        self.remember(|s| {
            s.last_interface = Some(req.interface.clone());
            s.last_profile = req.profile.clone();
            s.auto_repeat = req.auto_repeat;
        });
        Ok(info)
    }

    pub async fn stop(&self) -> Result<(), Failure> {
        self.engine().stop().await
    }

    pub async fn snapshot(&self) -> Result<Snapshot, Failure> {
        self.engine().snapshot().await
    }

    pub async fn flash(&self, mac: &str, profile: &str) -> Result<(), Failure> {
        let mac = parse_mac(mac)?;
        let profile = self.load_profile(profile)?;
        self.engine().flash(mac, profile).await
    }

    pub async fn retry(&self, mac: &str) -> Result<(), Failure> {
        self.engine().retry(parse_mac(mac)?).await
    }

    pub async fn cancel(&self, mac: &str) -> Result<(), Failure> {
        self.engine().cancel(parse_mac(mac)?).await
    }

    pub async fn forget(&self, mac: &str) -> Result<(), Failure> {
        self.engine().forget(parse_mac(mac)?).await
    }

    pub async fn set_auto_repeat(&self, on: bool) -> Result<(), Failure> {
        self.engine().set_auto_repeat(on).await?;
        self.remember(|s| s.auto_repeat = on);
        Ok(())
    }

    /// Change the active profile by name (`None`: none).
    pub async fn set_active_profile(&self, name: Option<&str>) -> Result<(), Failure> {
        let profile = name.map(|n| self.load_profile(n)).transpose()?;
        self.engine().set_active_profile(profile).await?;
        self.remember(|s| s.last_profile = name.map(String::from));
        Ok(())
    }

    pub async fn set_only_mac(&self, mac: Option<&str>) -> Result<(), Failure> {
        let mac = mac.map(parse_mac).transpose()?;
        self.engine().set_only_mac(mac).await
    }

    pub async fn set_max_parallel(&self, n: usize) -> Result<(), Failure> {
        self.engine().set_max_parallel(n).await?;
        self.remember(|s| s.max_parallel = n);
        Ok(())
    }

    pub async fn set_cooldown(&self, secs: u64) -> Result<(), Failure> {
        self.engine()
            .set_cooldown(Duration::from_secs(secs))
            .await?;
        self.remember(|s| s.cooldown_secs = secs);
        Ok(())
    }

    // ------------------------------------------------------------ profiles

    pub fn list_profiles(&self) -> ProfilesView {
        let listing = self.profiles.list();
        ProfilesView {
            profiles: listing.profiles,
            broken: listing
                .broken
                .into_iter()
                .map(|(path, f)| BrokenProfile {
                    file: path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    code: f.code,
                })
                .collect(),
        }
    }

    fn profile_exists(&self, name: &str) -> bool {
        !matches!(self.profiles.load(name), Err(f) if f.code == "profile_not_found")
    }

    /// Store `profile`; `previous` is its name before editing (`None` for a
    /// new one). Errors: profile validation codes, `profile_exists`
    /// (`name`; another profile has that name), `profile_io`.
    pub fn save_profile(&self, profile: &Profile, previous: Option<&str>) -> Result<(), Failure> {
        profile.validate()?;
        let same = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();
        let renamed = previous.filter(|p| !same(p, &profile.name));
        if (previous.is_none() || renamed.is_some()) && self.profile_exists(&profile.name) {
            return Err(Failure::new("profile_exists").with("name", &profile.name));
        }
        self.profiles.save(profile).map_err(without_paths)?;
        if let Some(old) = renamed {
            match self.profiles.delete(old) {
                Ok(()) => {}
                Err(f) if f.code == "profile_not_found" => {}
                Err(f) => return Err(without_paths(f)),
            }
            if self.settings().last_profile.as_deref() == Some(old) {
                self.remember(|s| s.last_profile = Some(profile.name.clone()));
            }
        }
        Ok(())
    }

    pub fn delete_profile(&self, name: &str) -> Result<(), Failure> {
        self.profiles.delete(name).map_err(without_paths)?;
        if self.settings().last_profile.as_deref() == Some(name) {
            self.remember(|s| s.last_profile = None);
        }
        Ok(())
    }

    /// Extra packages of `version` (a channel or a version) for `arch`.
    /// Error: `packages_unavailable` (`cause`: an asset error code).
    pub async fn available_packages(
        &self,
        version: &str,
        arch: &str,
    ) -> Result<PackagesView, Failure> {
        let fail = |e: AssetError| asset_cause("packages_unavailable", &e);
        let spec: VersionSpec = version.parse().map_err(|e| fail(AssetError::from(e)))?;
        let pkg_arch = PkgArch::from_device_arch(arch).map_err(|e| fail(e.into()))?;
        let engine = self.engine();
        let assets = engine.assets();
        let resolved = assets.resolve(&spec).await.map_err(fail)?;
        let packages = assets
            .available_packages(&resolved, pkg_arch)
            .await
            .map_err(fail)?;
        Ok(PackagesView {
            version: resolved.to_string(),
            arch: pkg_arch.as_str().to_string(),
            packages,
        })
    }

    /// The channels with the version each names now (from mikrotik.com, or
    /// the last one known offline), and the list of versions to pick from
    /// ([`ni_assets::Assets::version_list`]; `refresh` fetches it again).
    pub async fn available_versions(&self, refresh: bool) -> VersionsView {
        let engine = self.engine();
        let assets = engine.assets().clone();
        let tasks: Vec<_> = Channel::ALL
            .into_iter()
            .map(|c| {
                let assets = assets.clone();
                tokio::spawn(async move {
                    let spec = VersionSpec::Channel(c);
                    let version = ni_engine::prepare::resolve_or_remembered(&assets, &spec)
                        .await
                        .ok()
                        .map(|(v, _)| v.to_string());
                    ChannelView {
                        channel: c.as_str().to_string(),
                        version,
                    }
                })
            })
            .collect();
        let answer = assets.version_list(VERSION_LIST_MAX_AGE, refresh).await;
        let mut channels = Vec::with_capacity(tasks.len());
        for (task, c) in tasks.into_iter().zip(Channel::ALL) {
            channels.push(task.await.unwrap_or(ChannelView {
                channel: c.as_str().to_string(),
                version: None,
            }));
        }
        VersionsView {
            channels,
            versions: answer.list.versions,
            fetched_unix: answer.list.fetched_unix,
            source: answer.source,
            error: answer.error.map(String::from),
        }
    }

    /// Whether a typed version is published by MikroTik. Errors:
    /// `version_unavailable` (`cause`: `invalid_version`, `version_too_old`
    /// or a network error code).
    pub async fn check_version_exists(&self, version: &str) -> Result<bool, Failure> {
        let fail = |e: AssetError| asset_cause("version_unavailable", &e);
        let v = Version::parse(version).map_err(|e| fail(e.into()))?;
        self.engine()
            .assets()
            .version_exists(&v)
            .await
            .map_err(fail)
    }

    /// Register picked `.npk` files (each checked like a download and its
    /// header read). Errors: `not_a_package` (`name`), `package_failed`
    /// (`name`, `cause`).
    pub async fn register_packages(
        &self,
        paths: &[PathBuf],
    ) -> Result<Vec<PackageFileView>, Failure> {
        let engine = self.engine();
        let assets = engine.assets();
        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !name.to_lowercase().ends_with(".npk") {
                return Err(Failure::new("not_a_package").with("name", name));
            }
            let failed = |e: &AssetError| {
                Failure::new("package_failed")
                    .with("name", &name)
                    .with("cause", e.code())
            };
            let file = assets
                .register_local_file(path)
                .await
                .map_err(|e| failed(&e))?;
            let data = assets
                .local_file(&file.name)
                .await
                .map_err(|e| failed(&e))?;
            let header =
                ni_assets::npk::package_header(&data).map_err(|e| failed(&AssetError::from(e)))?;
            out.push(PackageFileView {
                name: file.name,
                size: file.size,
                package: header.name,
                arch: header.arch,
                version: ni_assets::npk::package_version(&data).map(|v| v.to_string()),
            });
        }
        Ok(out)
    }

    /// Register a picked `.rsc` file. Errors: `not_a_script` (`name`),
    /// `script_failed` (`cause`).
    pub async fn register_script(&self, path: &Path) -> Result<ScriptView, Failure> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !name.to_lowercase().ends_with(".rsc") {
            return Err(Failure::new("not_a_script").with("name", name));
        }
        let file = self
            .engine()
            .assets()
            .register_local_file(path)
            .await
            .map_err(|e| asset_cause("script_failed", &e))?;
        debug_assert_eq!(file.kind, LocalKind::Script);
        Ok(ScriptView {
            name: file.name,
            size: file.size,
        })
    }

    /// The license key in the key file at `path` (the official tool's
    /// `-k`), opened once and read only if it is a regular file (a FIFO
    /// or device never blocks the call; see
    /// [`ni_assets::read_regular_file`]). Errors: `key_file_failed`
    /// (`reason`: `unreadable`, `not_a_file`, or `too_large` past
    /// [`MAX_KEY_FILE`] bytes), `invalid_license_key` (`max`).
    pub fn read_license_key(&self, path: &Path) -> Result<LicenseKeyView, Failure> {
        let failed = |reason: &str| Failure::new("key_file_failed").with("reason", reason);
        let data = ni_assets::read_regular_file(path, MAX_KEY_FILE).map_err(|e| match e {
            ni_assets::ReadFileError::Io(_) => failed("unreadable"),
            ni_assets::ReadFileError::NotAFile => failed("not_a_file"),
            ni_assets::ReadFileError::TooLarge => failed("too_large"),
        })?;
        let key = license_key_from_file(&data)?;
        Ok(LicenseKeyView { key })
    }

    // ------------------------------------------------------------ cache, log

    /// What is in the download cache: quick (sizes, remembered checks), or
    /// with every file's SHA-256 checked (`verify`). Error: `cache_failed`
    /// (`cause`).
    pub async fn cache_inventory(&self, verify: bool) -> Result<CacheView, Failure> {
        let engine = self.engine();
        let assets = engine.assets();
        let inventory = if verify {
            assets.verify_cache().await
        } else {
            assets.cached_inventory().await
        }
        .map_err(|e| asset_cause("cache_failed", &e))?;
        Ok(CacheView {
            location: display_path(&self.paths.cache),
            inventory,
        })
    }

    /// Delete part of the download cache; returns the bytes freed. Allowed
    /// while running, except for a version the engine needs
    /// ([`EngineHandle::versions_in_use`]). Errors: `asset_in_use`
    /// (`version`), `cache_failed` (`cause`: `invalid_item`, `busy` while
    /// it is being downloaded, `io`).
    pub async fn delete_cached(&self, scope: DeleteScope) -> Result<u64, Failure> {
        let engine = self.engine();
        let assets = engine.assets();
        let in_use = engine.versions_in_use().await?;
        if !in_use.is_empty() {
            let targets: Vec<Version> = match &scope {
                DeleteScope::Item { id } => item_version(id).into_iter().collect(),
                DeleteScope::Version { version } => Version::parse(version).into_iter().collect(),
                DeleteScope::All | DeleteScope::Damaged => {
                    let inv = assets
                        .cached_inventory()
                        .await
                        .map_err(|e| asset_cause("cache_failed", &e))?;
                    let damaged_only = scope == DeleteScope::Damaged;
                    inv.versions
                        .iter()
                        .filter(|v| !damaged_only || v.items.iter().any(|i| i.integrity.damaged()))
                        .filter_map(|v| Version::parse(&v.version).ok())
                        .collect()
                }
            };
            if let Some(v) = targets.iter().find(|v| in_use.contains(v)) {
                return Err(Failure::new("asset_in_use").with("version", v));
            }
        }
        assets
            .delete_cached(scope)
            .await
            .map_err(|e| asset_cause("cache_failed", &e))
    }

    /// Write the log text the window collected. Error: `log_io`.
    pub fn write_log(&self, path: &Path, text: &str) -> Result<(), Failure> {
        std::fs::write(path, text).map_err(|e| Failure::new("log_io").with("detail", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs_are_parsed_or_rejected() {
        assert_eq!(
            parse_mac("48:a9:8a:01:02:03").unwrap(),
            Mac([0x48, 0xa9, 0x8a, 1, 2, 3])
        );
        let err = parse_mac("nope").unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.param("mac")),
            ("invalid_mac", Some("nope"))
        );
    }

    #[test]
    fn user_paths_live_under_the_bundle_id() {
        let p = Paths::for_user().unwrap();
        assert!(p
            .cache
            .ends_with("Library/Caches/io.github.sepsig.mac-netinstall"));
        assert!(p.settings.ends_with(
            "Library/Application Support/io.github.sepsig.mac-netinstall/settings.json"
        ));
        assert_eq!(p.profiles.parent(), p.settings.parent());
    }

    #[test]
    fn display_paths_hide_the_home_folder() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(
            display_path(&home.join("Library/Caches/x")),
            "~/Library/Caches/x"
        );
        assert_eq!(display_path(Path::new("/tmp/x")), "/tmp/x");
    }
}
