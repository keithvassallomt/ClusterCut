/// Watches for the ClusterCut GNOME Shell extension being enabled or disabled
/// at runtime and reconciles the clipboard backend accordingly.
///
/// Subscribes to `org.gnome.Shell.Extensions::ExtensionStateChanged`. When our
/// extension UUID appears in a signal, re-probes the D-Bus clipboard bridge
/// and transitions between `Degraded` and `GnomeExtension`, starting or
/// stopping the monitor as needed.
use super::{dbus_clipboard, set_backend, ClipboardBackend};
use crate::state::AppState;
use crate::transport::Transport;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::sync::Notify;

const EXTENSION_UUID: &str = "clustercut@keithvassallo.com";
const SHELL_EXT_NAME: &str = "org.gnome.Shell.Extensions";
const SHELL_EXT_PATH: &str = "/org/gnome/Shell/Extensions";
const SHELL_EXT_IFACE: &str = "org.gnome.Shell.Extensions";

// Persisted marker: present whenever we've told the user that clipboard sync is
// currently unavailable (extension missing/disabled). The "extension is active"
// notification fires ONLY as a recovery from that announced-down state, then
// clears the marker — so it shows once after an install/re-login/re-enable and
// NOT on every launch (cold start often starts Degraded before the extension's
// D-Bus is ready, then promotes — which must stay silent).
fn announced_down_marker(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("extension_sync_announced_down"))
}

fn mark_announced_down(app: &AppHandle) {
    if let Some(p) = announced_down_marker(app) {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&p, b"1");
    }
}

fn was_announced_down(app: &AppHandle) -> bool {
    announced_down_marker(app).map(|p| p.exists()).unwrap_or(false)
}

fn clear_announced_down(app: &AppHandle) {
    if let Some(p) = announced_down_marker(app) {
        let _ = std::fs::remove_file(p);
    }
}

pub fn start(app_handle: AppHandle, state: AppState, transport: Transport) {
    tauri::async_runtime::spawn(async move {
        let mut monitor_cancel: Option<Arc<Notify>> = None;

        // Start the monitor immediately if the extension is already live.
        if super::get_backend() == ClipboardBackend::GnomeExtension {
            monitor_cancel = Some(dbus_clipboard::start_monitor(
                app_handle.clone(),
                state.clone(),
                transport.clone(),
            ));
        }

        let conn = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    "Clipboard watcher: failed to connect to D-Bus ({}). \
                     Extension state changes will not be detected until app restart.",
                    e
                );
                return;
            }
        };

        let proxy = match zbus::Proxy::new(&conn, SHELL_EXT_NAME, SHELL_EXT_PATH, SHELL_EXT_IFACE)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(
                    "Clipboard watcher: failed to create Shell Extensions proxy ({}). \
                     Extension state changes will not be detected until app restart.",
                    e
                );
                return;
            }
        };

        let mut stream = match proxy.receive_signal("ExtensionStateChanged").await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    "Clipboard watcher: failed to subscribe to ExtensionStateChanged ({}).",
                    e
                );
                return;
            }
        };

        use futures::StreamExt;

        tracing::info!(
            "Clipboard watcher active — will reconcile backend when extension {} changes state",
            EXTENSION_UUID
        );

        // Close the race between the initial detect_backend() (called very early in
        // run()) and the signal subscription above: the extension may have become
        // available in that window, and ExtensionStateChanged only fires on
        // transitions, not on current state. Do one reconcile pass now.
        reconcile(&app_handle, &state, &transport, &mut monitor_cancel).await;

        // If after the cold-start reconcile we're still Degraded, the extension
        // isn't installed, isn't enabled, or is an incompatible version. Surface
        // that to the user so they don't have to dig through logs to understand
        // why clipboard sync isn't working. reconcile() only fires notifications
        // on transitions, so this covers the transition-less cold-start case.
        if super::get_backend() == ClipboardBackend::Degraded {
            crate::send_notification(
                &app_handle,
                "Clipboard sync needs the ClusterCut extension",
                "Install or enable the ClusterCut GNOME extension to start syncing. \
                 If you already have it installed, make sure it is up to date.",
                true,
                None,
                "history",
                crate::NotificationPayload::None,
            );
            // Remember we told the user sync is down, so the eventual
            // "now active" recovery notification fires exactly once.
            mark_announced_down(&app_handle);
        }

        loop {
            if state.is_shutdown() {
                break;
            }

            match stream.next().await {
                Some(msg) => {
                    // Signal signature: (s uuid, a{sv} state). We only care that our
                    // extension's UUID appears; the actual transition is determined by
                    // re-probing the bridge.
                    let uuid = msg
                        .body()
                        .deserialize::<(String, std::collections::HashMap<String, zbus::zvariant::Value>)>()
                        .ok()
                        .map(|(u, _)| u);
                    if uuid.as_deref() != Some(EXTENSION_UUID) {
                        continue;
                    }

                    reconcile(&app_handle, &state, &transport, &mut monitor_cancel).await;
                }
                None => {
                    tracing::warn!("Clipboard watcher: ExtensionStateChanged stream ended");
                    break;
                }
            }
        }

        if let Some(cancel) = monitor_cancel.take() {
            cancel.notify_one();
        }
    });
}

async fn reconcile(
    app_handle: &AppHandle,
    state: &AppState,
    transport: &Transport,
    monitor_cancel: &mut Option<Arc<Notify>>,
) {
    let available = dbus_clipboard::is_available_async().await;
    let current = super::get_backend();

    match (current, available) {
        (ClipboardBackend::Degraded, true) => {
            tracing::info!(
                "ClusterCut extension is now available — promoting clipboard backend to GnomeExtension"
            );
            set_backend(ClipboardBackend::GnomeExtension);
            *monitor_cancel = Some(dbus_clipboard::start_monitor(
                app_handle.clone(),
                state.clone(),
                transport.clone(),
            ));
            // Only confirm "now active" if we'd previously announced sync was
            // down (install/re-login/re-enable). A plain cold-start promotion
            // from the early-detect race stays silent and must not re-notify
            // on every launch.
            if was_announced_down(app_handle) {
                crate::send_notification(
                    app_handle,
                    "Clipboard sync ready",
                    "The ClusterCut GNOME extension is active — clipboard sync is now running.",
                    false,
                    None,
                    "history",
                    crate::NotificationPayload::None,
                );
                clear_announced_down(app_handle);
            }
        }
        (ClipboardBackend::GnomeExtension, false) => {
            tracing::warn!(
                "ClusterCut extension is no longer available — degrading clipboard backend. \
                 Clipboard sync will be paused until the extension is re-enabled."
            );
            set_backend(ClipboardBackend::Degraded);
            if let Some(cancel) = monitor_cancel.take() {
                cancel.notify_one();
            }
            crate::send_notification(
                app_handle,
                "Clipboard sync paused",
                "The ClusterCut GNOME extension is no longer active. Re-enable it to resume clipboard sync.",
                true,
                None,
                "history",
                crate::NotificationPayload::None,
            );
            // We've announced sync is down — arm the one-shot recovery notice.
            mark_announced_down(app_handle);
        }
        _ => {
            // No transition needed.
        }
    }
}
