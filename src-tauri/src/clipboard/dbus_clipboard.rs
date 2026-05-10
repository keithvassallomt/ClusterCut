/// Clipboard backend using the GNOME Shell extension as a D-Bus clipboard bridge.
/// Used on GNOME Wayland where wlr-data-control is not available.
///
/// The GNOME extension monitors clipboard via St.Clipboard (privileged compositor access)
/// and exposes clipboard operations over D-Bus.
use super::common::{self, ClipboardContent};
use crate::protocol::{ClipboardBlob, ClipboardFormat};
use crate::state::AppState;
use crate::transport::Transport;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::Notify;

pub(crate) const DBUS_NAME: &str = "org.gnome.Shell";
pub(crate) const DBUS_PATH: &str = "/org/gnome/Shell/Extensions/ClusterCut";
// Interface is versioned (.Clipboard2) so the is_available() probe can't be
// fooled by an older extension install that only exposes text operations.
pub(crate) const DBUS_IFACE: &str = "app.clustercut.clustercut.Clipboard2";

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

/// Write a list of file URIs via D-Bus. The extension writes both
/// `text/uri-list` and `x-special/gnome-copied-files` so file managers
/// recognise the paste as a file copy rather than plain text.
fn write_files_dbus(uris: &[String]) -> Result<(), String> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("D-Bus connection failed: {}", e))?;

    conn.call_method(
        Some(DBUS_NAME),
        DBUS_PATH,
        Some(DBUS_IFACE),
        "WriteFiles",
        &(uris,),
    )
    .map_err(|e| format!("WriteFiles D-Bus call failed: {}", e))?;

    Ok(())
}

/// Write an image blob to the GNOME extension's clipboard via WriteBlob.
/// Returns Err on extensions older than v4.0 that don't implement WriteBlob —
/// callers handle the error gracefully (image clipboard is silently disabled
/// against older extensions).
fn write_blob_dbus(mime: &str, data: &[u8]) -> Result<(), String> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("D-Bus connection failed: {}", e))?;

    conn.call_method(
        Some(DBUS_NAME),
        DBUS_PATH,
        Some(DBUS_IFACE),
        "WriteBlob",
        &(mime, data),
    )
    .map_err(|e| format!("WriteBlob D-Bus call failed: {}", e))?;

    Ok(())
}

/// Write plain text + alternate formats atomically via WriteFormats.
/// Returns Err on older extensions that don't implement WriteFormats —
/// callers fall back to plain-text writes via WriteClipboard.
fn write_formats_dbus(text: &str, formats: &[(String, Vec<u8>)]) -> Result<(), String> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("D-Bus connection failed: {}", e))?;

    conn.call_method(
        Some(DBUS_NAME),
        DBUS_PATH,
        Some(DBUS_IFACE),
        "WriteFormats",
        &(text, formats),
    )
    .map_err(|e| format!("WriteFormats D-Bus call failed: {}", e))?;

    Ok(())
}

fn write_text(_app: &AppHandle, text: String) -> Result<(), String> {
    write_text_dbus(&text)
}

fn write_image(_app: &AppHandle, blob: &ClipboardBlob) -> Result<(), String> {
    let bytes = blob.raw_bytes()?;
    write_blob_dbus(&blob.mime_type, &bytes)
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

    write_files_dbus(&uris)
}

pub fn set_clipboard(app: &AppHandle, text: String) {
    common::set_clipboard_with_ignore(app, text, write_text);
}

pub fn set_clipboard_paths(app: &AppHandle, paths: Vec<String>) {
    common::set_clipboard_paths_with_ignore(app, paths, write_files);
}

pub fn set_clipboard_image(app: &AppHandle, blob: ClipboardBlob) {
    common::set_clipboard_blob_with_ignore(app, blob, write_image);
}

/// Write rich content (plain text + alternate formats) via WriteFormats.
/// Falls back to plain-text via WriteClipboard if the extension is older than
/// v4.0 and doesn't have WriteFormats — receiver still gets readable text,
/// just without the formatting.
fn write_rich(app: &AppHandle, text: &str, formats: &[ClipboardFormat]) -> Result<(), String> {
    let dbus_formats: Vec<(String, Vec<u8>)> = formats
        .iter()
        .filter_map(|f| f.raw_bytes().ok().map(|b| (f.mime_type.clone(), b)))
        .collect();

    match write_formats_dbus(text, &dbus_formats) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::debug!(
                "WriteFormats unavailable ({}); falling back to plain text WriteClipboard",
                e
            );
            write_text(app, text.to_string())
        }
    }
}

