//! Mac Netinstall's Tauri app: a thin bridge between the web view and
//! `ni-engine`.
//!
//! - [`settings`]: `settings.json` in Application Support.

pub mod settings;

/// Run the app (blocks until it quits).
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("cannot run the Tauri app");
}
