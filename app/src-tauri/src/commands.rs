//! The `#[tauri::command]`s: unwrap the arguments, call [`AppCore`].
//!
//! Arguments use snake_case keys (`invoke("flash", { mac, profile })`).
//! Errors are [`Failure`]s (`{ code, params }`), never localized text.
//! The file dialogs run here, in Rust: the web view never sees a path.

use crate::core::{self, AppCore};
use crate::settings::Settings;
use crate::views::{
    CacheView, InterfaceView, LicenseKeyView, PackagesView, ProfilesView, ScriptView, StartRequest,
};
use ni_assets::DeleteScope;
use ni_engine::{Failure, PreflightReport, Profile, Snapshot, StartInfo};
use tauri::{AppHandle, State};
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
