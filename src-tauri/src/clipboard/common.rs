use crate::crypto;
use crate::protocol::{ClipboardBlob, ClipboardFormat, ClipboardPayload, FileMetadata, Message};
use crate::state::AppState;
use crate::transport::Transport;
use std::thread;
use tauri::{AppHandle, Emitter};

use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex};

/// Wire-format size cap for clipboard image blobs. Sender drops anything over.
#[cfg(target_os = "linux")]
pub const MAX_CLIPBOARD_IMAGE_WIRE_BYTES: usize = 10 * 1024 * 1024;

/// Compute a stable, cheap content fingerprint for a `ClipboardPayload` used
/// by both the sender (broadcast dedup) and receiver (re-broadcast loop guard)
/// against `state.last_clipboard_content`. Both ends must agree on the format
/// so a blob received from a peer can correctly suppress an immediate
/// re-broadcast back to that peer.
///
/// Format:
/// - Files:    `FILES:name1:size1;name2:size2;…`
/// - Blob:     `BLOB:<mime>:<base64_len>:<head16_hex>:<tail16_hex>`
/// - Formats:  `<text>|FORMATS:mime1:len1;mime2:len2;…` (when `formats` is
///             non-empty; the text portion still distinguishes two copies
///             that happen to carry the same MIME set with different bytes)
/// - Text:     the text itself (or empty string)
pub fn payload_signature(payload: &ClipboardPayload) -> String {
    if let Some(files) = payload.files.as_ref() {
        if !files.is_empty() {
            let mut sig = String::from("FILES:");
            for f in files {
                use std::fmt::Write;
                let _ = write!(sig, "{}:{};", f.name, f.size);
            }
            return sig;
        }
    }
    if let Some(blob) = payload.blob.as_ref() {
        let raw = blob.data.as_bytes();
        let head_len = raw.len().min(16);
        let tail_start = raw.len().saturating_sub(16);
        let mut sig = format!("BLOB:{}:{}:", blob.mime_type, raw.len());
        for b in &raw[..head_len] {
            use std::fmt::Write;
            let _ = write!(sig, "{:02x}", b);
        }
        sig.push(':');
        for b in &raw[tail_start..] {
            use std::fmt::Write;
            let _ = write!(sig, "{:02x}", b);
        }
        return sig;
    }
    if let Some(formats) = payload.formats.as_ref() {
        if !formats.is_empty() {
            let mut sig = payload.text.clone();
            sig.push_str("|FORMATS:");
            for f in formats {
                use std::fmt::Write;
                let _ = write!(sig, "{}:{};", f.mime_type, f.data.len());
            }
            return sig;
        }
    }
    payload.text.clone()
}

/// Map a MIME string to an `image::ImageFormat`. Returns `None` for unknown
/// types so callers can skip unsupported sources cleanly.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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

    Some(ClipboardBlob::from_bytes(
        "image/png",
        &png_bytes,
        Some(width),
        Some(height),
    ))
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClipboardContent {
    Text(String),
    Files(Vec<String>),
    Image(ClipboardBlob),
    /// Plain text plus one or more alternate format representations
    /// (text/html, text/rtf, …). Used when the OS clipboard offers rich
    /// formats — receivers re-stock all the formats they can write.
    Rich {
        text: String,
        formats: Vec<ClipboardFormat>,
    },
    None,
}

pub static IGNORED_CONTENT: Lazy<Arc<Mutex<ClipboardContent>>> =
    Lazy::new(|| Arc::new(Mutex::new(ClipboardContent::None)));

/// Stable equivalence for image blobs across an OS-clipboard round-trip.
///
/// `ClipboardBlob`'s derived `PartialEq` compares the base64 `data` field
/// byte-for-byte, but a blob written via `arboard::set_image` and then read
/// back via the monitor's `arboard::get_image` is RGBA-decoded then PNG-
/// re-encoded by the `image` crate, producing different bytes for the same
/// pixels. That false negative on the IGNORED_CONTENT match is what made
/// the receiver re-broadcast every image it received — see TIRI.
///
/// Equivalence is now `(mime_type, width, height)` when both sides have
/// dimensions (which is the normal path — every backend that produces a
/// `ClipboardBlob` populates them). Falls back to byte-exact when either
/// side is missing dimensions, so legacy or hand-built blobs without
/// `width`/`height` still get sensible behaviour.
fn image_blob_eq_stable(a: &ClipboardBlob, b: &ClipboardBlob) -> bool {
    if a.mime_type != b.mime_type {
        return false;
    }
    match ((a.width, a.height), (b.width, b.height)) {
        ((Some(aw), Some(ah)), (Some(bw), Some(bh))) => aw == bw && ah == bh,
        _ => a.data == b.data,
    }
}

