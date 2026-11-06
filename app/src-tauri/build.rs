//! Generates Tauri's context and the permissions of the app's own
//! commands: each command gets an `allow-<command>` permission, and only
//! the ones listed in `capabilities/default.json` can be called from the
//! window.

/// Every `#[tauri::command]` in `src/commands.rs`.
const COMMANDS: &[&str] = &[
    "list_interfaces",
    "preflight",
    "start",
    "stop",
    "flash",
    "retry",
    "cancel",
    "forget",
    "set_auto_repeat",
    "set_active_profile",
    "set_only_mac",
    "set_max_parallel",
    "set_cooldown",
    "snapshot",
    "list_profiles",
    "save_profile",
    "delete_profile",
    "available_packages",
    "pick_script",
    "get_settings",
    "set_settings",
    "cache_inventory",
    "delete_cached",
    "export_log",
    "preferred_languages",
    "quit",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri build script");
}
