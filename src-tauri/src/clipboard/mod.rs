pub mod common;
pub mod history_store;
pub mod preview;
mod plugin;
mod rich;

use crate::protocol::{ClipboardBlob, ClipboardFormat};

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
pub mod dbus_clipboard;
#[cfg(target_os = "linux")]
mod watcher;

use crate::state::AppState;
use crate::transport::Transport;
use tauri::AppHandle;

#[cfg(target_os = "linux")]
use std::sync::{OnceLock, RwLock};

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
static ACTIVE_BACKEND: OnceLock<RwLock<ClipboardBackend>> = OnceLock::new();

#[cfg(target_os = "linux")]
fn backend_cell() -> &'static RwLock<ClipboardBackend> {
    ACTIVE_BACKEND.get_or_init(|| {
        RwLock::new(if is_wayland() {
            ClipboardBackend::Degraded
        } else {
            ClipboardBackend::Plugin
        })
    })
}

#[cfg(target_os = "linux")]
pub fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v == "wayland")
            .unwrap_or(false)
}

/// True on a GNOME session. The extension bridge — and every user-facing prompt
/// about it — only applies here; on any other compositor there is no extension
/// to install, so clipboard messaging must not mention one.
#[cfg(target_os = "linux")]
pub fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .map(|v| v.to_ascii_uppercase().contains("GNOME"))
        .unwrap_or(false)
}

/// True when running inside a Flatpak sandbox.
#[cfg(target_os = "linux")]
pub fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Human-readable desktop name for diagnostics, e.g. "Hyprland".
#[cfg(target_os = "linux")]
pub fn desktop_name() -> String {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".to_string())
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
    } else if is_gnome() {
        tracing::warn!(
            "GNOME Wayland session with no clipboard backend. Clipboard monitoring \
             will not work until the ClusterCut extension is installed and enabled. \
             Will watch for the extension to become available."
        );
        ClipboardBackend::Degraded
    } else if is_flatpak() {
        // Compositors implementing wp_security_context_v1 (Hyprland, Sway, KDE)
        // filter privileged protocols out of sandboxed clients, and Flatpak 1.16+
        // tags every sandbox with one. Both data-control variants are among the
        // globals withheld, so the sandboxed build cannot see a clipboard here even
        // though the native build can. There is no extension to fall back to off
        // GNOME, so this is terminal for the Flatpak on this desktop.
        tracing::warn!(
            "Wayland session ({}) exposes no data-control protocol to the Flatpak \
             sandbox — privileged Wayland globals are filtered for sandboxed clients. \
             Clipboard monitoring cannot work in Flatpak on this desktop; the native \
             package (deb/rpm/binary) is not sandboxed and does have access.",
            desktop_name()
        );
        ClipboardBackend::Degraded
    } else {
        tracing::warn!(
            "Wayland session ({}) exposes neither wlr-data-control nor \
             ext-data-control. Clipboard monitoring will not work.",
            desktop_name()
        );
        ClipboardBackend::Degraded
    };

    *backend_cell().write().unwrap() = backend;
    backend
}

#[cfg(target_os = "linux")]
pub fn get_backend() -> ClipboardBackend {
    *backend_cell().read().unwrap()
}

/// Update the active backend. Used by the watcher to transition between
/// Degraded and GnomeExtension when the extension is enabled/disabled at runtime.
#[cfg(target_os = "linux")]
pub(crate) fn set_backend(backend: ClipboardBackend) {
    *backend_cell().write().unwrap() = backend;
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

/// Write plain text plus alternate format representations (text/html, text/rtf,
/// …) onto the local clipboard so the destination app can pick whichever
/// format it understands best. Wayland wlroots, GNOME-extension, and the
/// Plugin backend (Windows / macOS) carry the rich formats end-to-end. X11
/// (also Plugin) is intentionally out of scope — its set_clipboard_rich falls
/// back to plain text via tauri-plugin-clipboard.
pub fn set_clipboard_rich(app: &AppHandle, text: String, formats: Vec<ClipboardFormat>) {
    #[cfg(not(target_os = "linux"))]
    {
        plugin::set_clipboard_rich(app, text, formats);
    }

    #[cfg(target_os = "linux")]
    {
        match get_backend() {
            ClipboardBackend::WlrDataControl => wayland::set_clipboard_rich(app, text, formats),
            ClipboardBackend::Plugin => {
                // On X11 the rich module returns Err and the plugin path
                // gracefully falls back to writing plain text.
                plugin::set_clipboard_rich(app, text, formats);
            }
            ClipboardBackend::GnomeExtension => {
                dbus_clipboard::set_clipboard_rich(app, text, formats);
            }
            ClipboardBackend::Degraded => {
                tracing::warn!(
                    "Clipboard rich write attempted in degraded mode — no backend available"
                );
            }
        }
    }
}

/// Place an image blob (typically `image/png`) on the local clipboard so the
/// user can paste it in any app. Wired up across all four backends; the GNOME
/// extension path requires extension v4.0 or newer — older extensions return
/// UnknownMethod and the write fails gracefully.
pub fn set_clipboard_image(app: &AppHandle, blob: ClipboardBlob) {
    #[cfg(not(target_os = "linux"))]
    {
        plugin::set_clipboard_image(app, blob);
    }

    #[cfg(target_os = "linux")]
    {
        match get_backend() {
            ClipboardBackend::Plugin => plugin::set_clipboard_image(app, blob),
            ClipboardBackend::WlrDataControl => wayland::set_clipboard_image(app, blob),
            ClipboardBackend::GnomeExtension => dbus_clipboard::set_clipboard_image(app, blob),
            ClipboardBackend::Degraded => {
                tracing::warn!(
                    "Clipboard image write attempted in degraded mode — no backend available"
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
                Err(format!(
                    "No clipboard backend available on this Wayland session ({})",
                    desktop_name()
                ))
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
                Err(format!(
                    "No clipboard backend available on this Wayland session ({})",
                    desktop_name()
                ))
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
            // For GNOME, the watcher owns the dbus_clipboard monitor lifecycle so it
            // can start/stop as the extension is enabled or disabled at runtime.
            ClipboardBackend::GnomeExtension | ClipboardBackend::Degraded => {
                watcher::start(app_handle, state, transport);
            }
        }
    }
}