/// Stable equivalence for rich-text payloads across an OS-clipboard round-
/// trip. The `text` field round-trips byte-stably (every backend stores
/// plain UTF-8 text verbatim — NSPasteboard, Win32 CF_UNICODETEXT, and
/// Wayland `text/plain` all preserve bytes), but the per-format `data`
/// strings can be normalised by the OS layer (line endings, charset
/// declarations, etc.). Equivalence is `(text, sorted MIME set)` — drops
/// the per-format byte length comparison that was making the receiver
/// re-broadcast every rich-text paste it accepted.
///
/// Edge case: two distinct rich copies that happen to have the same plain
/// text and the same MIME set would compare equal. Vanishingly unlikely
/// in practice, and the cost of mis-suppressing one such copy is much
/// lower than the cost of bouncing every rich copy.
fn rich_eq_stable(
    ign_text: &str,
    ign_formats: &[ClipboardFormat],
    curr_text: &str,
    curr_formats: &[ClipboardFormat],
) -> bool {
    if ign_text != curr_text {
        return false;
    }
    let mut ign_mimes: Vec<&str> = ign_formats.iter().map(|f| f.mime_type.as_str()).collect();
    let mut curr_mimes: Vec<&str> =
        curr_formats.iter().map(|f| f.mime_type.as_str()).collect();
    ign_mimes.sort();
    curr_mimes.sort();
    ign_mimes == curr_mimes
}

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
                    // Stable comparison closes TIRI — a PNG round-tripped
                    // through the OS clipboard has different bytes but the
                    // same (mime, dims) and is therefore our own echo.
                    if image_blob_eq_stable(curr_blob, ign_blob) {
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
            ClipboardContent::Rich {
                text: ign_text,
                formats: ign_formats,
            } => {
                if let ClipboardContent::Rich { text, formats } = current_content {
                    // Stable comparison: text is byte-stable across the
                    // round-trip; the format data may be normalised by the
                    // OS clipboard layer, so we ignore per-format bytes
                    // and just compare the MIME set.
                    if rich_eq_stable(ign_text, ign_formats, text, formats) {
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
                formats: None,
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
                    formats: None,
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
                "Clipboard Image Change Detected (mime={}, decoded_len={})",
                blob.mime_type,
                blob.decoded_len()
            );

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
                formats: None,
                timestamp: ts,
                sender: hostname,
                sender_id: local_id,
            };

            let sig = payload_signature(&payload_obj);
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

            broadcast_clipboard(app_handle, state, transport, payload_obj);
        }
        ClipboardContent::Rich { text, formats } => {
            tracing::debug!(
                "Clipboard Rich Change Detected (text_len={}, format_count={})",
                text.len(),
                formats.len()
            );

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
                formats: Some(formats),
                timestamp: ts,
                sender: hostname,
                sender_id: local_id,
            };

            let sig = payload_signature(&payload_obj);
            {
                let mut last_global = state.last_clipboard_content.lock().unwrap();
                if *last_global == sig {
                    tracing::debug!(
                        "Ignoring broadcast - rich payload matches last_clipboard_content"
                    );
                    return;
                }
                *last_global = sig;
            }

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

/// Set clipboard rich content (plain text plus alternate formats like
/// text/html and text/rtf), with feedback loop prevention. The IGNORED_CONTENT
/// guard fires if the same Rich payload bounces straight back to us.
pub fn set_clipboard_rich_with_ignore(
    app: &AppHandle,
    text: String,
    formats: Vec<ClipboardFormat>,
    write_fn: fn(&AppHandle, &str, &[ClipboardFormat]) -> Result<(), String>,
) {
    let app_handle = app.clone();
    let text_clone = text.clone();
    let formats_clone = formats.clone();

    thread::spawn(move || {
        {
            let mut ignored = IGNORED_CONTENT.lock().unwrap();
            *ignored = ClipboardContent::Rich {
                text: text_clone.clone(),
                formats: formats_clone.clone(),
            };
        }

        if let Err(e) = write_fn(&app_handle, &text_clone, &formats_clone) {
            tracing::error!("Failed to set clipboard rich content: {}", e);
        } else {
            tracing::debug!(
                "Successfully set local clipboard rich (text_len={}, format_count={}).",
                text_clone.len(),
                formats_clone.len()
            );
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
                "Successfully set local clipboard blob (mime={}, decoded_len={}).",
                blob_clone.mime_type,
                blob_clone.decoded_len()
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClipboardFormat;

    fn payload_with(text: &str, formats: Option<Vec<ClipboardFormat>>) -> ClipboardPayload {
        ClipboardPayload {
            id: "id".to_string(),
            text: text.to_string(),
            files: None,
            blob: None,
            formats,
            timestamp: 0,
            sender: "host".to_string(),
            sender_id: "device".to_string(),
        }
    }

    #[test]
    fn signature_plain_text_is_just_text() {
        let p = payload_with("hello", None);
        assert_eq!(payload_signature(&p), "hello");
    }

    #[test]
    fn signature_with_empty_formats_falls_back_to_text() {
        let p = payload_with("hello", Some(vec![]));
        assert_eq!(payload_signature(&p), "hello");
    }

    #[test]
    fn signature_with_formats_includes_mime_and_lengths() {
        let html = ClipboardFormat::from_text("text/html", "<p>Hi</p>");
        let rtf = ClipboardFormat::from_text("text/rtf", r"{\rtf1 Hi}");
        let p = payload_with("Hi", Some(vec![html.clone(), rtf.clone()]));
        let sig = payload_signature(&p);
        assert!(sig.starts_with("Hi|FORMATS:"), "got: {}", sig);
        assert!(sig.contains(&format!("text/html:{};", html.data.len())));
        assert!(sig.contains(&format!("text/rtf:{};", rtf.data.len())));
    }

    #[test]
    fn signature_distinguishes_same_text_different_formats() {
        let html_a = ClipboardFormat::from_text("text/html", "<p>v1</p>");
        let html_b = ClipboardFormat::from_text("text/html", "<p>version 2</p>");
        let a = payload_with("plain", Some(vec![html_a]));
        let b = payload_with("plain", Some(vec![html_b]));
        assert_ne!(payload_signature(&a), payload_signature(&b));
    }

    #[test]
    fn signature_distinguishes_different_text_same_formats() {
        let html = ClipboardFormat::from_text("text/html", "<p>x</p>");
        let a = payload_with("first", Some(vec![html.clone()]));
        let b = payload_with("second", Some(vec![html]));
        assert_ne!(payload_signature(&a), payload_signature(&b));
    }

    // ─── TIRI: stable IGNORED_CONTENT comparison ───────────────────────────

    fn blob(mime: &str, data: &str, w: Option<u32>, h: Option<u32>) -> ClipboardBlob {
        ClipboardBlob {
            mime_type: mime.to_string(),
            data: data.to_string(),
            width: w,
            height: h,
        }
    }

    #[test]
    fn image_blob_eq_stable_matches_round_tripped_bytes() {
        // Same dims, different bytes — what TIRI looks like on the wire.
        let a = blob("image/png", "AAA…ORIGINAL_BYTES…", Some(1280), Some(720));
        let b = blob("image/png", "ZZZ…ROUNDTRIPPED…", Some(1280), Some(720));
        assert_ne!(a, b, "byte-exact PartialEq must still see them as different");
        assert!(image_blob_eq_stable(&a, &b), "stable comparator must match");
    }

    #[test]
    fn image_blob_eq_stable_distinguishes_different_dimensions() {
        let a = blob("image/png", "X", Some(1280), Some(720));
        let b = blob("image/png", "X", Some(640), Some(480));
        assert!(!image_blob_eq_stable(&a, &b));
    }

    #[test]
    fn image_blob_eq_stable_distinguishes_different_mime() {
        let a = blob("image/png", "X", Some(100), Some(100));
        let b = blob("image/jpeg", "X", Some(100), Some(100));
        assert!(!image_blob_eq_stable(&a, &b));
    }

    #[test]
    fn image_blob_eq_stable_falls_back_to_bytes_when_dimensions_absent() {
        // Without dims we have no choice but byte-exact — preserves old
        // behaviour for any backend that doesn't populate width/height.
        let a = blob("image/png", "X", None, None);
        let b = blob("image/png", "X", None, None);
        let c = blob("image/png", "Y", None, None);
        assert!(image_blob_eq_stable(&a, &b));
        assert!(!image_blob_eq_stable(&a, &c));
    }

    #[test]
    fn rich_eq_stable_matches_normalised_format_bytes() {
        // Plain text identical, MIMEs identical, but format bytes differ —
        // what rich-text TIRI looks like on the wire (line endings,
        // charset declarations etc. normalised by the OS clipboard layer).
        let ign_html = ClipboardFormat::from_text("text/html", "<p>v1\n</p>");
        let curr_html = ClipboardFormat::from_text("text/html", "<p>v1\r\n</p>");
        let ign = vec![ign_html];
        let curr = vec![curr_html];
        assert!(rich_eq_stable("hello", &ign, "hello", &curr));
    }

    #[test]
    fn rich_eq_stable_distinguishes_different_text() {
        let html = ClipboardFormat::from_text("text/html", "<p>x</p>");
        let formats = vec![html];
        assert!(!rich_eq_stable("hello", &formats, "world", &formats));
    }

    #[test]
    fn rich_eq_stable_distinguishes_different_mime_set() {
        let only_html = vec![ClipboardFormat::from_text("text/html", "<p>x</p>")];
        let html_and_rtf = vec![
            ClipboardFormat::from_text("text/html", "<p>x</p>"),
            ClipboardFormat::from_text("text/rtf", r"{\rtf1 x}"),
        ];
        assert!(!rich_eq_stable("hello", &only_html, "hello", &html_and_rtf));
    }

    #[test]
    fn rich_eq_stable_ignores_mime_order() {
        // Sender side may emit formats in any order; receiver may store in
        // a different order. Stable compare must not care.
        let order_a = vec![
            ClipboardFormat::from_text("text/html", "<p>x</p>"),
            ClipboardFormat::from_text("text/rtf", r"{\rtf1 x}"),
        ];
        let order_b = vec![
            ClipboardFormat::from_text("text/rtf", r"{\rtf1 x}"),
            ClipboardFormat::from_text("text/html", "<p>x</p>"),
        ];
        assert!(rich_eq_stable("hello", &order_a, "hello", &order_b));
    }
}
