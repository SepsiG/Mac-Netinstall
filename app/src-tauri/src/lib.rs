//! Mac Netinstall's Tauri app: a thin bridge between the web view and
//! `ni-engine`.
//!
//! - [`core::AppCore`]: what every command does (tested without a window).
//! - [`commands`]: the `#[tauri::command]` wrappers.
//! - [`forward`]: engine events to the window.
//! - [`settings`]: `settings.json` in Application Support.

pub mod commands;
pub mod core;
pub mod forward;
pub mod locale;
pub mod logging;
pub mod settings;
pub mod views;

use crate::core::{AppCore, Paths};
use crate::forward::{forward_events, Forwarded, CLOSE_REQUESTED, ENGINE_EVENT, ENGINE_LAGGED};
use ni_assets::HttpConfig;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};

/// Set once quitting is confirmed (or nothing is being flashed), so the
/// close and exit handlers let it through.
#[derive(Debug, Default)]
pub struct CloseGuard(AtomicBool);

impl CloseGuard {
    pub fn allow(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn allowed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Quit at once if no device is being prepared or flashed; otherwise ask
/// the window (`close-requested`), which calls `quit` when confirmed.
async fn request_close(app: AppHandle) {
    let busy = match app.state::<AppCore>().snapshot().await {
        Ok(s) => s.devices.iter().filter(|d| d.state.is_active()).count(),
        Err(_) => 0,
    };
    if busy == 0 {
        app.state::<CloseGuard>().allow();
        app.exit(0);
    } else {
        let _ = app.emit(CLOSE_REQUESTED, busy);
    }
}

/// Run the app (blocks until it quits).
pub fn run() {
    let filter = std::env::var("NI_LOG").unwrap_or_else(|_| "info".into());
    let _log = logging::default_log_dir().and_then(|dir| logging::init(&dir, &filter).ok());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "Mac Netinstall started"
    );
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let paths = Paths::for_user()?;
            let core = tauri::async_runtime::block_on(async {
                AppCore::new(paths, HttpConfig::default())
            })?;
            let engines = core.watch_engine();
            app.manage(core);
            app.manage(CloseGuard::default());
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(forward_events(engines, move |f| {
                let _ = match f {
                    Forwarded::Event(e) => handle.emit(ENGINE_EVENT, e),
                    Forwarded::Lagged(n) => handle.emit(ENGINE_LAGGED, n),
                };
            }));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if !app.state::<CloseGuard>().allowed() {
                    api.prevent_close();
                    tauri::async_runtime::spawn(request_close(app.clone()));
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_interfaces,
            commands::preflight,
            commands::start,
            commands::stop,
            commands::flash,
            commands::retry,
            commands::cancel,
            commands::forget,
            commands::set_auto_repeat,
            commands::set_active_profile,
            commands::set_only_mac,
            commands::set_max_parallel,
            commands::set_cooldown,
            commands::snapshot,
            commands::list_profiles,
            commands::save_profile,
            commands::delete_profile,
            commands::available_packages,
            commands::pick_script,
            commands::get_settings,
            commands::set_settings,
            commands::cache_info,
            commands::clear_cache,
            commands::export_log,
            commands::preferred_languages,
            commands::quit,
        ])
        .build(tauri::generate_context!())
        .expect("cannot build the Tauri app");
    app.run(|app, event| {
        // Cmd-Q and the menu's Quit: same rule as closing the window.
        if let RunEvent::ExitRequested { api, .. } = event {
            if !app.state::<CloseGuard>().allowed() {
                api.prevent_exit();
                tauri::async_runtime::spawn(request_close(app.clone()));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_close_guard_starts_closed() {
        let g = CloseGuard::default();
        assert!(!g.allowed());
        g.allow();
        assert!(g.allowed());
    }
}
