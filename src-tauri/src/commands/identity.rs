//! Device identity and network identity commands.

use crate::state::AppState;
use tauri::{Emitter, State};

#[tauri::command]
pub(crate) fn get_device_id(state: State<'_, AppState>) -> String {
    state.local_device_id.lock().unwrap().clone()
}

#[tauri::command]
pub(crate) fn get_network_name(state: State<'_, AppState>) -> String {
    state.network_name.lock().unwrap().clone()
}

#[tauri::command]
pub(crate) fn get_network_pin(state: State<'_, AppState>) -> String {
    state.network_pin.lock().unwrap().clone()
}

#[tauri::command]
pub(crate) fn get_hostname(state: State<'_, AppState>) -> String {
    let settings = state.settings.lock().unwrap();
    if let Some(custom_name) = &settings.custom_device_name {
        if !custom_name.trim().is_empty() {
             return custom_name.clone();
        }
    }

    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string())
}

/// Apply a local cluster-name change: bump the register version, set origin to
/// this device, persist all three fields, re-register mDNS, and broadcast the
/// new register to connected peers. Shared by provisioned set-name and auto
/// regenerate. Does NOT touch the PIN.
fn apply_local_rename(
    name: &str,
    state: &AppState,
    transport: &crate::transport::Transport,
    app_handle: &tauri::AppHandle,
) {
    let device_id = state.local_device_id.lock().unwrap().clone();
    let new_version = {
        let cur = *state.network_name_version.lock().unwrap();
        crate::cluster_name::next_local_version(cur)
    };

    *state.network_name.lock().unwrap() = name.to_string();
    *state.network_name_version.lock().unwrap() = new_version;
    *state.network_name_origin.lock().unwrap() = device_id.clone();

    crate::storage::save_network_name(app_handle, name);
    crate::storage::save_network_name_version(app_handle, new_version);
    crate::storage::save_network_name_origin(app_handle, &device_id);

    // Re-register mDNS with the new name.
    let port = state
        .transport
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|t| t.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(4654);
    if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
        let _ = discovery.register(&device_id, name, port);
    }

    // Propagate to connected peers.
    crate::net_util::broadcast_cluster_name(
        name, new_version, &device_id, state, transport, None,
    );

    let _ = app_handle.emit("network-update", ());
}

#[tauri::command]
pub(crate) fn set_network_identity(
    name: String,
    pin: String,
    state: State<'_, AppState>,
    transport: State<'_, crate::transport::Transport>,
    app_handle: tauri::AppHandle,
) {
    // PIN stays per-device; persist it as before.
    *state.network_pin.lock().unwrap() = pin.clone();
    crate::storage::save_network_pin(&app_handle, &pin);

    // The name is shared cluster state: bump the register + propagate.
    apply_local_rename(&name, &state, &transport, &app_handle);
}

#[tauri::command]
pub(crate) fn regenerate_network_identity(
    state: State<'_, AppState>,
    transport: State<'_, crate::transport::Transport>,
    app_handle: tauri::AppHandle,
) {
    // This command runs when switching to Auto mode. Generate a fresh random
    // name (persisted + version-bumped + propagated by apply_local_rename) and
    // an EPHEMERAL PIN — auto mode never stores the PIN on disk (issue 4).
    let name = crate::storage::regenerate_network_name(&app_handle);
    let pin = crate::storage::establish_network_pin(&app_handle, "auto");
    *state.network_pin.lock().unwrap() = pin;

    apply_local_rename(&name, &state, &transport, &app_handle);
}
