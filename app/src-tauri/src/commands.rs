//! The `#[tauri::command]`s: unwrap the arguments, call [`AppCore`].
//!
//! Arguments use snake_case keys (`invoke("flash", { mac, profile })`).
//! Errors are [`Failure`]s (`{ code, params }`), never localized text.
//! The file dialogs run here, in Rust: the web view never sees a path.

use crate::core::{self, AppCore};
use crate::permissions::{self, PermissionsView, SettingsPane};
use crate::settings::Settings;
use crate::update::{DownloadedView, RefreshView, UpdateProgress, UpdateView, UPDATE_PROGRESS};
use crate::views::{
    CacheView, InterfaceView, LicenseKeyView, PackageFileView, PackagesView, ProfilesView,
    ScriptView, StartRequest, VersionsView,
};
use ni_assets::DeleteScope;
use ni_engine::{Failure, PreflightReport, Profile, Snapshot, StartInfo};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;

type R<T> = Result<T, Failure>;

#[tauri::command(rename_all = "snake_case")]
pub fn list_interfaces() -> R<Vec<InterfaceView>> {
    core::list_interfaces()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn preflight(core: State<'_, AppCore>, req: StartRequest) -> R<PreflightReport> {
    core.preflight(&req).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn check_interface(core: State<'_, AppCore>, req: StartRequest) -> R<PreflightReport> {
    core.check_interface(&req).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn start(core: State<'_, AppCore>, req: StartRequest) -> R<StartInfo> {
    core.start(&req).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn stop(core: State<'_, AppCore>) -> R<()> {
    core.stop().await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn flash(core: State<'_, AppCore>, mac: String, profile: String) -> R<()> {
    core.flash(&mac, &profile).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn retry(core: State<'_, AppCore>, mac: String) -> R<()> {
    core.retry(&mac).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn cancel(core: State<'_, AppCore>, mac: String) -> R<()> {
    core.cancel(&mac).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn forget(core: State<'_, AppCore>, mac: String) -> R<()> {
    core.forget(&mac).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_auto_repeat(core: State<'_, AppCore>, on: bool) -> R<()> {
    core.set_auto_repeat(on).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_active_profile(core: State<'_, AppCore>, name: Option<String>) -> R<()> {
    core.set_active_profile(name.as_deref()).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_only_mac(core: State<'_, AppCore>, mac: Option<String>) -> R<()> {
    core.set_only_mac(mac.as_deref()).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_max_parallel(core: State<'_, AppCore>, n: usize) -> R<()> {
    core.set_max_parallel(n).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_cooldown(core: State<'_, AppCore>, secs: u64) -> R<()> {
    core.set_cooldown(secs).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn snapshot(core: State<'_, AppCore>) -> R<Snapshot> {
    core.snapshot().await
}

#[tauri::command(rename_all = "snake_case")]
pub fn list_profiles(core: State<'_, AppCore>) -> ProfilesView {
    core.list_profiles()
}

#[tauri::command(rename_all = "snake_case")]
pub fn save_profile(core: State<'_, AppCore>, profile: Profile, previous: Option<String>) -> R<()> {
    core.save_profile(&profile, previous.as_deref())
}

#[tauri::command(rename_all = "snake_case")]
pub fn delete_profile(core: State<'_, AppCore>, name: String) -> R<()> {
    core.delete_profile(&name)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn available_packages(
    core: State<'_, AppCore>,
    version: String,
    arch: String,
) -> R<PackagesView> {
    core.available_packages(&version, &arch).await
}

/// The channels and versions the version picker offers (`refresh`: fetch
/// the list from mikrotik.com again).
#[tauri::command(rename_all = "snake_case")]
pub async fn available_versions(core: State<'_, AppCore>, refresh: bool) -> R<VersionsView> {
    Ok(core.available_versions(refresh).await)
}

/// Whether a typed version is published.
#[tauri::command(rename_all = "snake_case")]
pub async fn check_version_exists(core: State<'_, AppCore>, version: String) -> R<bool> {
    core.check_version_exists(&version).await
}

/// Let the user pick one or more `.npk` files and register them (empty:
/// cancelled). Paths stay here; the window gets names and header facts.
#[tauri::command(rename_all = "snake_case")]
pub async fn pick_packages(app: AppHandle, core: State<'_, AppCore>) -> R<Vec<PackageFileView>> {
    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .add_filter("RouterOS package", &["npk"])
        .pick_files(move |files| {
            let _ = tx.send(files);
        });
    let Some(files) = rx.await.map_err(|_| Failure::new("dialog_failed"))? else {
        return Ok(Vec::new());
    };
    let paths = files
        .into_iter()
        .map(|f| f.into_path())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| Failure::new("dialog_failed").with("detail", e))?;
    core.register_packages(&paths).await
}

/// Accept the current disclaimer (needed before the first start).
#[tauri::command(rename_all = "snake_case")]
pub fn accept_disclaimer(core: State<'_, AppCore>) -> Settings {
    core.accept_disclaimer();
    core.settings()
}

/// The pages the About section links to (constant URLs only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Link {
    Repository,
    Disclaimer,
    License,
}

impl Link {
    pub fn url(self) -> &'static str {
        match self {
            Link::Repository => "https://github.com/SepsiG/Mac-Netinstall",
            Link::Disclaimer => ni_engine::DISCLAIMER_URL,
            Link::License => "https://github.com/SepsiG/Mac-Netinstall/blob/main/LICENSE",
        }
    }
}

/// Open one of [`Link`]'s pages in the default browser.
#[tauri::command(rename_all = "snake_case")]
pub fn open_link(link: Link) -> R<()> {
    let failed = |d: String| Failure::new("open_link_failed").with("detail", d);
    let status = std::process::Command::new("/usr/bin/open")
        .arg(link.url())
        .status()
        .map_err(|e| failed(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(failed(status.to_string()))
    }
}

/// What the last update check found (no network).
#[tauri::command(rename_all = "snake_case")]
pub fn update_status(core: State<'_, AppCore>) -> UpdateView {
    core.update_status()
}

/// Ask GitHub for a new release: `manual` from the About section (always
/// asks, reports failures), otherwise the automatic check at launch (at
/// most once a day, if turned on; quiet).
#[tauri::command(rename_all = "snake_case")]
pub async fn check_for_updates(core: State<'_, AppCore>, manual: bool) -> R<UpdateView> {
    core.check_for_updates(manual).await
}

/// Do not announce this release again (only newer ones).
#[tauri::command(rename_all = "snake_case")]
pub fn skip_update(core: State<'_, AppCore>, version: String) -> R<UpdateView> {
    core.skip_update(&version)
}

/// Announce the skipped release again.
#[tauri::command(rename_all = "snake_case")]
pub fn clear_skipped_update(core: State<'_, AppCore>) -> R<UpdateView> {
    core.clear_skipped_update()
}

/// Ask GitHub for the latest release again ("What's new?", before
/// "Download"): past the daily throttle, at most once per 10 s; `stale`
/// if GitHub could not be asked and the last known release is used.
#[tauri::command(rename_all = "snake_case")]
pub async fn refresh_update(core: State<'_, AppCore>) -> R<RefreshView> {
    core.refresh_update().await
}

/// Download the newest release's DMG (asking GitHub again first), verify its SHA-256 and place it
/// in Downloads (`update-progress` events on the way). It is not opened
/// here: the window asks with `open_downloaded_update` once no device is
/// being prepared or flashed. The app keeps running.
#[tauri::command(rename_all = "snake_case")]
pub async fn download_update(app: AppHandle, core: State<'_, AppCore>) -> R<DownloadedView> {
    let emit = |done, total| {
        let _ = app.emit(UPDATE_PROGRESS, UpdateProgress { done, total });
    };
    let path = core.download_update(&emit).await?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(DownloadedView { file_name })
}

/// `/usr/bin/open <arg>` off the async runtime. Error: `update_open_failed`
/// (`detail`).
async fn open_with_finder(arg: std::ffi::OsString) -> R<()> {
    let failed = |d: String| Failure::new("update_open_failed").with("detail", d);
    let status = tokio::task::spawn_blocking(move || {
        // A constant program; the argument is a path this app created or
        // a checked release page.
        std::process::Command::new("/usr/bin/open")
            .arg(arg)
            .status()
    })
    .await
    .map_err(|e| failed(e.to_string()))?
    .map_err(|e| failed(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(failed(status.to_string()))
    }
}

/// Open the verified DMG (the last download; the window never passes a
/// path). Errors: `update_not_available`, `update_open_busy` (devices are
/// being prepared or flashed), `update_open_failed`.
#[tauri::command(rename_all = "snake_case")]
pub async fn open_downloaded_update(core: State<'_, AppCore>) -> R<()> {
    let path = core.downloaded_update().await?;
    open_with_finder(path.into_os_string()).await
}

/// Open the latest release's page on GitHub (only the project's release
/// pages; the window never passes a URL).
#[tauri::command(rename_all = "snake_case")]
pub async fn open_release_page(core: State<'_, AppCore>) -> R<()> {
    let url = core.release_page_url();
    if !ni_update::release_page_allowed(&url) {
        return Err(Failure::new("update_open_failed").with("detail", "not a release page"));
    }
    open_with_finder(url.into()).await
}

/// Let the user pick a `.rsc` file and register it (`None`: cancelled).
#[tauri::command(rename_all = "snake_case")]
pub async fn pick_script(app: AppHandle, core: State<'_, AppCore>) -> R<Option<ScriptView>> {
    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .add_filter("RouterOS script", &["rsc"])
        .pick_file(move |file| {
            let _ = tx.send(file);
        });
    let Some(file) = rx.await.map_err(|_| Failure::new("dialog_failed"))? else {
        return Ok(None);
    };
    let path = file
        .into_path()
        .map_err(|e| Failure::new("dialog_failed").with("detail", e))?;
    core.register_script(&path).await.map(Some)
}

/// Let the user pick a MikroTik key file and read the license key from it
/// (`None`: cancelled). The path stays here; the key goes to the window,
/// which puts it in the profile.
#[tauri::command(rename_all = "snake_case")]
pub async fn pick_license_key(
    app: AppHandle,
    core: State<'_, AppCore>,
) -> R<Option<LicenseKeyView>> {
    let (tx, rx) = oneshot::channel();
    app.dialog().file().pick_file(move |file| {
        let _ = tx.send(file);
    });
    let Some(file) = rx.await.map_err(|_| Failure::new("dialog_failed"))? else {
        return Ok(None);
    };
    let path = file
        .into_path()
        .map_err(|e| Failure::new("dialog_failed").with("detail", e))?;
    core.read_license_key(&path).map(Some)
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_settings(core: State<'_, AppCore>) -> Settings {
    core.settings()
}

/// Remember a dialog's size (by dialog id), for the next time it opens.
#[tauri::command(rename_all = "snake_case")]
pub fn set_dialog_size(core: State<'_, AppCore>, id: String, width: u32, height: u32) -> R<()> {
    core.set_dialog_size(&id, width, height)
}

/// Remember the log panel's height (CSS px of its lines).
#[tauri::command(rename_all = "snake_case")]
pub fn set_log_panel_height(core: State<'_, AppCore>, height: u32) -> R<()> {
    core.set_log_panel_height(height)
}

/// Remember that the permissions sheet was gone through.
#[tauri::command(rename_all = "snake_case")]
pub fn set_onboarding_done(core: State<'_, AppCore>, done: bool) {
    core.set_onboarding_done(done);
}

/// Make macOS ask for the Local Network and incoming-connection
/// permissions now (if it has not yet), and report what it says.
#[tauri::command(rename_all = "snake_case")]
pub async fn request_permissions() -> PermissionsView {
    permissions::request().await
}

/// Open System Settings at a permission's pane (a pane name, not a URL).
#[tauri::command(rename_all = "snake_case")]
pub fn open_system_settings(pane: SettingsPane) -> R<()> {
    permissions::open_settings(pane)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_settings(core: State<'_, AppCore>, settings: Settings) -> R<Settings> {
    core.set_settings(settings).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn cache_inventory(core: State<'_, AppCore>, verify: bool) -> R<CacheView> {
    core.cache_inventory(verify).await
}

/// `scope`: `{"scope": "item", "id": ...}`, `{"scope": "version",
/// "version": ...}`, `{"scope": "all"}` or `{"scope": "damaged"}`.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_cached(core: State<'_, AppCore>, scope: DeleteScope) -> R<u64> {
    core.delete_cached(scope).await
}

/// Let the user choose where to save `text` (the window's log); `false`
/// if cancelled.
#[tauri::command(rename_all = "snake_case")]
pub async fn export_log(app: AppHandle, core: State<'_, AppCore>, text: String) -> R<bool> {
    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Text", &["txt"])
        .set_file_name("mac-netinstall-log.txt")
        .save_file(move |file| {
            let _ = tx.send(file);
        });
    let Some(file) = rx.await.map_err(|_| Failure::new("dialog_failed"))? else {
        return Ok(false);
    };
    let path = file
        .into_path()
        .map_err(|e| Failure::new("dialog_failed").with("detail", e))?;
    core.write_log(&path, &text)?;
    Ok(true)
}

#[tauri::command(rename_all = "snake_case")]
pub fn preferred_languages() -> Vec<String> {
    crate::locale::preferred_languages()
}

/// Quit now (after the window confirmed it while devices were busy): the
/// engine is stopped first.
#[tauri::command(rename_all = "snake_case")]
pub async fn quit(app: AppHandle) -> R<()> {
    crate::quit_now(app).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_constant_https_urls() {
        for link in [Link::Repository, Link::Disclaimer, Link::License] {
            assert!(link
                .url()
                .starts_with("https://github.com/SepsiG/Mac-Netinstall"));
        }
        let l: Link = serde_json::from_str("\"disclaimer\"").unwrap();
        assert_eq!(l, Link::Disclaimer);
        assert!(serde_json::from_str::<Link>("\"https://evil\"").is_err());
    }
}
