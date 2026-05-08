use crate::crypto;
use crate::protocol::{ClipboardBlob, ClipboardPayload, FileMetadata, Message};
use crate::state::AppState;
use crate::transport::Transport;
use std::thread;
use tauri::{AppHandle, Emitter};

use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex};

/// Wire-format size cap for clipboard image blobs. Sender drops anything over.
pub const MAX_CLIPBOARD_IMAGE_WIRE_BYTES: usize = 10 * 1024 * 1024;

/// Map a MIME string to an `image::ImageFormat`. Returns `None` for unknown
/// types so callers can skip unsupported sources cleanly.
pub fn image_format_for_mime(mime: &str) -> Option<image::ImageFormat> {
    match mime {
        "image/png" => Some(image::ImageFormat::Png),
        "image/jpeg" => Some(image::ImageFormat::Jpeg),
        "image/webp" => Some(image::ImageFormat::WebP),
        "image/bmp" | "image/x-bmp" => Some(image::ImageFormat::Bmp),
        "image/tiff" => Some(image::ImageFormat::Tiff),
        "image/gif" => Some(image::ImageFormat::Gif),
        _ => None,
    }
}

/// Decode raw clipboard bytes of a known image MIME and return a normalised
/// `ClipboardBlob` containing PNG bytes. Used by both the Wayland (wlr) and
/// GNOME-extension (D-Bus) backends. Returns `None` if the MIME isn't an
/// image format we know, the bytes don't decode, or the encoded blob exceeds
/// `MAX_CLIPBOARD_IMAGE_WIRE_BYTES`.
///
/// PNG sources skip the re-encode step — we just validate the bytes by
/// loading them and reuse the original buffer.
pub fn normalize_image_blob_from_bytes(
    bytes: Vec<u8>,
    source_mime: &str,
) -> Option<ClipboardBlob> {
    let format = image_format_for_mime(source_mime)?;

    let img = match image::load_from_memory_with_format(&bytes, format) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("Failed to decode clipboard {}: {}", source_mime, e);
            return None;
        }
    };
    let width = img.width();
    let height = img.height();

    let png_bytes = if matches!(format, image::ImageFormat::Png) {
        bytes
    } else {
        let mut out = Vec::new();
        if let Err(e) = img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        {
            tracing::warn!("Failed to PNG-encode clipboard image: {}", e);
            return None;
        }
        out
    };

    if png_bytes.len() > MAX_CLIPBOARD_IMAGE_WIRE_BYTES {
        tracing::warn!(
            "Clipboard image PNG ({} bytes) exceeds {} byte wire cap; skipping.",
            png_bytes.len(),
            MAX_CLIPBOARD_IMAGE_WIRE_BYTES
        );
        return None;
    }

    Some(ClipboardBlob {
        mime_type: "image/png".to_string(),
        data: png_bytes,
        width: Some(width),
        height: Some(height),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClipboardContent {
    Text(String),
    Files(Vec<String>),
    Image(ClipboardBlob),
    None,
}

pub static IGNORED_CONTENT: Lazy<Arc<Mutex<ClipboardContent>>> =
    Lazy::new(|| Arc::new(Mutex::new(ClipboardContent::None)));

/// Process a clipboard read result through the dedup/feedback-loop logic.
/// Returns true if the content should be broadcast.
pub fn should_process_content(
    current_content: &ClipboardContent,
    last_content: &ClipboardContent,
) -> bool {
    let mut should_process = false;
    {
        let mut ignored = IGNORED_CONTENT.lock().unwrap();
        match &*ignored {
            ClipboardContent::None => {
                if current_content != last_content && *current_content != ClipboardContent::None {
                    should_process = true;
                }
            }
            ClipboardContent::Text(ign_text) => {
                if let ClipboardContent::Text(curr_text) = current_content {
                    if curr_text == ign_text {
                        *ignored = ClipboardContent::None;
                    } else if current_content != last_content {
                        should_process = true;
                    }
                } else if current_content != last_content
                    && *current_content != ClipboardContent::None
                {
                    should_process = true;
                }
            }
            ClipboardContent::Files(ign_files) => {
                if let ClipboardContent::Files(curr_files) = current_content {
                    if curr_files == ign_files {
                        *ignored = ClipboardContent::None;
                    } else if current_content != last_content {
                        should_process = true;
                    }
                } else if current_content != last_content
                    && *current_content != ClipboardContent::None
                {
                    should_process = true;
                }
            }
            ClipboardContent::Image(ign_blob) => {
                if let ClipboardContent::Image(curr_blob) = current_content {
                    if curr_blob == ign_blob {
                        *ignored = ClipboardContent::None;
                    } else if current_content != last_content {
                        should_process = true;
                    }
                } else if current_content != last_content
                    && *current_content != ClipboardContent::None
                {
                    should_process = true;
                }
            }
        }
    }
    should_process
}

/// Process a changed clipboard content: build payload and broadcast.
pub fn process_clipboard_change(
    content: ClipboardContent,
    app_handle: &AppHandle,
    state: &AppState,
    transport: &Transport,
) {
    match content {
        ClipboardContent::Text(text) => {
            tracing::debug!("Clipboard Text Change Detected (len={})", text.len());

            {
                let mut last_global = state.last_clipboard_content.lock().unwrap();
                if *last_global != text {
                    *last_global = text.clone();
                }
            }

            let hostname = crate::get_hostname_internal();
            let msg_id = uuid::Uuid::new_v4().to_string();
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let local_id = state.local_device_id.lock().unwrap().clone();
            let payload_obj = ClipboardPayload {
                id: msg_id,
                text,
                files: None,
                blob: None,
                timestamp: ts,
                sender: hostname,
                sender_id: local_id,
            };

            broadcast_clipboard(app_handle, state, transport, payload_obj);
        }
        ClipboardContent::Files(raw_paths) => {
            tracing::debug!(
                "Clipboard File Change Detected. Raw paths: {:?}",
                raw_paths
            );

            let hostname = crate::get_hostname_internal();
            let msg_id = uuid::Uuid::new_v4().to_string();
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let mut file_metas = Vec::new();
            let mut valid_paths = Vec::new();

            for path_str in &raw_paths {
                let path_buf = if let Ok(u) = url::Url::parse(path_str) {
                    if u.scheme() == "file" {
                        if let Ok(p) = u.to_file_path() {
                            p
                        } else {
                            std::path::PathBuf::from(path_str)
                        }
                    } else {
                        std::path::PathBuf::from(path_str)
                    }
                } else {
                    let decoded = percent_encoding::percent_decode_str(path_str)
                        .decode_utf8_lossy();
                    std::path::PathBuf::from(decoded.as_ref())
                };

                let path = path_buf.as_path();
                if path.exists() {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    file_metas.push(FileMetadata { name, size });
                    valid_paths.push(path.to_string_lossy().to_string());
                } else if path_buf.to_string_lossy() != *path_str {
                    let raw_p = std::path::Path::new(path_str);
                    if raw_p.exists() {
                        let name = raw_p
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let size = std::fs::metadata(raw_p).map(|m| m.len()).unwrap_or(0);
                        file_metas.push(FileMetadata { name, size });
                        valid_paths.push(path_str.clone());
                    } else {
                        tracing::warn!("Path does not exist: {:?}", path);
                    }
                } else {
                    tracing::warn!("Path does not exist: {:?}", path);
                }
            }

            if !file_metas.is_empty() {
                let mut sig = String::from("FILES:");
                for f in &file_metas {
                    use std::fmt::Write;
                    let _ = write!(sig, "{}:{};", f.name, f.size);
                }

                {
                    let mut last_global = state.last_clipboard_content.lock().unwrap();
                    if *last_global == sig {
                        tracing::debug!(
                            "Ignoring broadcast - files match last_clipboard_content"
                        );
                        return;
                    }
                    *last_global = sig;
                }

                {
                    let mut files_lock = state.local_files.lock().unwrap();
                    files_lock.insert(msg_id.clone(), valid_paths);
                }

                let local_id = state.local_device_id.lock().unwrap().clone();
                let payload_obj = ClipboardPayload {
                    id: msg_id,
                    text: String::new(),
                    files: Some(file_metas),
                    blob: None,
                    timestamp: ts,
                    sender: hostname,
                    sender_id: local_id,
                };
                broadcast_clipboard(app_handle, state, transport, payload_obj);
            } else {
                tracing::warn!("No valid files found in clipboard content.");
            }
        }
        ClipboardContent::Image(blob) => {
            tracing::debug!(
                "Clipboard Image Change Detected (mime={}, len={})",
                blob.mime_type,
                blob.data.len()
            );

            // Dedup: blob signature is mime + length + first/last 16 bytes (cheap, avoids full
            // byte-equality on multi-megabyte images while still distinguishing distinct copies).
            let head_len = blob.data.len().min(16);
            let tail_start = blob.data.len().saturating_sub(16);
            let mut sig = format!("BLOB:{}:{}:", blob.mime_type, blob.data.len());
            for b in &blob.data[..head_len] {
                use std::fmt::Write;
                let _ = write!(sig, "{:02x}", b);
            }
            sig.push(':');
            for b in &blob.data[tail_start..] {
                use std::fmt::Write;
                let _ = write!(sig, "{:02x}", b);
            }

            {
                let mut last_global = state.last_clipboard_content.lock().unwrap();
                if *last_global == sig {
                    tracing::debug!(
                        "Ignoring broadcast - blob matches last_clipboard_content"
                    );
                    return;
                }
                *last_global = sig;
            }

            let hostname = crate::get_hostname_internal();
            let msg_id = uuid::Uuid::new_v4().to_string();
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let local_id = state.local_device_id.lock().unwrap().clone();
            let payload_obj = ClipboardPayload {
                id: msg_id,
                text: String::new(),
                files: None,
                blob: Some(blob),
                timestamp: ts,
                sender: hostname,
                sender_id: local_id,
            };
            broadcast_clipboard(app_handle, state, transport, payload_obj);
        }
        ClipboardContent::None => {}
    }
}

pub fn broadcast_clipboard(
    app_handle: &AppHandle,
    state: &AppState,
    transport: &Transport,
    payload_obj: ClipboardPayload,
) {
    let auto_send = { state.settings.lock().unwrap().auto_send };
    if !auto_send {
        tracing::debug!("Auto-send disabled. Emitting monitor update only.");
        let _ = app_handle.emit("clipboard-monitor-update", &payload_obj);
        return;
    }

    let _ = app_handle.emit("clipboard-change", &payload_obj);

    let payload_bytes = match serde_json::to_vec(&payload_obj) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("Failed to serialize clipboard payload: {}", e);
            return;
        }
    };

    let ck_lock = state.cluster_key.lock().unwrap();
    if let Some(key) = ck_lock.as_ref() {
        if key.len() == 32 {
            let mut key_arr = [0u8; 32];
            key_arr.copy_from_slice(key);

            match crypto::encrypt(&key_arr, &payload_bytes) {
                Ok(cipher) => {
                    let msg = Message::Clipboard(cipher);
                    let data = serde_json::to_vec(&msg).unwrap_or_default();

                    let peers = state.get_peers();
                    if !peers.is_empty() {
                        let notifications =
                            state.settings.lock().unwrap().notifications.clone();
                        if notifications.data_sent {
                            let body = if payload_obj.files.is_some() {
                                "File info broadcasted to cluster."
                            } else if payload_obj.blob.is_some() {
                                "Image broadcasted to cluster."
                            } else {
                                "Clipboard content broadcasted to cluster."
                            };
                            crate::send_notification(
                                app_handle,
                                "Clipboard Sent",
                                body,
                                false,
                                Some(2),
                                "history",
                                crate::NotificationPayload::None,
                            );
                        }
                    }

                    for peer in peers.values() {
                        let addr = std::net::SocketAddr::new(peer.ip, peer.port);
                        let transport_clone = transport.clone();
                        let data_vec = data.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = transport_clone.send_message(addr, &data_vec).await {
                                tracing::error!("Failed to send to {}: {}", addr, e);
                            } else {
                                tracing::info!("Sent clipboard to {}", addr);
                            }
                        });
                    }
                }
                Err(e) => tracing::error!("Encryption failed: {}", e),
            }
        }
    }
}

