//! Generates Tauri's context and the permissions of the app's own
//! commands: each command gets an `allow-<command>` permission, and only
//! the ones listed in `capabilities/default.json` can be called from the
//! window.

/// Every `#[tauri::command]` in `src/commands.rs`.
const COMMANDS: &[&str] = &[
    "list_interfaces",
    "preflight",
    "check_interface",
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
    "available_versions",
    "check_version_exists",
    "pick_packages",
    "accept_disclaimer",
    "open_link",
    "pick_script",
    "pick_license_key",
    "get_settings",
    "set_settings",
    "set_dialog_size",
    "set_log_panel_height",
    "set_onboarding_done",
    "request_permissions",
    "open_system_settings",
    "cache_inventory",
    "delete_cached",
    "export_log",
    "preferred_languages",
    "quit",
    "update_status",
    "check_for_updates",
    "skip_update",
    "clear_skipped_update",
    "download_update",
    "open_release_page",
    "open_downloaded_update",
    "refresh_update",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri build script");
}
