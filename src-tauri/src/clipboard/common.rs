use crate::crypto;
use crate::protocol::{ClipboardBlob, ClipboardFormat, ClipboardPayload, FileMetadata, Message};
use crate::state::{AppState, ClipboardBlobMetadata};
use crate::transport::Transport;
use std::thread;
use tauri::{AppHandle, Emitter, Manager};

use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex};

/// Encoded-byte threshold above which a clipboard image switches from the
/// inline `Message::Clipboard` path to the descriptor + file-transfer path
/// (§3.3 in `BLOB_DATA_TRANSFER_PLAN.md`). At or below this size, bytes
/// ride inline; above it, the sender writes a temp file, registers it,
/// and broadcasts a descriptor for the receiver to fetch via the
/// `clustercut-file` ALPN stream.
pub const MAX_CLIPBOARD_IMAGE_WIRE_BYTES: usize = 10 * 1024 * 1024;

/// Absolute upper bound on clipboard-image bytes. Anything larger drops with
/// a warning — both the inline path and the descriptor + file-transfer path
/// would still in principle work (file transfer has no per-message cap), but
/// arbitrarily large clipboard payloads hit memory pressure on the sender's
/// PNG encode and the receiver's accumulator, and are vanishingly rare in
/// real-world clipboard use. 500 MB is several times bigger than the
/// largest plausible image clipboard payload (a 4K uncompressed BMP is
/// ~33 MB, an 8K PNG screenshot tops out around ~150 MB).
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 500 * 1024 * 1024;

/// Threshold above which a "Receiving large clipboard…" notification fires on
/// the receiver side. A multi-MB inline transfer takes a perceptible amount
/// of time on the network and the user might paste mid-transfer otherwise.
pub const LARGE_CLIPBOARD_BLOB_NOTIFY_THRESHOLD: usize = 10 * 1024 * 1024;

