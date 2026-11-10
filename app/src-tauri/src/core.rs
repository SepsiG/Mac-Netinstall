//! Everything the commands do, without Tauri: an [`AppCore`] owns the
//! engine, the profile store and the settings. The `#[tauri::command]`
//! functions in `commands.rs` only unwrap their arguments and call it, so
//! this is tested directly (see `tests/bridge.rs`).

use crate::settings::{Settings, SettingsStore};
use crate::update::{self, RefreshView, UpdateState, UpdateView, CURRENT_VERSION};
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::watch;

/// Largest key file read (MikroTik key files are a few hundred bytes).
pub const MAX_KEY_FILE: u64 = 64 * 1024;
/// Shortest time between two manual update checks (a quicker one is
/// answered from what is known).
pub const MANUAL_CHECK_COOLDOWN: Duration = Duration::from_secs(60);
/// "What's new?" and "Download" ask GitHub again unless they did less than
/// this ago (a double click is one request).
pub const REFRESH_DEDUPE: Duration = Duration::from_secs(10);
/// Partial update downloads older than this are leftovers (removed at
/// start).
const STALE_UPDATE_PART: Duration = Duration::from_secs(60 * 60);

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
    /// What the last update check found (`update-check.json` next to the
    /// settings).
    pub update_state: PathBuf,
    /// Where a new version's DMG is downloaded (`~/Downloads`).
    pub downloads: PathBuf,
    /// Where it is staged and checked first (`<cache>/updates`).
    pub update_staging: PathBuf,
}

impl Paths {
    /// The standard locations; `no_home` without `HOME`.
    pub fn for_user() -> Result<Paths, Failure> {
        let (Some(cache), Some(profiles)) = (default_cache_root(), default_profiles_root()) else {
            return Err(Failure::new("no_home"));
        };
        let support = profiles.parent().map(Path::to_path_buf).unwrap_or_default();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| Failure::new("no_home"))?;
        Ok(Paths {
            update_staging: cache.join("updates"),
            cache,
            profiles,
            settings: support.join("settings.json"),
            update_state: support.join("update-check.json"),
            downloads: home.join("Downloads"),
        })
    }

    /// Everything under `root` (tests).
    pub fn under(root: &Path) -> Paths {
        Paths {
            cache: root.join("cache"),
            profiles: root.join("profiles"),
            settings: root.join("settings.json"),
            update_state: root.join("update-check.json"),
            downloads: root.join("downloads"),
            update_staging: root.join("cache").join("updates"),
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
    /// Where updates are looked for (GitHub; tests use a local server).
    update_cfg: ni_update::UpdateConfig,
    /// What the last update check found.
    update_state: Mutex<UpdateState>,
    /// A new version is being downloaded.
    update_downloading: AtomicBool,
    /// When the last manual check asked GitHub.
    last_manual_check: Mutex<Option<std::time::Instant>>,
    manual_check_cooldown: Duration,
    /// The last verified download, to open once no device is busy.
    downloaded_update: Mutex<Option<PathBuf>>,
    /// When "What's new?" / "Download" last asked GitHub, and whether that
    /// failed; held while asking, so concurrent asks wait for one answer.
    last_refresh: tokio::sync::Mutex<Option<(std::time::Instant, bool)>>,
}

/// Clears the "downloading" flag however the download ends.
struct DownloadingGuard<'a>(&'a AtomicBool);

