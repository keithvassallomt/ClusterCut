//! Global-shortcut status for the settings UI.

#[derive(serde::Serialize)]
pub(crate) struct ShortcutStatus {
    /// "native": the Tauri plugin (X11, Windows, macOS). "portal": Wayland,
    /// through the desktop's GlobalShortcuts portal.
    backend: &'static str,
    #[cfg(target_os = "linux")]
    portal: Option<crate::portal_shortcuts::PortalStatus>,
    #[cfg(target_os = "linux")]
    hyprland: Option<crate::hyprland::HyprlandStatus>,
    /// How the one-step shortcuts press Copy/Cut/Paste here.
    keys: crate::keys::KeyStatus,
}

#[tauri::command]
pub(crate) async fn get_shortcut_status(app_handle: tauri::AppHandle) -> ShortcutStatus {
    let keys = crate::keys::status(&app_handle).await;
    #[cfg(target_os = "linux")]
    if crate::clipboard::is_wayland() {
        return ShortcutStatus {
            backend: "portal",
            portal: Some(crate::portal_shortcuts::status().await),
            hyprland: crate::hyprland::status(),
            keys,
        };
    }
    ShortcutStatus {
        backend: "native",
        #[cfg(target_os = "linux")]
        portal: None,
        #[cfg(target_os = "linux")]
        hyprland: None,
        keys,
    }
}

/// Ask for the desktop's permission to press keys now (KDE), not on first use.
#[tauri::command]
pub(crate) async fn allow_key_presses(app_handle: tauri::AppHandle) -> Result<(), String> {
    crate::keys::request_permission(&app_handle).await.map_err(|e| match e {
        crate::keys::KeysError::Declined => "Not allowed. Try again whenever you like.".to_string(),
        e => format!("Couldn't ask: {e}"),
    })
}

/// Open the desktop's own shortcut settings for ClusterCut (Wayland portal v2+).
#[tauri::command]
pub(crate) async fn configure_portal_shortcuts(app_handle: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return crate::portal_shortcuts::configure(&app_handle).await;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app_handle;
        Err("Not supported on this platform".to_string())
    }
}

/// Ask the desktop again after the user declined its shortcuts dialog.
#[tauri::command]
pub(crate) fn retry_portal_shortcuts(app_handle: tauri::AppHandle) {
    #[cfg(target_os = "linux")]
    crate::portal_shortcuts::retry(app_handle);
    #[cfg(not(target_os = "linux"))]
    let _ = app_handle;
}