/// MIME types whose bytes pass through verbatim instead of being decoded
/// and re-encoded to PNG. Three reasons to preserve a source MIME:
///
/// - **Vector**: SVG (`image/svg+xml`). PNG-normalising loses the vector
///   representation entirely and gives downstream apps a flattened raster
///   instead of an editable shape.
/// - **Animated**: GIF (`image/gif`). PNG-normalising loses the animation
///   (only frame 0 survives the RGBA round-trip).
/// - **Wire-size sane for photos**: JPEG (`image/jpeg`). PNG-normalising a
///   30 MB JPEG photo decodes RGBA and re-encodes lossless PNG, ballooning
///   to ~150 MB which exceeds the 60 MB wire cap and silently drops the
///   sync. Keeping JPEG verbatim preserves the source's compression
///   choice and never inflates.
///
/// Bytes ride the wire under the source MIME and the receiver writes them
/// verbatim under that same MIME. Whether a destination app *paints* the
/// SVG, *animates* the GIF, or *renders* the JPEG is the destination's
/// concern.
pub fn is_passthrough_image_mime(mime: &str) -> bool {
    matches!(mime, "image/svg+xml" | "image/gif" | "image/jpeg")
}

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
        // Descriptor mode (§3.3 large blob): `data` is empty and the unique
        // identifier is the parent payload id, which sender + receiver share
        // verbatim. Hash that plus mime + total_size so a re-broadcast of
        // the same descriptor matches and a different one doesn't.
        if let Some(fetch_id) = blob.fetch_id.as_ref() {
            let total = blob.total_size.unwrap_or(0);
            return format!("BLOBDESC:{}:{}:{}", blob.mime_type, fetch_id, total);
        }
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

    if png_bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        tracing::warn!(
            "Clipboard image PNG ({} bytes) exceeds {} byte absolute cap; skipping.",
            png_bytes.len(),
            MAX_CLIPBOARD_IMAGE_BYTES
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

/// Build a `ClipboardBlob` from raw clipboard bytes, branching between
/// passthrough (verbatim) and raster (PNG-normalise) paths based on the
/// source MIME. Passthrough MIMEs ride the wire as-is — receivers re-stock
/// them under the same MIME, so e.g. SVG copied from Inkscape pastes as SVG
/// into a vector-aware destination on the receiving peer, and an animated
/// GIF retains animation through to the receiver.
///
/// Passthrough blobs have `width = height = None`. For SVG that's because
/// vector formats don't carry intrinsic raster dimensions; for GIF we
/// could parse the LSD header but skip it deliberately because the
/// TIRI-fix's `image_blob_eq_stable` falls back to byte-exact comparison
/// when dims are absent, and passthrough bytes round-trip stably through
/// every OS clipboard layer (SVG XML, GIF byte stream — both preserved
/// verbatim by NSPasteboard / Win32 registered formats / wlroots), so
/// byte-exact compare doesn't bounce.
#[cfg(target_os = "linux")]
pub fn build_image_blob(bytes: Vec<u8>, source_mime: &str) -> Option<ClipboardBlob> {
    if is_passthrough_image_mime(source_mime) {
        if bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
            tracing::warn!(
                "Clipboard {} ({} bytes) exceeds {} byte absolute cap; skipping.",
                source_mime,
                bytes.len(),
                MAX_CLIPBOARD_IMAGE_BYTES
            );
            return None;
        }
        return Some(ClipboardBlob::from_bytes(source_mime, &bytes, None, None));
    }
    normalize_image_blob_from_bytes(bytes, source_mime)
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

/// Wall-clock timestamp of the most recent `IGNORED_CONTENT` set. Combined
/// with `IGNORED_TTL`, this auto-expires a stale echo guard if the expected
/// echo never arrives — for example, if our `set_clipboard_image` write
/// failed silently, or the user copied something different on the local
/// clipboard before our intended echo could bounce back through the
/// monitor poll.
///
/// Without this, a stuck IGNORED would generate "variant differs" /
/// "mime differs" misses on every subsequent clipboard event indefinitely,
/// each one looping the content back to peers. Observed in the wild after
/// an SVG paste followed by unrelated copies — the SVG IGNORED stayed
/// referenced for the entire session.
pub static IGNORED_SET_AT: Lazy<Arc<Mutex<Option<std::time::Instant>>>> =
    Lazy::new(|| Arc::new(Mutex::new(None)));

/// How long an IGNORED guard remains valid after being set. Generous enough
/// for the slowest legitimate echo (Windows `arboard::set_image` with full
/// retry budget can take ~1.6 s; 10 s leaves comfortable headroom on slow
/// disks / clipboard managers / etc). Shorter than the timescales at which
/// stale state is likely to drive observable bugs.
pub const IGNORED_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// Set both the IGNORED content and its timestamp atomically. All
/// `set_clipboard_*_with_ignore` helpers funnel through this so the two
/// halves can never drift.
fn set_ignored(content: ClipboardContent) {
    {
        let mut ignored = IGNORED_CONTENT.lock().unwrap();
        *ignored = content;
    }
    {
        let mut at = IGNORED_SET_AT.lock().unwrap();
        *at = Some(std::time::Instant::now());
    }
}

/// Clear the IGNORED guard and its timestamp atomically.
fn clear_ignored(ignored: &mut ClipboardContent) {
    *ignored = ClipboardContent::None;
    let mut at = IGNORED_SET_AT.lock().unwrap();
    *at = None;
}

/// One-line summary of a `ClipboardContent` for log lines. Keep it compact
/// so an event chain (`Set IGNORED → Check → MATCH/MISS → Broadcast?`) reads
/// inline without wrapping.
pub fn describe_content(c: &ClipboardContent) -> String {
    match c {
        ClipboardContent::None => "None".to_string(),
        ClipboardContent::Text(s) => format!("Text(len={})", s.len()),
        ClipboardContent::Files(f) => format!("Files(count={})", f.len()),
        ClipboardContent::Image(b) => {
            let dims = match (b.width, b.height) {
                (Some(w), Some(h)) => format!("{}x{}", w, h),
                _ => "?".to_string(),
            };
            format!(
                "Image(mime={}, decoded={}, dims={})",
                b.mime_type,
                b.decoded_len(),
                dims
            )
        }
        ClipboardContent::Rich { text, formats } => {
            let mimes: Vec<&str> = formats.iter().map(|f| f.mime_type.as_str()).collect();
            format!("Rich(text_len={}, formats=[{}])", text.len(), mimes.join(", "))
        }
    }
}

/// Stable equivalence for image blobs across an OS-clipboard round-trip.
///
/// The IGNORED guard is a *one-shot* check whose only job is to suppress our
/// own echo: we wrote an image to the OS clipboard and want to ignore the
/// next read of "an image". The OS layer mangles both the bytes (RGBA → DIB
/// → RGBA → re-encoded PNG produces different bytes for the same pixels)
/// **and** the reported dimensions on Windows (CF_DIB header padding /
/// alignment can shift width or height by a handful of pixels), so a strict
/// `(mime, dims)` check produces false negatives that make the receiver
/// re-broadcast every image it accepts. Same-mime image-vs-image is
/// therefore treated as our own echo unconditionally.
///
/// False-suppression cost: if the user copies a *different* image within
/// one poll cycle of receiving one, the new image is suppressed for that
/// cycle and broadcast on the next (~500 ms later). Acceptable.
fn image_blob_eq_stable(a: &ClipboardBlob, b: &ClipboardBlob) -> bool {
    a.mime_type == b.mime_type
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

/// Outcome of an IGNORED-guard check. The caller uses this to decide both
/// whether to broadcast AND whether to advance `last_content` — bundling the
/// two stops the bug where a successful echo-suppression leaves
/// `last_content` stale, so the next poll sees the same content as "new"
/// and broadcasts it (which was the loop-back observed in production).
pub enum EchoVerdict {
    /// Real new content — broadcast it. Caller should also set
    /// `last_content = current`.
    Process,
    /// Echo of our own write — IGNORED guard matched and was cleared. Don't
    /// broadcast, but caller MUST set `last_content = current` so the next
    /// poll doesn't re-flag the same content as "new" via the `IGNORED is
    /// None` branch.
    Echo,
    /// Nothing to do — content unchanged from last seen, or empty. Caller
    /// leaves `last_content` alone.
    NoChange,
}

/// Process a clipboard read result through the dedup/feedback-loop logic.
///
/// Logging contract for the [Echo] tag (`info!`):
///   `[Echo] Check: ignored=… current=… -> MATCH (clearing guard)` — guard fired
///   `[Echo] Check: ignored=… current=… -> MISS (reason=…)` — guard didn't fire
///   `[Echo] Triggering loop-back broadcast — IGNORED check missed`
///     (only when a MISS leads to an actual broadcast)
/// A MATCH should never be followed by a broadcast for the same content.
pub fn should_process_content(
    current_content: &ClipboardContent,
    last_content: &ClipboardContent,
) -> EchoVerdict {
    if *current_content == ClipboardContent::None {
        return EchoVerdict::NoChange;
    }

    // Expire stale IGNORED guard before the check. If the timestamp is
    // older than IGNORED_TTL, the expected echo never bounced — most likely
    // because the user copied something else locally before the OS clipboard
    // round-trip completed. Keeping the stale guard would manufacture
    // spurious "variant differs" misses for every subsequent clipboard
    // event indefinitely (the bug that surfaced after an SVG paste).
    {
        let mut at = IGNORED_SET_AT.lock().unwrap();
        if let Some(t) = *at {
            if t.elapsed() > IGNORED_TTL {
                let mut ignored = IGNORED_CONTENT.lock().unwrap();
                if !matches!(*ignored, ClipboardContent::None) {
                    tracing::info!(
                        "[Echo] IGNORED guard expired after {:?} — clearing stale {} guard",
                        t.elapsed(),
                        describe_content(&ignored)
                    );
                }
                *ignored = ClipboardContent::None;
                *at = None;
            }
        }
    }

    let mut verdict = EchoVerdict::NoChange;
    {
        let mut ignored = IGNORED_CONTENT.lock().unwrap();
        let ign_desc = describe_content(&ignored);
        let cur_desc = describe_content(current_content);
        match &*ignored {
            ClipboardContent::None => {
                if current_content != last_content {
                    verdict = EchoVerdict::Process;
                }
            }
            ClipboardContent::Text(ign_text) => {
                if let ClipboardContent::Text(curr_text) = current_content {
                    if curr_text == ign_text {
                        tracing::info!("[Echo] Check: ignored={} current={} -> MATCH (clearing guard)", ign_desc, cur_desc);
                        clear_ignored(&mut ignored);
                        verdict = EchoVerdict::Echo;
                    } else if current_content != last_content {
                        tracing::info!("[Echo] Check: ignored={} current={} -> MISS (reason=text_differs)", ign_desc, cur_desc);
                        verdict = EchoVerdict::Process;
                    }
                } else if current_content != last_content {
                    tracing::info!("[Echo] Check: ignored={} current={} -> MISS (reason=variant_differs; ignored is Text but current isn't)", ign_desc, cur_desc);
                    verdict = EchoVerdict::Process;
                }
            }
            ClipboardContent::Files(ign_files) => {
                if let ClipboardContent::Files(curr_files) = current_content {
                    if curr_files == ign_files {
                        tracing::info!("[Echo] Check: ignored={} current={} -> MATCH (clearing guard)", ign_desc, cur_desc);
                        clear_ignored(&mut ignored);
                        verdict = EchoVerdict::Echo;
                    } else if current_content != last_content {
                        tracing::info!("[Echo] Check: ignored={} current={} -> MISS (reason=files_differ)", ign_desc, cur_desc);
                        verdict = EchoVerdict::Process;
                    }
                } else if current_content != last_content {
                    tracing::info!("[Echo] Check: ignored={} current={} -> MISS (reason=variant_differs; ignored is Files but current isn't)", ign_desc, cur_desc);
                    verdict = EchoVerdict::Process;
                }
            }
            ClipboardContent::Image(ign_blob) => {
                if let ClipboardContent::Image(curr_blob) = current_content {
                    if image_blob_eq_stable(curr_blob, ign_blob) {
                        tracing::info!("[Echo] Check: ignored={} current={} -> MATCH (clearing guard)", ign_desc, cur_desc);
                        clear_ignored(&mut ignored);
                        verdict = EchoVerdict::Echo;
                    } else if current_content != last_content {
                        tracing::info!("[Echo] Check: ignored={} current={} -> MISS (reason=mime_differs)", ign_desc, cur_desc);
                        verdict = EchoVerdict::Process;
                    }
                } else if current_content != last_content {
                    tracing::info!("[Echo] Check: ignored={} current={} -> MISS (reason=variant_differs; ignored is Image but current isn't)", ign_desc, cur_desc);
                    verdict = EchoVerdict::Process;
                }
            }
            ClipboardContent::Rich {
                text: ign_text,
                formats: ign_formats,
            } => {
                if let ClipboardContent::Rich { text, formats } = current_content {
                    if rich_eq_stable(ign_text, ign_formats, text, formats) {
                        tracing::info!("[Echo] Check: ignored={} current={} -> MATCH (clearing guard)", ign_desc, cur_desc);
                        clear_ignored(&mut ignored);
                        verdict = EchoVerdict::Echo;
                    } else if current_content != last_content {
                        tracing::info!("[Echo] Check: ignored={} current={} -> MISS (reason=rich_differs)", ign_desc, cur_desc);
                        verdict = EchoVerdict::Process;
                    }
                } else if current_content != last_content {
                    tracing::info!("[Echo] Check: ignored={} current={} -> MISS (reason=variant_differs; ignored is Rich but current isn't)", ign_desc, cur_desc);
                    verdict = EchoVerdict::Process;
                }
            }
        }
    }
    if matches!(verdict, EchoVerdict::Process) {
        tracing::info!(
            "[Echo] Triggering loop-back broadcast — IGNORED check missed; current={}",
            describe_content(current_content)
        );
    }
    verdict
}

/// File extension for the §3.3 temp-file written when a large clipboard blob
/// is staged for the file-transfer path. Used in `temp_downloads/<id>.<ext>`.
/// Picked from MIME so the file is visually meaningful if it leaks past the
/// startup `clear_cache` (which it shouldn't — but if it does, having the
/// right extension makes manual cleanup easier).
fn extension_for_clipboard_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "image/webp" => "webp",
        "image/bmp" | "image/x-bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => "bin",
    }
}

