//! Mac Netinstall's Tauri app: a thin bridge between the web view and
//! `ni-engine`.
//!
//! - [`core::AppCore`]: what every command does (tested without a window).
//! - [`commands`]: the `#[tauri::command]` wrappers.
//! - [`forward`]: engine events to the window.
//! - [`settings`]: `settings.json` in Application Support.
//! - [`codes`]: the code catalog the UI's translations are checked against.

pub mod codes;
pub mod commands;
pub mod core;
pub mod forward;
pub mod locale;
pub mod logging;
#[cfg(target_os = "macos")]
mod macos;
pub mod settings;
pub mod views;

use crate::core::{AppCore, Paths};
use crate::forward::{forward_events, Forwarded, CLOSE_REQUESTED, ENGINE_EVENT, ENGINE_LAGGED};
use ni_assets::HttpConfig;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, WindowEvent};

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

/// What to do when quitting is asked for (window close button, the Quit
/// menu item / Cmd-Q, Dock Quit, logout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitDecision {
    /// Nothing is being prepared or flashed, or quitting was confirmed.
    Now,
    /// Ask the window first (`close-requested` with this many busy
    /// devices); it calls `quit` when the user confirms.
    Ask(usize),
}

/// `confirmed`: the user already said yes; `busy`: devices being prepared
/// or flashed.
pub fn quit_decision(confirmed: bool, busy: usize) -> QuitDecision {
    if confirmed || busy == 0 {
        QuitDecision::Now
    } else {
        QuitDecision::Ask(busy)
    }
}

/// AppKit's `NSTerminateCancel`.
pub const NS_TERMINATE_CANCEL: usize = 0;
/// AppKit's `NSTerminateNow`.
pub const NS_TERMINATE_NOW: usize = 1;

/// The reply to AppKit's `applicationShouldTerminate:` (Dock Quit, logout,
/// any `terminate:`): cancel and `ask` the window while devices are busy.
pub fn terminate_reply(confirmed: bool, busy: usize, ask: impl FnOnce(usize)) -> usize {
    match quit_decision(confirmed, busy) {
        QuitDecision::Now => NS_TERMINATE_NOW,
        QuitDecision::Ask(n) => {
            ask(n);
            NS_TERMINATE_CANCEL
        }
    }
}

/// Run `reply` (the `applicationShouldTerminate:` decision); a panic must
/// not unwind into AppKit: it is logged and the app may quit
/// (`NSTerminateNow`), as it would without the guard.
pub fn terminate_reply_guarded(reply: impl FnOnce() -> usize) -> usize {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(reply)) {
        Ok(r) => r,
        Err(_) => {
            tracing::error!("panic while deciding whether to quit; quitting");
            NS_TERMINATE_NOW
        }
    }
}

/// Id of the app menu's Quit item.
const QUIT_MENU_ID: &str = "quit";
/// How long quitting waits for the engine to stop (or a snapshot).
const QUIT_ENGINE_WAIT: Duration = Duration::from_secs(3);

/// Devices being prepared or flashed (0 if the engine does not answer).
async fn busy_devices(app: &AppHandle) -> usize {
    let core = app.state::<AppCore>();
    match tokio::time::timeout(QUIT_ENGINE_WAIT, core.snapshot()).await {
        Ok(Ok(s)) => s.devices.iter().filter(|d| d.state.is_active()).count(),
        _ => 0,
    }
}

/// Stop the engine (cancelling sessions, releasing the ports), bounded.
async fn stop_engine(app: &AppHandle) {
    let core = app.state::<AppCore>();
    match tokio::time::timeout(QUIT_ENGINE_WAIT, core.stop()).await {
        Ok(Ok(())) => tracing::info!("engine stopped for quitting"),
        Ok(Err(f)) if f.code == "not_running" => {}
        Ok(Err(f)) => tracing::warn!("stopping the engine for quitting: {f}"),
        Err(_) => tracing::warn!("the engine did not stop in time"),
    }
}

/// Quit for good: stop the engine first, then exit.
pub(crate) async fn quit_now(app: AppHandle) {
    app.state::<CloseGuard>().allow();
    stop_engine(&app).await;
    app.exit(0);
}

/// Quit at once if no device is being prepared or flashed; otherwise ask
/// the window (`close-requested`), which calls `quit` when confirmed.
async fn request_close(app: AppHandle) {
    let confirmed = app.state::<CloseGuard>().allowed();
    match quit_decision(confirmed, busy_devices(&app).await) {
        QuitDecision::Now => quit_now(app).await,
        QuitDecision::Ask(n) => {
            let _ = app.emit(CLOSE_REQUESTED, n);
        }
    }
}

