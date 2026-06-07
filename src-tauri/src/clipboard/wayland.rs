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
/// Sized to match `common::MAX_CLIPBOARD_IMAGE_BYTES` so the wlroots backend
/// can see clipboard images up to the same upper bound the descriptor
/// (§3.3) path supports — anything below the 10 MB inline cap rides
/// `Message::Clipboard` directly, anything above goes via the file-transfer
/// ALPN.
const MAX_CLIPBOARD_IMAGE_READ_BYTES: u64 = 500 * 1024 * 1024;

/// Raster image MIME types we know how to decode, in preference order.
/// PNG first because it's lossless and the most commonly offered by browsers.
/// `image/gif` and `image/jpeg` are intentionally absent — both go through
/// the passthrough path below (GIF to preserve animation, JPEG to avoid
/// the 5-30× wire-size inflation that PNG re-encoding of photo content
/// causes).
const IMAGE_MIME_PRIORITY: &[&str] = &[
    "image/png",
    "image/webp",
    "image/bmp",
    "image/x-bmp",
    "image/tiff",
];

/// Passthrough image MIMEs — checked *before* the raster `IMAGE_MIME_PRIORITY`
/// so a source app that offers both passthrough and raster representations
/// (e.g. Inkscape: image/svg+xml + a rasterised image/png fallback) gives
/// the higher-fidelity passthrough representation. Bytes go on the wire
/// verbatim; receivers re-stock under the same MIME.
const PASSTHROUGH_IMAGE_MIME_PRIORITY: &[&str] =
    &["image/svg+xml", "image/gif", "image/jpeg"];

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

