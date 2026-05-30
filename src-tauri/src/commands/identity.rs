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

#[tauri::command]
pub(crate) fn set_network_identity(
    name: String,
    pin: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    // Validate?
    *state.network_name.lock().unwrap() = name.clone();
    *state.network_pin.lock().unwrap() = pin.clone();

    crate::storage::save_network_name(&app_handle, &name);
    crate::storage::save_network_pin(&app_handle, &pin);

    // Also likely need to reset keys if we are "provisioning" a new identity?
    // Or do we keep the key?
    // If I type a new name/pin, I am essentially saying "I belong to THIS network now".
    // I need the key for THAT network.
    // If I'm creating it, I generate a key.
    // If I'm joining it (provisioned), I usually need the Key too OR I need to Pair.
    // But "Provisioned" usually means "I set the config manually".
    // The prompt says "Toggle... default behaviour applies (random)... Provisioned... user can enter".
    // It doesn't say "User enters Key".
    // So "Provisioned" here effectively just means "Manual valid Network Name/PIN" instead of "Random Name/PIN".
    // It implies we are STARTING a cluster with this name/pin.
    // So we keep our current Key (or gen a new one).
    // Since we are changing identity, a new Key is safer.
    // But if we just rename the cluster, we might want to keep the key.
    // Actually, if I just want to rename my cluster "My Home", I don't want to break existing peers if I can help it?
    // But existing peers know me by Key? No, they pair with Spake2 using PIN.
    // If I change PIN, they can't pair.
    // If I change Name, they see "My Home" instead of "Fuzzy-Badger".
    // I'll stick to just updating Name/PIN.

    // Re-register mDNS with new name
    let device_id = state.local_device_id.lock().unwrap().clone();

    // Get actual port from transport
    let port = if let Some(transport) = state.transport.lock().unwrap().as_ref() {
        transport.local_addr().map(|a| a.port()).unwrap_or(4654)
    } else {
        4654
    };

    // Discovery usually stores port.
    if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
          let _ = discovery.register(&device_id, &name, port);
    }

    let _ = app_handle.emit("network-update", ());
}

#[tauri::command]
pub(crate) fn regenerate_network_identity(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    let (name, pin) = crate::storage::regenerate_identity(&app_handle);

    *state.network_name.lock().unwrap() = name.clone();
    *state.network_pin.lock().unwrap() = pin.clone();

    let device_id = state.local_device_id.lock().unwrap().clone();

    // Get actual port from transport
    let port = if let Some(transport) = state.transport.lock().unwrap().as_ref() {
        transport.local_addr().map(|a| a.port()).unwrap_or(4654)
    } else {
        4654
    };

    if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
          let _ = discovery.register(&device_id, &name, port);
    }

    let _ = app_handle.emit("network-update", ());
}