pub fn set_clipboard_rich(app: &AppHandle, text: String, formats: Vec<ClipboardFormat>) {
    common::set_clipboard_rich_with_ignore(app, text, formats, write_rich);
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

        let mut text_stream = match proxy.receive_signal("ClipboardChanged").await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to subscribe to ClipboardChanged signal: {}", e);
                return;
            }
        };

        let mut files_stream = match proxy.receive_signal("FilesChanged").await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to subscribe to FilesChanged signal: {}", e);
                return;
            }
        };

        // BlobChanged was added in extension v4.0. The match rule is just a
        // name filter on incoming signals — subscribing against an older
        // extension that never emits BlobChanged is harmless: the stream
        // simply never produces, and image clipboard sync is silently
        // unavailable. Text + files keep working unchanged in either case.
        let mut blob_stream = match proxy.receive_signal("BlobChanged").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Failed to subscribe to BlobChanged signal (image clipboard sync disabled): {}",
                    e
                );
                // Subscribe to the same signal name that produces nothing — by
                // chaining off a different signal we don't care about — just so
                // the select! arm has a valid stream to await.
                proxy
                    .receive_signal("ClipboardChanged")
                    .await
                    .expect("ClipboardChanged subscription succeeded above")
            }
        };

        // FormatsChanged also v4.0 — same compatibility rationale as BlobChanged.
        let mut formats_stream = match proxy.receive_signal("FormatsChanged").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Failed to subscribe to FormatsChanged signal (rich-text sync disabled): {}",
                    e
                );
                proxy
                    .receive_signal("ClipboardChanged")
                    .await
                    .expect("ClipboardChanged subscription succeeded above")
            }
        };

        use futures::StreamExt;

        let mut last_content = ClipboardContent::None;

        loop {
            if state.is_shutdown() {
                tracing::info!("GNOME clipboard monitor received shutdown signal, exiting.");
                break;
            }

            let new_content: Option<ClipboardContent> = tokio::select! {
                _ = cancel_task.notified() => {
                    tracing::info!("GNOME clipboard monitor cancelled, exiting.");
                    break;
                }
                next = text_stream.next() => match next {
                    Some(msg) => match msg.body().deserialize::<String>() {
                        Ok(text) if !text.is_empty() => Some(ClipboardContent::Text(text)),
                        _ => None,
                    },
                    None => {
                        tracing::warn!("D-Bus ClipboardChanged stream ended");
                        break;
                    }
                },
                next = files_stream.next() => match next {
                    Some(msg) => match msg.body().deserialize::<Vec<String>>() {
                        Ok(uris) if !uris.is_empty() => Some(ClipboardContent::Files(uris)),
                        _ => None,
                    },
                    None => {
                        tracing::warn!("D-Bus FilesChanged stream ended");
                        break;
                    }
                },
                next = blob_stream.next() => match next {
                    Some(msg) => match msg.body().deserialize::<(String, Vec<u8>)>() {
                        Ok((mime, data)) if !data.is_empty() => {
                            common::build_image_blob(data, &mime)
                                .map(ClipboardContent::Image)
                        }
                        _ => None,
                    },
                    None => {
                        tracing::warn!("D-Bus BlobChanged stream ended");
                        break;
                    }
                },
                next = formats_stream.next() => match next {
                    Some(msg) => match msg.body().deserialize::<(String, Vec<(String, Vec<u8>)>)>() {
                        Ok((text, raw_formats)) if !raw_formats.is_empty() => {
                            // Wrap each (mime, bytes) tuple into a ClipboardFormat.
                            // Text formats (text/html, text/rtf) are UTF-8 — drop
                            // the format if it doesn't decode rather than send
                            // garbage downstream.
                            let formats: Vec<ClipboardFormat> = raw_formats
                                .into_iter()
                                .filter_map(|(mime, bytes)| match String::from_utf8(bytes) {
                                    Ok(s) => Some(ClipboardFormat::from_text(mime, s)),
                                    Err(e) => {
                                        tracing::warn!(
                                            "FormatsChanged: {} did not decode as UTF-8: {}",
                                            mime,
                                            e
                                        );
                                        None
                                    }
                                })
                                .collect();
                            if formats.is_empty() {
                                None
                            } else {
                                Some(ClipboardContent::Rich { text, formats })
                            }
                        }
                        _ => None,
                    },
                    None => {
                        tracing::warn!("D-Bus FormatsChanged stream ended");
                        break;
                    }
                },
            };

            if let Some(current_content) = new_content {
                match common::should_process_content(&current_content, &last_content) {
                    common::EchoVerdict::Process => {
                        last_content = current_content.clone();
                        common::process_clipboard_change(
                            current_content,
                            &app_handle,
                            &state,
                            &transport,
                        );
                    }
                    common::EchoVerdict::Echo => {
                        last_content = current_content;
                    }
                    common::EchoVerdict::NoChange => {}
                }
            }
        }
    });

    cancel
}