/// Set clipboard text, with feedback loop prevention.
pub fn set_clipboard_with_ignore(app: &AppHandle, text: String, write_fn: fn(&AppHandle, String) -> Result<(), String>) {
    let app_handle = app.clone();
    let text_clone = text.clone();

    thread::spawn(move || {
        {
            let mut ignored = IGNORED_CONTENT.lock().unwrap();
            *ignored = ClipboardContent::Text(text_clone.clone());
        }

        if let Err(e) = write_fn(&app_handle, text_clone) {
            tracing::error!("Failed to set clipboard text: {}", e);
        } else {
            tracing::debug!("Successfully set local clipboard text.");
        }
    });
}

/// Set clipboard files, with feedback loop prevention.
pub fn set_clipboard_paths_with_ignore(app: &AppHandle, paths: Vec<String>, write_fn: fn(&AppHandle, Vec<String>) -> Result<(), String>) {
    let app_handle = app.clone();
    let paths_clone = paths.clone();

    thread::spawn(move || {
        {
            let mut ignored = IGNORED_CONTENT.lock().unwrap();
            *ignored = ClipboardContent::Files(paths_clone.clone());
        }

        if let Err(e) = write_fn(&app_handle, paths_clone) {
            tracing::error!("Failed to set clipboard files: {}", e);
        } else {
            tracing::debug!("Successfully set local clipboard files.");
        }
    });
}

/// Set clipboard image blob, with feedback loop prevention.
/// `write_fn` is the platform-specific writer that places `data` on the OS clipboard
/// under `mime_type` (canonically "image/png" today).
pub fn set_clipboard_blob_with_ignore(
    app: &AppHandle,
    blob: ClipboardBlob,
    write_fn: fn(&AppHandle, &ClipboardBlob) -> Result<(), String>,
) {
    let app_handle = app.clone();
    let blob_clone = blob.clone();

    thread::spawn(move || {
        {
            let mut ignored = IGNORED_CONTENT.lock().unwrap();
            *ignored = ClipboardContent::Image(blob_clone.clone());
        }

        if let Err(e) = write_fn(&app_handle, &blob_clone) {
            tracing::error!("Failed to set clipboard blob: {}", e);
        } else {
            tracing::debug!(
                "Successfully set local clipboard blob (mime={}, len={}).",
                blob_clone.mime_type,
                blob_clone.data.len()
            );
        }
    });
}
