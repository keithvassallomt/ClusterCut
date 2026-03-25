pub mod common;
mod plugin;

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
pub mod dbus_clipboard;

use crate::state::AppState;
use crate::transport::Transport;
use tauri::AppHandle;

#[cfg(target_os = "linux")]
use std::sync::OnceLock;

/// Which clipboard backend is active on Linux.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipboardBackend {
    /// tauri-plugin-clipboard (X11)
    Plugin,
    /// wl-clipboard-rs via wlr-data-control (KDE, Sway, Hyprland)
    WlrDataControl,
    /// GNOME Shell extension D-Bus bridge
    GnomeExtension,
    /// Nothing works — degraded mode
    Degraded,
}

#[cfg(target_os = "linux")]
static ACTIVE_BACKEND: OnceLock<ClipboardBackend> = OnceLock::new();

#[cfg(target_os = "linux")]
pub fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v == "wayland")
            .unwrap_or(false)
}

/// Detect and store the appropriate clipboard backend.
/// Must be called early in app startup, after the tokio runtime is available.
#[cfg(target_os = "linux")]
pub fn detect_backend() -> ClipboardBackend {
    let backend = if !is_wayland() {
        tracing::info!("X11 session detected, using tauri-plugin-clipboard backend");
        ClipboardBackend::Plugin
    } else if dbus_clipboard::is_available() {
        tracing::info!(
            "Wayland + GNOME extension clipboard bridge detected, using D-Bus backend"
        );
        ClipboardBackend::GnomeExtension
    } else if wayland::is_available() {
        tracing::info!(
            "Wayland + wlr-data-control detected, using wl-clipboard-rs backend"
        );
        ClipboardBackend::WlrDataControl
    } else {
        tracing::warn!(
            "Wayland session detected but no clipboard backend available. \
             Clipboard monitoring will not work. \
             On GNOME, install the ClusterCut extension for clipboard support."
        );
        ClipboardBackend::Degraded
    };

    let _ = ACTIVE_BACKEND.set(backend);
    backend
}

#[cfg(target_os = "linux")]
pub fn get_backend() -> ClipboardBackend {
    ACTIVE_BACKEND.get().copied().unwrap_or(ClipboardBackend::Plugin)
}

/// Returns true if the tauri-plugin-clipboard should be initialized.
#[cfg(target_os = "linux")]
pub fn should_init_plugin() -> bool {
    matches!(get_backend(), ClipboardBackend::Plugin)
}

// ── Public API (same signature regardless of platform) ──

pub fn set_clipboard(app: &AppHandle, text: String) {
    #[cfg(not(target_os = "linux"))]
    {
        plugin::set_clipboard(app, text);
    }

    #[cfg(target_os = "linux")]
    {
        match get_backend() {
            ClipboardBackend::Plugin => plugin::set_clipboard(app, text),
            ClipboardBackend::WlrDataControl => wayland::set_clipboard(app, text),
            ClipboardBackend::GnomeExtension => dbus_clipboard::set_clipboard(app, text),
            ClipboardBackend::Degraded => {
                tracing::warn!("Clipboard write attempted in degraded mode — no backend available");
            }
        }
    }
}

pub fn set_clipboard_paths(app: &AppHandle, paths: Vec<String>) {
    #[cfg(not(target_os = "linux"))]
    {
        plugin::set_clipboard_paths(app, paths);
    }

    #[cfg(target_os = "linux")]
    {
        match get_backend() {
            ClipboardBackend::Plugin => plugin::set_clipboard_paths(app, paths),
            ClipboardBackend::WlrDataControl => wayland::set_clipboard_paths(app, paths),
            ClipboardBackend::GnomeExtension => dbus_clipboard::set_clipboard_paths(app, paths),
            ClipboardBackend::Degraded => {
                tracing::warn!(
                    "Clipboard file write attempted in degraded mode — no backend available"
                );
            }
        }
    }
}

/// Read clipboard text directly. Used for manual send shortcut.
pub fn read_text(app: &AppHandle) -> Result<String, String> {
    #[cfg(not(target_os = "linux"))]
    {
        plugin::read_text(app)
    }

    #[cfg(target_os = "linux")]
    {
        match get_backend() {
            ClipboardBackend::Plugin => plugin::read_text(app),
            ClipboardBackend::WlrDataControl => wayland::read_text(app),
            ClipboardBackend::GnomeExtension => dbus_clipboard::read_text(app),
            ClipboardBackend::Degraded => {
                Err("No clipboard backend available (Wayland without extension)".to_string())
            }
        }
    }
}

/// Write clipboard text directly. Used for manual receive shortcut.
pub fn write_text_direct(app: &AppHandle, text: String) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        plugin::write_text_direct(app, text)
    }

    #[cfg(target_os = "linux")]
    {
        match get_backend() {
            ClipboardBackend::Plugin => plugin::write_text_direct(app, text),
            ClipboardBackend::WlrDataControl => wayland::write_text_direct(app, text),
            ClipboardBackend::GnomeExtension => dbus_clipboard::write_text_direct(app, text),
            ClipboardBackend::Degraded => {
                Err("No clipboard backend available (Wayland without extension)".to_string())
            }
        }
    }
}

pub fn start_monitor(app_handle: AppHandle, state: AppState, transport: Transport) {
    #[cfg(not(target_os = "linux"))]
    {
        plugin::start_monitor(app_handle, state, transport);
    }

    #[cfg(target_os = "linux")]
    {
        match get_backend() {
            ClipboardBackend::Plugin => plugin::start_monitor(app_handle, state, transport),
            ClipboardBackend::WlrDataControl => {
                wayland::start_monitor(app_handle, state, transport)
            }
            ClipboardBackend::GnomeExtension => {
                dbus_clipboard::start_monitor(app_handle, state, transport)
            }
            ClipboardBackend::Degraded => {
                tracing::warn!(
                    "Clipboard monitor not started — no backend available in degraded mode"
                );
            }
        }
    }
}
