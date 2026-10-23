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
}

#[tauri::command]
pub(crate) async fn get_shortcut_status() -> ShortcutStatus {
    #[cfg(target_os = "linux")]
    if crate::clipboard::is_wayland() {
        return ShortcutStatus {
            backend: "portal",
            portal: Some(crate::portal_shortcuts::status().await),
            hyprland: crate::hyprland::status(),
        };
    }
    ShortcutStatus {
        backend: "native",
        #[cfg(target_os = "linux")]
        portal: None,
        #[cfg(target_os = "linux")]
        hyprland: None,
    }
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