/// `applicationShouldTerminate:` (see [`macos`]): runs on the main thread,
/// so it blocks briefly on the engine. Stops the engine before agreeing.
#[cfg(target_os = "macos")]
fn should_terminate(app: &AppHandle) -> usize {
    let confirmed = app.state::<CloseGuard>().allowed();
    let busy = tauri::async_runtime::block_on(busy_devices(app));
    let reply = terminate_reply(confirmed, busy, |n| {
        let _ = app.emit(CLOSE_REQUESTED, n);
    });
    if reply == NS_TERMINATE_NOW {
        app.state::<CloseGuard>().allow();
        tauri::async_runtime::block_on(stop_engine(app));
    }
    reply
}

/// The menu bar: the standard app, Edit and Window menus, except that Quit
/// (Cmd-Q) is our own item, so it asks first like the close button (the
/// standard one sends `terminate:` straight to AppKit).
fn app_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let name = app.package_info().name.clone();
    let quit = MenuItemBuilder::with_id(QUIT_MENU_ID, format!("Quit {name}"))
        .accelerator("CmdOrCtrl+Q")
        .build(app)?;
    let app_menu = SubmenuBuilder::new(app, &name)
        .about(None)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .item(&quit)
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .build()?;
    MenuBuilder::new(app)
        .items(&[&app_menu, &edit, &window])
        .build()
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
        .menu(app_menu)
        .on_menu_event(|app, event| {
            if event.id() == QUIT_MENU_ID {
                tauri::async_runtime::spawn(request_close(app.clone()));
            }
        })
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
            // Without the guard the app still works; only Dock Quit and
            // logout then skip the confirmation (the close button and the
            // Quit menu item still ask).
            #[cfg(target_os = "macos")]
            if let Err(e) = macos::guard_termination(app.handle().clone(), should_terminate) {
                tracing::error!("termination guard not installed: {e}");
            }
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
            commands::check_interface,
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
            commands::pick_license_key,
            commands::get_settings,
            commands::set_settings,
            commands::cache_inventory,
            commands::delete_cached,
            commands::export_log,
            commands::preferred_languages,
            commands::quit,
        ])
        .build(tauri::generate_context!())
        .expect("cannot build the Tauri app");
    app.run(|app, event| {
        // The last window closing or `exit` from elsewhere: same rule as
        // the close button. (Cmd-Q is the menu item above; Dock Quit and
        // logout come through `applicationShouldTerminate:`, see `macos`.)
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
    fn busy_devices_make_quitting_ask_first() {
        assert_eq!(quit_decision(false, 0), QuitDecision::Now);
        assert_eq!(quit_decision(false, 2), QuitDecision::Ask(2));
        assert_eq!(quit_decision(true, 2), QuitDecision::Now, "confirmed");
    }

    #[test]
    fn terminate_is_cancelled_and_the_window_asked_while_busy() {
        let mut asked = None;
        let reply = terminate_reply(false, 3, |n| asked = Some(n));
        assert_eq!((reply, asked), (NS_TERMINATE_CANCEL, Some(3)));
        let mut asked = None;
        let reply = terminate_reply(false, 0, |n| asked = Some(n));
        assert_eq!((reply, asked), (NS_TERMINATE_NOW, None));
        let mut asked = None;
        let reply = terminate_reply(true, 3, |n| asked = Some(n));
        assert_eq!((reply, asked), (NS_TERMINATE_NOW, None), "confirmed");
        // AppKit's NSApplicationTerminateReply values.
        assert_eq!((NS_TERMINATE_CANCEL, NS_TERMINATE_NOW), (0, 1));
    }

    #[test]
    fn a_panic_while_asked_to_terminate_lets_the_app_quit() {
        assert_eq!(
            terminate_reply_guarded(|| NS_TERMINATE_CANCEL),
            NS_TERMINATE_CANCEL
        );
        let reply = terminate_reply_guarded(|| panic!("boom"));
        assert_eq!(reply, NS_TERMINATE_NOW);
    }

    #[test]
    fn the_close_guard_starts_closed() {
        let g = CloseGuard::default();
        assert!(!g.allowed());
        g.allow();
        assert!(g.allowed());
    }
}