/// Bytes pulled for the cheap per-poll clipboard change-probe. Large enough
/// that two distinct clipboard payloads almost never share an identical prefix,
/// small enough that probing a huge selection every tick stays cheap — and,
/// crucially, closing the pipe at this size stops the source process from
/// pumping a giant selection through the compositor on every poll (the thing
/// that wedges the Wayland session when a 100 MB+ payload sits on the clipboard).
const CLIP_PROBE_BYTES: u64 = 64 * 1024;

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
    let (pipe, _mime) =
        get_contents(ClipboardType::Regular, Seat::Unspecified, PasteMimeType::Text).ok()?;
    // Bound the read at the text ceiling (+ a small margin). Reading the whole
    // of a pathological selection would balloon memory *and* — because we poll —
    // force the source to pump the entire payload through the compositor every
    // tick. Closing the pipe at the cap stops both. The +64 margin lets an
    // over-ceiling payload still come back as `len > cap`, so the wire-decision
    // routes it to the "too large" path rather than silently truncating-and-sending.
    let cap = common::MAX_CLIPBOARD_TEXT_BYTES as u64;
    let mut buf = Vec::new();
    if pipe.take(cap + 64).read_to_end(&mut buf).is_err() || buf.is_empty() {
        return None;
    }
    let over_cap = buf.len() as u64 > cap;
    match String::from_utf8(buf) {
        Ok(s) => Some(s),
        // Hitting the cap can split a multi-byte char at the boundary. Keep the
        // valid prefix — its length stays > cap, so it still classifies as too
        // large. (A genuinely non-UTF-8 selection within the cap is dropped, as
        // before.)
        Err(e) if over_cap => {
            let valid = e.utf8_error().valid_up_to();
            let mut b = e.into_bytes();
            b.truncate(valid);
            String::from_utf8(b).ok().filter(|s| !s.is_empty())
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

/// Probe for passthrough image formats (SVG, animated GIF) and pass the
/// bytes through verbatim without raster decode/re-encode. Called from
/// `read_clipboard_image` before the raster MIME loop, so passthrough
/// representations win over rasterised companions when both are offered.
fn read_clipboard_passthrough_image(
    offered: &std::collections::HashSet<String>,
) -> Option<ClipboardBlob> {
    let mime = PASSTHROUGH_IMAGE_MIME_PRIORITY
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

    // Prefer passthrough representations (SVG, animated GIF) when offered —
    // verbatim bytes, no re-encoding.
    if let Some(blob) = read_clipboard_passthrough_image(&offered) {
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

/// Pick a single representative MIME to prefix-probe for change-detection,
/// following `read_clipboard`'s priority (files → image → rich → text) so the
/// probed representation is the one the monitor would actually act on.
fn clipboard_probe_mime(offered: &std::collections::HashSet<String>) -> Option<String> {
    const PRIORITY: &[&str] = &[
        "text/uri-list",
        "image/png",
        "image/jpeg",
        "image/svg+xml",
        "image/gif",
        "image/webp",
        "image/bmp",
        "image/tiff",
        "text/html",
        "text/rtf",
        "application/rtf",
        "text/plain;charset=utf-8",
        "text/plain",
        "UTF8_STRING",
        "STRING",
    ];
    for m in PRIORITY {
        if offered.contains(*m) {
            return Some((*m).to_string());
        }
    }
    // Fall back to any remaining text type, else the lexicographically first
    // offered type, so we still produce a stable probe target.
    offered
        .iter()
        .find(|m| m.starts_with("text/"))
        .or_else(|| offered.iter().min())
        .cloned()
}

/// Read at most `CLIP_PROBE_BYTES` of `mime` from the clipboard, returning the
/// bytes read (possibly empty). Closing the pipe at the cap keeps this cheap
/// even for a multi-hundred-MB selection.
fn read_clip_prefix(mime: &str) -> Vec<u8> {
    match get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Specific(mime),
    ) {
        Ok((pipe, _)) => {
            let mut buf = Vec::new();
            let _ = pipe.take(CLIP_PROBE_BYTES).read_to_end(&mut buf);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// A cheap fingerprint of the current clipboard selection: the sorted set of
/// offered MIME types plus a bounded prefix of the representation the monitor
/// would use. `get_mime_types` transfers no content and the prefix read closes
/// the pipe early, so this stays cheap every poll regardless of payload size.
/// Returns `None` for an empty/unavailable clipboard.
///
/// Equality means "the monitor would produce the same content as last time", so
/// the poll loop can skip the full (potentially huge) read. The only blind spot
/// is two payloads that share an identical first `CLIP_PROBE_BYTES` *and* MIME
/// set — vanishingly rare for real clipboard content, and the cost of a miss is
/// one skipped sync, not a crash.
fn clipboard_fingerprint() -> Option<(Vec<String>, Vec<u8>)> {
    let offered = get_mime_types(ClipboardType::Regular, Seat::Unspecified).ok()?;
    if offered.is_empty() {
        return None;
    }
    let mut mimes: Vec<String> = offered.iter().cloned().collect();
    mimes.sort();
    let prefix = match clipboard_probe_mime(&offered) {
        Some(m) => read_clip_prefix(&m),
        None => Vec::new(),
    };
    Some((mimes, prefix))
}

pub fn start_monitor(app_handle: AppHandle, state: AppState, transport: Transport) {
    thread::spawn(move || {
        tracing::info!("Starting Wayland clipboard monitor (wlr-data-control polling)");

        let mut last_content = ClipboardContent::None;
        let mut last_fp: Option<(Vec<String>, Vec<u8>)> = None;

        loop {
            if state.is_shutdown() {
                tracing::info!("Wayland clipboard monitor shutting down.");
                break;
            }

            // Cheap change-probe first. Without this the loop re-reads the whole
            // clipboard every 500 ms; for a large selection (e.g. 100 MB+ of
            // text) that pumps the entire payload through the compositor on every
            // tick and wedges the Wayland session. Only do the full read when the
            // fingerprint actually changes.
            let fp = clipboard_fingerprint();
            if fp == last_fp {
                thread::sleep(Duration::from_millis(500));
                continue;
            }
            last_fp = fp;

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

#[cfg(test)]
mod tests {
    use super::clipboard_probe_mime;
    use std::collections::HashSet;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn probe_mime_prefers_files_over_image_and_text() {
        let s = set(&["text/plain", "image/png", "text/uri-list"]);
        assert_eq!(clipboard_probe_mime(&s).as_deref(), Some("text/uri-list"));
    }

    #[test]
    fn probe_mime_prefers_image_over_text() {
        let s = set(&["text/plain", "image/png"]);
        assert_eq!(clipboard_probe_mime(&s).as_deref(), Some("image/png"));
    }

    #[test]
    fn probe_mime_prefers_charset_text_variant() {
        let s = set(&["text/plain;charset=utf-8", "text/plain"]);
        assert_eq!(
            clipboard_probe_mime(&s).as_deref(),
            Some("text/plain;charset=utf-8")
        );
    }

    #[test]
    fn probe_mime_falls_back_to_unknown_text_type() {
        let s = set(&["text/x-weird"]);
        assert_eq!(clipboard_probe_mime(&s).as_deref(), Some("text/x-weird"));
    }

    #[test]
    fn probe_mime_none_for_empty() {
        assert_eq!(clipboard_probe_mime(&HashSet::new()), None);
    }
}
