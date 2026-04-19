/// Clipboard backend using the GNOME Shell extension as a D-Bus clipboard bridge.
/// Used on GNOME Wayland where wlr-data-control is not available.
///
/// The GNOME extension monitors clipboard via St.Clipboard (privileged compositor access)
/// and exposes clipboard operations over D-Bus.
use super::common::{self, ClipboardContent};
use crate::state::AppState;
use crate::transport::Transport;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::Notify;

pub(crate) const DBUS_NAME: &str = "org.gnome.Shell";
pub(crate) const DBUS_PATH: &str = "/org/gnome/Shell/Extensions/ClusterCut";
pub(crate) const DBUS_IFACE: &str = "app.clustercut.clustercut.Clipboard";

/// Check if the GNOME extension's clipboard bridge is available on D-Bus.
/// Uses blocking D-Bus so it can be called before the async runtime is ready.
pub fn is_available() -> bool {
    let conn = match zbus::blocking::Connection::session() {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Check if the extension exposes the Clipboard interface
    let msg = conn.call_method(
        Some(DBUS_NAME),
        DBUS_PATH,
        Some("org.freedesktop.DBus.Introspectable"),
        "Introspect",
        &(),
    );

    match msg {
        Ok(reply) => {
            if let Ok(xml) = reply.body().deserialize::<String>() {
                xml.contains(DBUS_IFACE)
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// Async version of `is_available` for use from within tokio tasks.
pub async fn is_available_async() -> bool {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(_) => return false,
    };

    let reply = conn
        .call_method(
            Some(DBUS_NAME),
            DBUS_PATH,
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )
        .await;

    match reply {
        Ok(msg) => msg
            .body()
            .deserialize::<String>()
            .map(|xml| xml.contains(DBUS_IFACE))
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// Read clipboard text via D-Bus from the extension (blocking).
fn read_text_dbus() -> Result<String, String> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("D-Bus connection failed: {}", e))?;

    let reply = conn
        .call_method(
            Some(DBUS_NAME),
            DBUS_PATH,
            Some(DBUS_IFACE),
            "ReadClipboard",
            &(),
        )
        .map_err(|e| format!("ReadClipboard D-Bus call failed: {}", e))?;

    reply
        .body()
        .deserialize::<String>()
        .map_err(|e| format!("Failed to deserialize clipboard text: {}", e))
}

/// Write clipboard text via D-Bus to the extension (blocking).
fn write_text_dbus(text: &str) -> Result<(), String> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("D-Bus connection failed: {}", e))?;

    conn.call_method(
        Some(DBUS_NAME),
        DBUS_PATH,
        Some(DBUS_IFACE),
        "WriteClipboard",
        &(text,),
    )
    .map_err(|e| format!("WriteClipboard D-Bus call failed: {}", e))?;

    Ok(())
}

fn write_text(_app: &AppHandle, text: String) -> Result<(), String> {
    write_text_dbus(&text)
}

fn write_files(_app: &AppHandle, files: Vec<String>) -> Result<(), String> {
    // Convert paths to file:// URIs
    let uris: Vec<String> = files
        .into_iter()
        .filter_map(|p| {
            let path = std::path::Path::new(&p);
            let abs_path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().ok()?.join(path)
            };
            url::Url::from_file_path(abs_path)
                .ok()
                .map(|u| u.to_string())
        })
        .collect();

    if uris.is_empty() {
        return Err("No valid file paths".to_string());
    }

    // Write as newline-separated URI list
    let uri_text = uris.join("\n");
    write_text_dbus(&uri_text)
}

pub fn set_clipboard(app: &AppHandle, text: String) {
    common::set_clipboard_with_ignore(app, text, write_text);
}

pub fn set_clipboard_paths(app: &AppHandle, paths: Vec<String>) {
    common::set_clipboard_paths_with_ignore(app, paths, write_files);
}

/// Read clipboard text directly (for manual send shortcut).
pub fn read_text(_app: &AppHandle) -> Result<String, String> {
    read_text_dbus()
}

/// Write clipboard text directly (for manual receive shortcut).
pub fn write_text_direct(app: &AppHandle, text: String) -> Result<(), String> {
    write_text(app, text)
}

/// Start the GNOME extension D-Bus clipboard monitor. Returns a cancel handle;
/// call `Notify::notify_one()` on it (or drop the last Arc) to stop the monitor.
pub fn start_monitor(
    app_handle: AppHandle,
    state: AppState,
    transport: Transport,
) -> Arc<Notify> {
    let cancel = Arc::new(Notify::new());
    let cancel_task = cancel.clone();

    tauri::async_runtime::spawn(async move {
        tracing::info!("Starting GNOME extension clipboard monitor (D-Bus bridge)");

        let conn = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to connect to D-Bus for clipboard monitoring: {}", e);
                return;
            }
        };

        let proxy = match zbus::Proxy::new(&conn, DBUS_NAME, DBUS_PATH, DBUS_IFACE).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Failed to create D-Bus proxy for clipboard bridge: {}", e);
                return;
            }
        };

        let mut stream = match proxy.receive_signal("ClipboardChanged").await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to subscribe to ClipboardChanged signal: {}", e);
                return;
            }
        };

        use futures::StreamExt;

        let mut last_content = ClipboardContent::None;

        loop {
            if state.is_shutdown() {
                tracing::info!("GNOME clipboard monitor received shutdown signal, exiting.");
                break;
            }

            tokio::select! {
                _ = cancel_task.notified() => {
                    tracing::info!("GNOME clipboard monitor cancelled, exiting.");
                    break;
                }
                next = stream.next() => {
                    match next {
                        Some(msg) => {
                            if let Ok(text) = msg.body().deserialize::<String>() {
                                if !text.is_empty() {
                                    let current_content = ClipboardContent::Text(text);

                                    if common::should_process_content(&current_content, &last_content) {
                                        last_content = current_content.clone();
                                        common::process_clipboard_change(
                                            current_content,
                                            &app_handle,
                                            &state,
                                            &transport,
                                        );
                                    } else {
                                        last_content = current_content;
                                    }
                                }
                            }
                        }
                        None => {
                            tracing::warn!("D-Bus clipboard signal stream ended");
                            break;
                        }
                    }
                }
            }
        }
    });

    cancel
}
