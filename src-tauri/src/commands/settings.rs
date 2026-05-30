//! App settings commands.

use crate::state::AppState;
use crate::storage::AppSettings;
use tauri::{Emitter, State};

#[tauri::command]
pub(crate) fn get_settings(state: State<'_, AppState>) -> AppSettings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
pub(crate) fn save_settings(
    mut settings: AppSettings,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    // Preserve backend-only fields that the frontend doesn't manage
    settings.flatpak_autostart = state.settings.lock().unwrap().flatpak_autostart;
    *state.settings.lock().unwrap() = settings.clone();
    tracing::info!("Saving Settings: auto_send={}, auto_receive={}", settings.auto_send, settings.auto_receive);
    crate::storage::save_settings(&app_handle, &settings);
    let _ = app_handle.emit("settings-changed", settings.clone());

    #[cfg(desktop)]
    crate::tray::update_tray_menu(&app_handle);

    // Update Shortcuts
    crate::register_shortcuts(&app_handle);
    // If auto_receive is now OFF, we might want to do something?
    // If device name changed, we should probably rebroadcast or something,
    // but the next heartbeat or discovery probe will pick it up.
    // Ideally we emit an event if needed.

    // Check if network name changed via Provisioning (this function saves AppSettings, but UI might call separate commands for Network Name/PIN)
    // Wait, the UI for Provisioned Mode will likely update NetworkName/PIN directly?
    // Or do we store them in AppSettings too?
    // The requirement says "Provisioned mode, the user can enter a cluster name and PIN".
    // Those are actually `state.network_name` and `state.network_pin`.
    // `AppSettings` stores the *mode*.
    // So the UI should call `save_network_identity` (new command needed?) or Update existing commands?
    // We already have `load_network_name` but no set command exposed.
    // I will add `set_network_identity` command.
}