/// Stage a large clipboard blob's raw bytes to disk in `temp_downloads/<id>.<ext>`
/// and register the entry in `state.local_clipboard_blobs`. Used by the §3.3
/// descriptor path: when an image's encoded size exceeds the inline cap, the
/// sender writes the bytes here so peers can fetch them via the existing
/// `clustercut-file` ALPN stream once they receive the descriptor on
/// `Message::Clipboard`.
fn stage_clipboard_blob_temp_file(
    app: &AppHandle,
    state: &AppState,
    msg_id: &str,
    mime_type: &str,
    width: Option<u32>,
    height: Option<u32>,
    bytes: &[u8],
) -> Result<(), String> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("resolve cache dir: {}", e))?
        .join("temp_downloads");
    std::fs::create_dir_all(&cache_dir).map_err(|e| format!("create temp dir: {}", e))?;

    let ext = extension_for_clipboard_mime(mime_type);
    let path = cache_dir.join(format!("{}.{}", msg_id, ext));

    std::fs::write(&path, bytes).map_err(|e| format!("write temp file {:?}: {}", path, e))?;

    let metadata = ClipboardBlobMetadata {
        path: path.clone(),
        mime_type: mime_type.to_string(),
        width,
        height,
        total_size: bytes.len() as u64,
    };

    {
        let mut map = state.local_clipboard_blobs.lock().unwrap();
        map.insert(msg_id.to_string(), metadata);
    }

    Ok(())
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

            // Inline-vs-descriptor decision (§3.3). Decoded byte size is
            // what would actually ride the wire as base64 inside
            // Message::Clipboard; over the threshold, switch to the
            // descriptor + file-transfer path.
            let decoded_len = blob.decoded_len();
            let payload_obj = if decoded_len <= MAX_CLIPBOARD_IMAGE_WIRE_BYTES {
                ClipboardPayload {
                    id: msg_id,
                    text: String::new(),
                    files: None,
                    blob: Some(blob),
                    formats: None,
                    timestamp: ts,
                    sender: hostname,
                    sender_id: local_id,
                }
            } else {
                // Descriptor path. Write the bytes to a temp file under the
                // same `temp_downloads` dir the file-transfer path uses on
                // the receiver side, register in `local_clipboard_blobs`, and
                // emit a descriptor-only `ClipboardBlob` on the wire. Peers
                // will respond with `Message::FileRequest` and pull the bytes
                // over the `clustercut-file` ALPN.
                let raw_bytes = match blob.raw_bytes() {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::error!(
                            "Failed to decode clipboard blob bytes for descriptor path: {}",
                            e
                        );
                        return;
                    }
                };
                match stage_clipboard_blob_temp_file(
                    app_handle,
                    state,
                    &msg_id,
                    &blob.mime_type,
                    blob.width,
                    blob.height,
                    &raw_bytes,
                ) {
                    Ok(()) => {
                        tracing::info!(
                            "[ClipboardBlob] Large blob detected ({} bytes, mime={}) — broadcasting descriptor (id={})",
                            raw_bytes.len(),
                            blob.mime_type,
                            msg_id
                        );
                        ClipboardPayload {
                            id: msg_id.clone(),
                            text: String::new(),
                            files: None,
                            blob: Some(ClipboardBlob::descriptor(
                                blob.mime_type.clone(),
                                msg_id,
                                raw_bytes.len() as u64,
                                blob.width,
                                blob.height,
                            )),
                            formats: None,
                            timestamp: ts,
                            sender: hostname,
                            sender_id: local_id,
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to stage large clipboard blob to temp file: {} (size={} bytes, mime={})",
                            e,
                            raw_bytes.len(),
                            blob.mime_type
                        );
                        return;
                    }
                }
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
        let content = ClipboardContent::Text(text_clone.clone());
        tracing::info!("[Echo] Set IGNORED guard -> {}", describe_content(&content));
        set_ignored(content);

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
        let content = ClipboardContent::Files(paths_clone.clone());
        tracing::info!("[Echo] Set IGNORED guard -> {}", describe_content(&content));
        set_ignored(content);

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
        let content = ClipboardContent::Rich {
            text: text_clone.clone(),
            formats: formats_clone.clone(),
        };
        tracing::info!("[Echo] Set IGNORED guard -> {}", describe_content(&content));
        set_ignored(content);

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
        let content = ClipboardContent::Image(blob_clone.clone());
        tracing::info!("[Echo] Set IGNORED guard -> {}", describe_content(&content));
        set_ignored(content);

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
            fetch_id: None,
            total_size: None,
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
    fn image_blob_eq_stable_matches_when_dimensions_differ() {
        // The Windows CF_DIB round-trip can shift reported dims by a handful
        // of pixels (header padding artifacts). Same-mime images are now
        // treated as our own echo regardless — the IGNORED guard is one-shot
        // so the false-suppression cost is at most one poll cycle.
        let a = blob("image/png", "X", Some(1280), Some(720));
        let b = blob("image/png", "X", Some(640), Some(480));
        assert!(image_blob_eq_stable(&a, &b));
    }

    #[test]
    fn image_blob_eq_stable_distinguishes_different_mime() {
        let a = blob("image/png", "X", Some(100), Some(100));
        let b = blob("image/jpeg", "X", Some(100), Some(100));
        assert!(!image_blob_eq_stable(&a, &b));
    }

    #[test]
    fn image_blob_eq_stable_matches_same_mime_without_dimensions() {
        // No dims present (legacy/hand-built blobs) still match if the mime
        // matches — same one-shot rationale.
        let a = blob("image/png", "X", None, None);
        let b = blob("image/png", "Y", None, None);
        assert!(image_blob_eq_stable(&a, &b));
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

    // ─── §3.3 — descriptor signature is stable, fetch_id-keyed ─────────────

    #[test]
    fn signature_for_descriptor_blob_uses_fetch_id() {
        let descriptor = ClipboardBlob::descriptor(
            "image/png",
            "abc-123",
            25_000_000,
            Some(1920),
            Some(1080),
        );
        let payload = ClipboardPayload {
            id: "abc-123".to_string(),
            text: String::new(),
            files: None,
            blob: Some(descriptor),
            formats: None,
            timestamp: 0,
            sender: "host".to_string(),
            sender_id: "device".to_string(),
        };
        let sig = payload_signature(&payload);
        assert!(sig.starts_with("BLOBDESC:image/png:abc-123:"), "got: {}", sig);
        assert!(sig.contains("25000000"), "size missing from sig: {}", sig);
    }

    #[test]
    fn signature_descriptor_distinguishes_different_fetch_ids() {
        let a = ClipboardBlob::descriptor("image/png", "id-a", 100, None, None);
        let b = ClipboardBlob::descriptor("image/png", "id-b", 100, None, None);
        let pa = ClipboardPayload {
            id: "id-a".to_string(),
            text: String::new(),
            files: None,
            blob: Some(a),
            formats: None,
            timestamp: 0,
            sender: "h".to_string(),
            sender_id: "d".to_string(),
        };
        let pb = ClipboardPayload {
            id: "id-b".to_string(),
            text: String::new(),
            files: None,
            blob: Some(b),
            formats: None,
            timestamp: 0,
            sender: "h".to_string(),
            sender_id: "d".to_string(),
        };
        assert_ne!(payload_signature(&pa), payload_signature(&pb));
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