impl Drop for DownloadingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
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
        let mut update_state = UpdateState::load(&paths.update_state);
        update_state.drop_not_newer(CURRENT_VERSION);
        let swept = ni_update::sweep_partials(&paths.update_staging, STALE_UPDATE_PART);
        if swept > 0 {
            tracing::info!(swept, "removed unfinished update downloads");
        }
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
            update_cfg: ni_update::UpdateConfig::default(),
            update_state: Mutex::new(update_state),
            update_downloading: AtomicBool::new(false),
            last_manual_check: Mutex::new(None),
            manual_check_cooldown: MANUAL_CHECK_COOLDOWN,
            downloaded_update: Mutex::new(None),
            last_refresh: tokio::sync::Mutex::new(None),
        })
    }

    /// Another cooldown between manual update checks (tests).
    pub fn with_manual_check_cooldown(mut self, cooldown: Duration) -> Self {
        self.manual_check_cooldown = cooldown;
        self
    }

    /// Look for updates at `cfg` instead of GitHub (tests).
    pub fn with_update_config(mut self, cfg: ni_update::UpdateConfig) -> Self {
        self.update_cfg = cfg;
        self
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
    /// (`keep_downloads`) or the engine is running (it may need them; an
    /// engine that does not answer counts as running). Registered local
    /// files and the version lists stay. The cache is shared with `ni-cli`:
    /// this also deletes what it downloaded, except versions another
    /// process is using right now (skipped and logged). Best-effort:
    /// returns whether deleting was attempted.
    pub async fn purge_downloads_if_wanted(&self) -> bool {
        if self.settings().keep_downloads {
            return false;
        }
        let engine = self.engine();
        match engine.snapshot().await {
            Ok(s) if !s.running => {}
            Ok(_) => return false,
            Err(f) => {
                tracing::warn!("not deleting downloads: the engine does not answer ({f})");
                return false;
            }
        }
        match engine.assets().clear_downloads().await {
            Ok(skipped) if skipped.is_empty() => {
                tracing::info!("downloaded RouterOS files deleted")
            }
            Ok(skipped) => tracing::info!(
                ?skipped,
                "downloaded RouterOS files deleted, except versions in use by another process"
            ),
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

    /// Register picked `.npk` files. Each is read once and checked first
    /// (complete NPK, readable header): a malformed file is never
    /// registered. A registered file of the same name (names ignore case)
    /// with other contents is not overwritten. Errors: `not_a_package`
    /// (`name`), `package_failed` (`name`, `cause`), `package_exists`
    /// (`name`).
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
            let max = self.http.max_download_bytes;
            let read_path = path.clone();
            let data =
                tokio::task::spawn_blocking(move || ni_assets::read_regular_file(&read_path, max))
                    .await
                    .map_err(|e| {
                        Failure::new("package_failed")
                            .with("name", &name)
                            .with("detail", e)
                    })?
                    .map_err(|e| {
                        let cause = match e {
                            ni_assets::ReadFileError::Io(_) => "io",
                            ni_assets::ReadFileError::NotAFile => "local_not_file",
                            ni_assets::ReadFileError::TooLarge => "local_too_large",
                        };
                        Failure::new("package_failed")
                            .with("name", &name)
                            .with("cause", cause)
                    })?;
            ni_assets::npk::check_npk(&name, &data).map_err(|e| failed(&e.into()))?;
            let header = ni_assets::npk::package_header(&data).map_err(|e| failed(&e.into()))?;
            let key = name.to_lowercase();
            if let Ok(existing) = assets.local_file(&key).await {
                if existing.as_ref() != data.as_slice() {
                    return Err(Failure::new("package_exists").with("name", key));
                }
            }
            let file = assets
                .register_local_file(path)
                .await
                .map_err(|e| failed(&e))?;
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

    // ------------------------------------------------------------- updates

    /// What the last update check found, for the running version.
    pub fn update_status(&self) -> UpdateView {
        let s = self.settings();
        update::view(
            &lock(&self.update_state),
            CURRENT_VERSION,
            s.check_updates,
            s.skipped_update_version.as_deref(),
        )
    }

    fn updater(&self) -> Result<ni_update::Updater, ni_update::UpdateError> {
        ni_update::Updater::new(self.update_cfg.clone())
    }

    /// Ask GitHub for the latest release. Automatic checks (`manual` false)
    /// ask only if turned on and at most once a day
    /// ([`ni_update::check_due`]), and fail quietly (the last known state
    /// is returned). A manual check asks unless another one did less than
    /// [`MANUAL_CHECK_COOLDOWN`] ago: then the freshest known state is
    /// returned (what the last check or [`AppCore::refresh_update`]
    /// found). Error (manual only):
    /// `update_check_failed` (`problem`: an [`ni_update::PROBLEMS`] code).
    pub async fn check_for_updates(&self, manual: bool) -> Result<UpdateView, Failure> {
        let last = lock(&self.update_state).last_check_unix;
        let now = update::now_unix();
        if !manual && (!self.settings().check_updates || !ni_update::check_due(last, now)) {
            return Ok(self.update_status());
        }
        if manual {
            let mut last = lock(&self.last_manual_check);
            if last.is_some_and(|t| t.elapsed() < self.manual_check_cooldown) {
                return Ok(self.update_status());
            }
            *last = Some(std::time::Instant::now());
        }
        let result = match self.updater() {
            Ok(u) => u.latest().await,
            Err(e) => Err(e),
        };
        let found = match result {
            Ok(latest) => Some(latest),
            Err(e) => {
                tracing::info!(problem = e.code(), "update check failed: {e}");
                if let ni_update::UpdateError::RateLimited { .. } = e {
                    // Not again today (automatically).
                    let mut st = lock(&self.update_state);
                    st.last_check_unix = Some(now);
                    st.save(&self.paths.update_state);
                }
                if manual {
                    return Err(Failure::new("update_check_failed").with("problem", e.code()));
                }
                None
            }
        };
        if let Some(latest) = found {
            tracing::info!(
                latest = latest.as_ref().map(|r| r.version.as_str()),
                "checked for updates"
            );
            let mut st = lock(&self.update_state);
            st.last_check_unix = Some(now);
            st.last_success_unix = Some(now);
            st.latest = latest;
            st.save(&self.paths.update_state);
        }
        Ok(self.update_status())
    }

    /// Ask GitHub for the latest release again, past the once-a-day
    /// throttle ("What's new?" and "Download" always use the newest
    /// published release); at most once per [`REFRESH_DEDUPE`]. A newer
    /// answer replaces what is remembered. If GitHub cannot be asked, the
    /// last known release is kept (`stale`).
    pub async fn refresh_update(&self) -> Result<RefreshView, Failure> {
        let mut last = self.last_refresh.lock().await;
        if let Some((at, stale)) = *last {
            if at.elapsed() < REFRESH_DEDUPE {
                return Ok(RefreshView {
                    update: self.update_status(),
                    stale,
                });
            }
        }
        let result = match self.updater() {
            Ok(u) => u.latest().await,
            Err(e) => Err(e),
        };
        let stale = match result {
            Ok(latest) => {
                let now = update::now_unix();
                let mut st = lock(&self.update_state);
                st.last_check_unix = Some(now);
                st.last_success_unix = Some(now);
                st.latest = latest;
                st.drop_not_newer(CURRENT_VERSION);
                st.save(&self.paths.update_state);
                false
            }
            Err(e) => {
                tracing::info!(
                    problem = e.code(),
                    "update refresh failed, using the last known release: {e}"
                );
                true
            }
        };
        *last = Some((std::time::Instant::now(), stale));
        Ok(RefreshView {
            update: self.update_status(),
            stale,
        })
    }

    /// Do not announce `version` (`X.Y.Z`) again, only newer ones. Error:
    /// `invalid_setting` (`field`: `skipped_update_version`).
    pub fn skip_update(&self, version: &str) -> Result<UpdateView, Failure> {
        let next = Settings {
            skipped_update_version: Some(version.to_string()),
            ..self.settings()
        };
        next.validate()?;
        self.remember(|s| s.skipped_update_version = Some(version.to_string()));
        Ok(self.update_status())
    }

    /// Announce the skipped release again.
    pub fn clear_skipped_update(&self) -> Result<UpdateView, Failure> {
        self.remember(|s| s.skipped_update_version = None);
        Ok(self.update_status())
    }

    /// Ask GitHub again ([`AppCore::refresh_update`]), then download the
    /// newest known release's DMG (only if it is newer than the running
    /// version) into the Downloads folder and
    /// check it against its published SHA-256 (`progress`: bytes done,
    /// total). Returns the DMG's path. Errors: `update_not_available` (no
    /// newer release known), `update_busy` (already downloading),
    /// `update_download_failed` (`problem`; the partial file is deleted).
    pub async fn download_update(
        &self,
        progress: &(dyn Fn(u64, Option<u64>) + Sync),
    ) -> Result<PathBuf, Failure> {
        self.refresh_update().await?;
        let release = lock(&self.update_state)
            .latest
            .clone()
            .filter(|r| ni_update::is_newer(&r.version, CURRENT_VERSION))
            .ok_or_else(|| Failure::new("update_not_available"))?;
        if self.update_downloading.swap(true, Ordering::SeqCst) {
            return Err(Failure::new("update_busy"));
        }
        let _guard = DownloadingGuard(&self.update_downloading);
        let result = async {
            std::fs::create_dir_all(&self.paths.downloads)
                .map_err(|e| ni_update::UpdateError::Io(e.to_string()))?;
            self.updater()?
                .download(
                    &release,
                    &self.paths.update_staging,
                    &self.paths.downloads,
                    progress,
                )
                .await
        }
        .await;
        result
            .inspect(|p| {
                tracing::info!(path = %p.display(), "update downloaded and verified");
                *lock(&self.downloaded_update) = Some(p.clone());
            })
            .map_err(|e| {
                tracing::warn!(problem = e.code(), "update download failed: {e}");
                Failure::new("update_download_failed").with("problem", e.code())
            })
    }

    /// The verified DMG to open now. Errors: `update_not_available` (none
    /// downloaded, or it is gone), `update_open_busy` (devices are being
    /// prepared or flashed: opened later).
    pub async fn downloaded_update(&self) -> Result<PathBuf, Failure> {
        let path = lock(&self.downloaded_update)
            .clone()
            .filter(|p| p.is_file())
            .ok_or_else(|| Failure::new("update_not_available"))?;
        let snap = self.snapshot().await?;
        if snap.devices.iter().any(|d| d.state.is_active()) {
            return Err(Failure::new("update_open_busy"));
        }
        Ok(path)
    }

    /// The release page to open: the latest release's, if it is one of the
    /// project's ([`ni_update::release_page_allowed`]), else the releases'.
    pub fn release_page_url(&self) -> String {
        lock(&self.update_state)
            .latest
            .as_ref()
            .map(|r| r.page_url.clone())
            .filter(|u| ni_update::release_page_allowed(u))
            .unwrap_or_else(update::fallback_page)
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
