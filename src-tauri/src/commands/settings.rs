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
    // Capture the previous settings so we can detect toggle transitions.
    let prev = state.settings.lock().unwrap().clone();

    // Preserve backend-only fields that the frontend doesn't manage. These are
    // owned elsewhere (autostart plugin; the header-bar `set_pairing_accept`
    // command) and are absent from the frontend `AppSettings` type, so a general
    // settings save must never overwrite them — otherwise a stale SettingsView
    // copy could clobber a value the header toggle just changed.
    settings.flatpak_autostart = prev.flatpak_autostart;
    settings.pairing_accept_enabled = prev.pairing_accept_enabled;
    *state.settings.lock().unwrap() = settings.clone();
    tracing::info!(
        "Saving Settings: auto_send={}, auto_receive={}, configure_firewall={}, mdns_advertising={}",
        settings.auto_send, settings.auto_receive, settings.configure_firewall, settings.mdns_advertising
    );
    crate::storage::save_settings(&app_handle, &settings);
    let _ = app_handle.emit("settings-changed", settings.clone());

    // --- Issue #18: apply mDNS advertising toggle live ---
    if settings.mdns_advertising != prev.mdns_advertising {
        let mut disc_lock = state.discovery.lock().unwrap();
        if let Some(disc) = disc_lock.as_mut() {
            if settings.mdns_advertising {
                let device_id = state.local_device_id.lock().unwrap().clone();
                let network_name = state.network_name.lock().unwrap().clone();
                let port = state
                    .transport
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|t| t.local_addr().ok())
                    .map(|a| a.port())
                    .unwrap_or(4654);
                if let Err(e) = disc.register(&device_id, &network_name, port) {
                    tracing::error!("Failed to re-register mDNS service: {}", e);
                }
            } else {
                disc.unregister();
            }
        }
    }

    // --- Issue #18: apply firewall toggle live (Windows OFF->ON only) ---
    #[cfg(target_os = "windows")]
    {
        if settings.configure_firewall && !prev.configure_firewall {
            std::thread::spawn(|| {
                crate::net_util::configure_windows_firewall();
            });
        }
    }

    #[cfg(desktop)]
    crate::tray::update_tray_menu(&app_handle);

    // Update Shortcuts
    crate::shortcuts::register_shortcuts(&app_handle);
}
