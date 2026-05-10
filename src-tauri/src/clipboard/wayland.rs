/// Clipboard backend using wl-clipboard-rs (wlr-data-control protocol).
/// Used on Wayland with compositors that support wlr-data-control (KDE, Sway, Hyprland).
///
/// Uses polling with get_contents (not subprocess spawning), so no flickering.
use super::common::{self, ClipboardContent};
use crate::protocol::{ClipboardBlob, ClipboardFormat};
use crate::state::AppState;
use crate::transport::Transport;
use std::io::Read;
use std::thread;
use std::time::Duration;
use tauri::AppHandle;
use wl_clipboard_rs::copy::{MimeSource, MimeType as CopyMimeType, Options as CopyOptions, Source};
use wl_clipboard_rs::paste::{
    get_contents, get_mime_types, ClipboardType, Error as PasteError,
    MimeType as PasteMimeType, Seat,
};

/// Hard cap on bytes we'll read from the clipboard for an image probe.
/// A 4K uncompressed BMP is ~33 MB; 64 MB is generous headroom.
const MAX_CLIPBOARD_IMAGE_READ_BYTES: u64 = 64 * 1024 * 1024;

/// Image MIME types we know how to decode, in preference order.
/// PNG first because it's lossless and the most commonly offered by browsers.
const IMAGE_MIME_PRIORITY: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/bmp",
    "image/x-bmp",
    "image/tiff",
    "image/gif",
];

/// Vector image MIMEs — checked *before* the raster `IMAGE_MIME_PRIORITY` so
/// when a source app offers both (e.g. Inkscape: image/svg+xml + a rasterised
/// image/png fallback), we pick the higher-fidelity vector representation.
/// Bytes pass through verbatim; receivers re-stock under the same MIME.
const VECTOR_IMAGE_MIME_PRIORITY: &[&str] = &["image/svg+xml"];

/// Rich-text MIME types we relay verbatim alongside the plain text. These
/// carry formatted content (HTML/RTF) that destination apps can pick up
/// instead of the plain text — the difference between pasting from Word
/// into Word and getting formatting vs getting raw text.
///
/// Strict allowlist: vendor-specific blobs like
/// `application/x-qt-windows-mime;value="Native"`,
/// `chromium/x-renderer-taint`, `org.chromium.web-custom-data`, and
/// `text/_moz_htmlcontext` are never probed — they're either OS-internal,
/// app-internal metadata, or duplicate of the plain-text version, and
/// shipping them would just bloat the wire format.
const RICH_TEXT_MIME_PRIORITY: &[&str] = &["text/html", "text/rtf"];

/// Returns true if `offered` contains a MIME that matches `prefix`, allowing
/// for trailing parameters (e.g. `text/html;charset=utf-8` matches the prefix
/// `text/html`). Some apps include the charset suffix; the canonical form is
/// the bare type.
fn offered_contains_prefix(offered: &std::collections::HashSet<String>, prefix: &str) -> bool {
    offered.iter().any(|m| m == prefix || m.starts_with(&format!("{};", prefix)))
}

/// Hard cap on bytes we'll read from a single rich-text representation.
/// HTML / RTF from real apps (Word, browsers) are usually well under 1 MB
/// even with embedded images, but Word can attach big metadata blobs.
/// 16 MB is well above expected values and well below the 64 MB transport
/// per-message cap.
const MAX_RICH_TEXT_READ_BYTES: u64 = 16 * 1024 * 1024;

/// Check if wlr-data-control is available by attempting a paste.
pub fn is_available() -> bool {
    match get_contents(ClipboardType::Regular, Seat::Unspecified, PasteMimeType::Text) {
        Ok(_) => true,
        Err(PasteError::NoSeats) | Err(PasteError::NoMimeType) => true,
        Err(PasteError::MissingProtocol { .. }) => false,
        Err(_) => true,
    }
}

