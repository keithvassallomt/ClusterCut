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
use tauri::AppHandle;
use tokio::sync::Notify;

const EXTENSION_UUID: &str = "clustercut@keithvassallo.com";
const SHELL_EXT_NAME: &str = "org.gnome.Shell.Extensions";
const SHELL_EXT_PATH: &str = "/org/gnome/Shell/Extensions";
const SHELL_EXT_IFACE: &str = "org.gnome.Shell.Extensions";

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
            crate::send_notification(
                app_handle,
                "Clipboard sync ready",
                "The ClusterCut GNOME extension is active — clipboard sync is now running.",
                false,
                None,
                "history",
                crate::NotificationPayload::None,
            );
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
        }
        _ => {
            // No transition needed.
        }
    }
}