fn read_clipboard_text() -> Option<String> {
    match get_contents(ClipboardType::Regular, Seat::Unspecified, PasteMimeType::Text) {
        Ok((mut pipe, _mime)) => {
            let mut text = String::new();
            if pipe.read_to_string(&mut text).is_ok() && !text.is_empty() {
                Some(text)
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

fn read_clipboard_files() -> Option<Vec<String>> {
    match get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Specific("text/uri-list"),
    ) {
        Ok((mut pipe, _mime)) => {
            let mut data = String::new();
            if pipe.read_to_string(&mut data).is_ok() && !data.is_empty() {
                let uris: Vec<String> = data
                    .lines()
                    .filter(|l| !l.starts_with('#') && !l.is_empty())
                    .map(|l| l.trim().to_string())
                    .collect();
                if !uris.is_empty() {
                    Some(uris)
                } else {
                    None
                }
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

/// Probe for vector image formats (SVG) and pass the bytes through verbatim
/// without raster decode. Called from `read_clipboard_image` before the raster
/// MIME loop, so vector representations win over rasterised companions when
/// both are offered.
fn read_clipboard_vector_image(
    offered: &std::collections::HashSet<String>,
) -> Option<ClipboardBlob> {
    let mime = VECTOR_IMAGE_MIME_PRIORITY
        .iter()
        .copied()
        .find(|m| offered.contains(*m))?;

    let mut pipe = match get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Specific(mime),
    ) {
        Ok((p, _)) => p,
        Err(e) => {
            tracing::debug!("clipboard vector image read failed for {}: {}", mime, e);
            return None;
        }
    };

    let mut buf = Vec::new();
    if (&mut pipe)
        .take(MAX_CLIPBOARD_IMAGE_READ_BYTES + 1)
        .read_to_end(&mut buf)
        .is_err()
    {
        return None;
    }
    if buf.is_empty() {
        return None;
    }
    if buf.len() as u64 > MAX_CLIPBOARD_IMAGE_READ_BYTES {
        tracing::warn!(
            "Clipboard {} exceeds {} byte read cap; skipping.",
            mime,
            MAX_CLIPBOARD_IMAGE_READ_BYTES
        );
        return None;
    }

    common::build_image_blob(buf, mime)
}

/// Read an image from the clipboard if one is offered. Vector MIMEs (SVG)
/// pass through verbatim; raster MIMEs are decoded and re-encoded to PNG so
/// peers receive a uniform wire format for raster images. Returns None if no
/// image MIME is offered, the data couldn't be decoded, or the encoded blob
/// exceeds the wire-size cap.
fn read_clipboard_image() -> Option<ClipboardBlob> {
    let offered = match get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
        Ok(set) => set,
        Err(_) => return None,
    };

    // Prefer vector representations when available — verbatim pass-through.
    if let Some(blob) = read_clipboard_vector_image(&offered) {
        return Some(blob);
    }

    let mime = IMAGE_MIME_PRIORITY
        .iter()
        .copied()
        .find(|m| offered.contains(*m))?;

    let mut pipe = match get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Specific(mime),
    ) {
        Ok((p, _)) => p,
        Err(e) => {
            tracing::debug!("clipboard image read failed for {}: {}", mime, e);
            return None;
        }
    };

    // Read with a hard cap so a runaway source can't exhaust memory.
    let mut buf = Vec::new();
    if (&mut pipe)
        .take(MAX_CLIPBOARD_IMAGE_READ_BYTES + 1)
        .read_to_end(&mut buf)
        .is_err()
    {
        return None;
    }
    if buf.is_empty() {
        return None;
    }
    if buf.len() as u64 > MAX_CLIPBOARD_IMAGE_READ_BYTES {
        tracing::warn!(
            "Clipboard {} exceeds {} byte read cap; skipping.",
            mime,
            MAX_CLIPBOARD_IMAGE_READ_BYTES
        );
        return None;
    }

    common::normalize_image_blob_from_bytes(buf, mime)
}

/// Read alternate text formats (HTML, RTF) alongside the primary plain text.
/// Returns None if the OS clipboard offers nothing more than plain text — in
/// which case the caller should fall through to the plain-text path.
fn read_clipboard_rich() -> Option<(String, Vec<ClipboardFormat>)> {
    let offered = match get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
        Ok(set) => set,
        Err(_) => return None,
    };

    // Collect any rich-text formats actually advertised by the source.
    // Prefix-match so apps that offer `text/html;charset=utf-8` (rare but
    // permitted by the spec) are still picked up under the canonical
    // `text/html` mime in the ClipboardFormat we emit.
    let mut formats: Vec<ClipboardFormat> = Vec::new();
    for prefix in RICH_TEXT_MIME_PRIORITY {
        if !offered_contains_prefix(&offered, prefix) {
            continue;
        }
        // Pass the actual offered MIME to wlr — it serves bytes by exact match.
        let actual: &str = if offered.contains(*prefix) {
            *prefix
        } else {
            match offered
                .iter()
                .find(|m| m.starts_with(&format!("{};", prefix)))
            {
                Some(m) => m.as_str(),
                None => continue,
            }
        };
        let mut pipe = match get_contents(
            ClipboardType::Regular,
            Seat::Unspecified,
            PasteMimeType::Specific(actual),
        ) {
            Ok((p, _)) => p,
            Err(e) => {
                tracing::debug!("clipboard rich read failed for {}: {}", actual, e);
                continue;
            }
        };
        let mut buf = Vec::new();
        if (&mut pipe)
            .take(MAX_RICH_TEXT_READ_BYTES + 1)
            .read_to_end(&mut buf)
            .is_err()
        {
            continue;
        }
        if buf.is_empty() {
            continue;
        }
        if buf.len() as u64 > MAX_RICH_TEXT_READ_BYTES {
            tracing::warn!(
                "Clipboard {} exceeds {} byte read cap; skipping format.",
                actual,
                MAX_RICH_TEXT_READ_BYTES
            );
            continue;
        }
        // text/html and text/rtf are UTF-8 (and 7-bit ASCII for RTF). If the
        // bytes don't decode as UTF-8 we drop the format rather than send
        // potentially corrupt text — the receiver wouldn't know what to do.
        match String::from_utf8(buf) {
            Ok(s) => formats.push(ClipboardFormat::from_text(*prefix, s)),
            Err(e) => {
                tracing::warn!("Clipboard {} did not decode as UTF-8: {}", actual, e);
            }
        }
    }

    if formats.is_empty() {
        return None;
    }

    // We need a primary plain text alongside the rich formats. If the source
    // didn't advertise text/plain, fall back to an empty string — receivers
    // still get the formatted content, and apps that can only paste plain
    // text will get an empty paste rather than a half-formatted one.
    let text = read_clipboard_text().unwrap_or_default();
    Some((text, formats))
}

fn read_clipboard() -> ClipboardContent {
    if let Some(files) = read_clipboard_files() {
        return ClipboardContent::Files(files);
    }
    if let Some(blob) = read_clipboard_image() {
        return ClipboardContent::Image(blob);
    }
    if let Some((text, formats)) = read_clipboard_rich() {
        return ClipboardContent::Rich { text, formats };
    }
    if let Some(text) = read_clipboard_text() {
        return ClipboardContent::Text(text);
    }
    ClipboardContent::None
}

fn write_text(_app: &AppHandle, text: String) -> Result<(), String> {
    let opts = CopyOptions::new();
    opts.copy(Source::Bytes(text.into_bytes().into()), CopyMimeType::Text)
        .map_err(|e| format!("wl-clipboard-rs copy failed: {}", e))
}

fn write_files(_app: &AppHandle, files: Vec<String>) -> Result<(), String> {
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

    // Advertise both text/uri-list and x-special/gnome-copied-files so GTK file
    // managers (Nautilus) and others recognise a file paste rather than a text
    // paste. The `copy` field in x-special/gnome-copied-files distinguishes a
    // copy from a cut — we always use "copy".
    let uri_list = format!("{}\n", uris.join("\n"));
    let gnome_copied = format!("copy\n{}", uris.join("\n"));

    let sources = vec![
        MimeSource {
            source: Source::Bytes(uri_list.into_bytes().into()),
            mime_type: CopyMimeType::Specific("text/uri-list".to_string()),
        },
        MimeSource {
            source: Source::Bytes(gnome_copied.into_bytes().into()),
            mime_type: CopyMimeType::Specific("x-special/gnome-copied-files".to_string()),
        },
    ];

    let opts = CopyOptions::new();
    opts.copy_multi(sources)
        .map_err(|e| format!("wl-clipboard-rs copy files failed: {}", e))
}

fn write_image(_app: &AppHandle, blob: &ClipboardBlob) -> Result<(), String> {
    let bytes = blob.raw_bytes()?;
    let opts = CopyOptions::new();
    opts.copy(
        Source::Bytes(bytes.into()),
        CopyMimeType::Specific(blob.mime_type.clone()),
    )
    .map_err(|e| format!("wl-clipboard-rs copy image failed: {}", e))
}

/// Write plain text + alternate format representations (text/html, text/rtf, …)
/// as a single clipboard offering, so the destination app can pick whichever
/// MIME it understands best — matching the buffet the source originally had.
fn write_rich(_app: &AppHandle, text: &str, formats: &[ClipboardFormat]) -> Result<(), String> {
    let mut sources: Vec<MimeSource> = Vec::with_capacity(1 + formats.len());
    sources.push(MimeSource {
        source: Source::Bytes(text.as_bytes().to_vec().into()),
        mime_type: CopyMimeType::Text,
    });
    for f in formats {
        let bytes = f.raw_bytes()?;
        sources.push(MimeSource {
            source: Source::Bytes(bytes.into()),
            mime_type: CopyMimeType::Specific(f.mime_type.clone()),
        });
    }

    let opts = CopyOptions::new();
    opts.copy_multi(sources)
        .map_err(|e| format!("wl-clipboard-rs copy rich failed: {}", e))
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

pub fn set_clipboard_rich(app: &AppHandle, text: String, formats: Vec<ClipboardFormat>) {
    common::set_clipboard_rich_with_ignore(app, text, formats, write_rich);
}

pub fn read_text(_app: &AppHandle) -> Result<String, String> {
    read_clipboard_text().ok_or_else(|| "No text in clipboard".to_string())
}

pub fn write_text_direct(app: &AppHandle, text: String) -> Result<(), String> {
    write_text(app, text)
}

pub fn start_monitor(app_handle: AppHandle, state: AppState, transport: Transport) {
    thread::spawn(move || {
        tracing::info!("Starting Wayland clipboard monitor (wlr-data-control polling)");

        let mut last_content = ClipboardContent::None;

        loop {
            if state.is_shutdown() {
                tracing::info!("Wayland clipboard monitor shutting down.");
                break;
            }

            let current_content = read_clipboard();

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

            thread::sleep(Duration::from_millis(500));
        }
    });
}
