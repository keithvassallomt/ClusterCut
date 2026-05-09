/// Clipboard backend using tauri-plugin-clipboard.
/// Used on macOS, Windows, and X11 on Linux.
///
/// Image clipboard data (PNG, JPEG, etc. that apps put on the clipboard as
/// raw bytes — e.g. "Copy Image" in a browser) is handled in parallel via
/// `arboard` because tauri-plugin-clipboard only exposes text and file reads.
/// The two crates run side-by-side on the same monitor thread; reads on all
/// three platforms (X11, Windows, macOS) are non-destructive, so the
/// existing text/file paths are unaffected if arboard is disabled or fails.
use super::common::{
    self, ClipboardContent, IGNORED_CONTENT,
};
use super::rich;
use crate::protocol::{ClipboardBlob, ClipboardFormat};
use crate::state::AppState;
use crate::transport::Transport;
use std::time::Duration;
use std::{thread, sync::mpsc};
use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard::Clipboard;

/// Cap on RGBA bytes returned by arboard. A 4K image is ~33 MB; 200 MB
/// covers up to ~7K screenshots without risking absurd allocations.
const MAX_CLIPBOARD_IMAGE_RGBA_BYTES: usize = 200 * 1024 * 1024;
/// Wire-format size cap. PNG-encoded blobs over this are dropped on send.
const MAX_CLIPBOARD_IMAGE_WIRE_BYTES: usize = 10 * 1024 * 1024;

/// Try to pull an image from the clipboard via arboard, encode to PNG, and
/// return it as a `ClipboardBlob`. Returns `None` for any failure mode —
/// arboard returns `Err` when no image is offered, which is the common case.
fn read_clipboard_image_arboard(arboard: &mut arboard::Clipboard) -> Option<ClipboardBlob> {
    let img = match arboard.get_image() {
        Ok(i) => i,
        Err(arboard::Error::ContentNotAvailable) => return None,
        Err(e) => {
            tracing::debug!("arboard get_image failed: {}", e);
            return None;
        }
    };

    let width = img.width as u32;
    let height = img.height as u32;

    if img.bytes.len() > MAX_CLIPBOARD_IMAGE_RGBA_BYTES {
        tracing::warn!(
            "Clipboard image RGBA buffer ({} bytes) exceeds {} byte cap; skipping.",
            img.bytes.len(),
            MAX_CLIPBOARD_IMAGE_RGBA_BYTES
        );
        return None;
    }

    let rgba = match image::RgbaImage::from_raw(width, height, img.bytes.into_owned()) {
        Some(r) => r,
        None => {
            tracing::warn!(
                "Clipboard image dims/bytes mismatch (w={}, h={}); skipping.",
                width,
                height
            );
            return None;
        }
    };

    let mut png_bytes = Vec::new();
    if let Err(e) = image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
    {
        tracing::warn!("Failed to PNG-encode clipboard image: {}", e);
        return None;
    }

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

/// Decode a PNG blob into RGBA and hand it to arboard, which writes the
/// platform-canonical formats (CF_DIB/CF_DIBV5 on Windows; NSPasteboardTypePNG
/// + NSPasteboardTypeTIFF on macOS; image/png on X11).
fn write_clipboard_image_arboard(_app: &AppHandle, blob: &ClipboardBlob) -> Result<(), String> {
    let format = match blob.mime_type.as_str() {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        "image/bmp" | "image/x-bmp" => image::ImageFormat::Bmp,
        "image/tiff" => image::ImageFormat::Tiff,
        "image/gif" => image::ImageFormat::Gif,
        other => return Err(format!("unsupported clipboard image MIME: {}", other)),
    };

    let bytes = blob.raw_bytes()?;
    let decoded = image::load_from_memory_with_format(&bytes, format)
        .map_err(|e| format!("decode clipboard image: {}", e))?;
    let rgba = decoded.into_rgba8();
    let width = rgba.width() as usize;
    let height = rgba.height() as usize;
    let raw = rgba.into_raw();

    // Retry with backoff. Windows in particular (error 1418 / ERROR_CLIPBOARD_NOT_OPEN)
    // can fail if tauri-plugin-clipboard's monitor or another clipboard-aware process
    // grabs the clipboard between our OpenClipboard and SetClipboardData calls.
    // arboard's internal retry is short (~250 ms total); a longer outer retry covers
    // briefly-stuck monitors. ImageData consumes its bytes, so we rebuild it per attempt
    // from the same `raw` Vec via Cow::Borrowed (cheap — no copy until consumed).
    const MAX_ATTEMPTS: u32 = 6;
    let mut last_err: Option<String> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let img_data = arboard::ImageData {
            width,
            height,
            bytes: std::borrow::Cow::Borrowed(&raw),
        };
        let result = arboard::Clipboard::new()
            .map_err(|e| format!("arboard init failed: {}", e))
            .and_then(|mut clip| {
                clip.set_image(img_data)
                    .map_err(|e| format!("arboard set_image failed: {}", e))
            });
        match result {
            Ok(()) => {
                if attempt > 1 {
                    tracing::info!("Clipboard image set succeeded on attempt {}", attempt);
                }
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e.clone());
                if attempt < MAX_ATTEMPTS {
                    let backoff_ms = 50_u64 * (1 << (attempt - 1)).min(8); // 50, 100, 200, 400, 400, then give up
                    tracing::warn!(
                        "Clipboard image set attempt {}/{} failed: {}. Retrying in {} ms",
                        attempt,
                        MAX_ATTEMPTS,
                        e,
                        backoff_ms
                    );
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "set_image failed for unknown reason".to_string()))
}

fn read_clipboard(
    app: &AppHandle,
    arboard_opt: Option<&mut arboard::Clipboard>,
) -> ClipboardContent {
    let clip = app.state::<Clipboard>();

    match clip.read_files() {
        Ok(files) => {
            if !files.is_empty() {
                return ClipboardContent::Files(files);
            }
        }
        Err(_) => {}
    }

    // Image probe sits between files and text so the canonical "Copy Image"
    // browser case is caught (no uri-list, no useful text), while the
    // existing "Copy a file" flow that does emit uri-list still wins above.
    if let Some(arb) = arboard_opt {
        if let Some(blob) = read_clipboard_image_arboard(arb) {
            return ClipboardContent::Image(blob);
        }
    }

    // Rich-text probe (HTML / RTF) — sits above plain text so when Word /
    // browsers offer both `text/plain` and `text/html` we capture both.
    // X11 returns an empty Vec from rich::read_clipboard_rich_formats so
    // the X11 plain-text path below is taken unchanged.
    let rich_formats = rich::read_clipboard_rich_formats();
    if !rich_formats.is_empty() {
        let text = clip.read_text().unwrap_or_default();
        return ClipboardContent::Rich {
            text,
            formats: rich_formats,
        };
    }

    match clip.read_text() {
        Ok(text) => {
            if !text.is_empty() {
                return ClipboardContent::Text(text);
            }
        }
        Err(_) => {}
    }

    ClipboardContent::None
}

fn write_text(app: &AppHandle, text: String) -> Result<(), String> {
    app.state::<Clipboard>()
        .write_text(text)
        .map_err(|e| e.to_string())
}

fn write_files(app: &AppHandle, files: Vec<String>) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let paths: Vec<String> = files
            .into_iter()
            .filter_map(|p| {
                let path = std::path::Path::new(&p);
                if path.is_absolute() {
                    Some(p)
                } else {
                    std::env::current_dir()
                        .ok()
                        .map(|c| c.join(path).to_string_lossy().to_string())
                }
            })
            .collect();

        if paths.is_empty() {
            return Err("No valid paths".to_string());
        }

        app.state::<Clipboard>()
            .write_files_uris(paths)
            .map_err(|e| e.to_string())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let uris: Vec<String> = files
            .into_iter()
            .filter_map(|p| {
                let path = std::path::Path::new(&p);
                let abs_path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    match std::env::current_dir().ok() {
                        Some(c) => c.join(path),
                        None => return None,
                    }
                };

                url::Url::from_file_path(abs_path)
                    .ok()
                    .map(|u| u.to_string())
            })
            .collect();

        if uris.is_empty() {
            return Err("No valid file paths convertible to URIs".to_string());
        }

        app.state::<Clipboard>()
            .write_files_uris(uris)
            .map_err(|e| e.to_string())
    }
}

pub fn set_clipboard(app: &AppHandle, text: String) {
    common::set_clipboard_with_ignore(app, text, write_text);
}

pub fn set_clipboard_paths(app: &AppHandle, paths: Vec<String>) {
    common::set_clipboard_paths_with_ignore(app, paths, write_files);
}

pub fn set_clipboard_image(app: &AppHandle, blob: ClipboardBlob) {
    common::set_clipboard_blob_with_ignore(app, blob, write_clipboard_image_arboard);
}

/// Write plain text + HTML/RTF formats. On X11 the rich module returns an
/// error; we fall back to writing plain text via tauri-plugin-clipboard so
/// the user still gets *something* — graceful degradation matches what
/// `set_clipboard_rich` in mod.rs documents.
fn write_rich(app: &AppHandle, text: &str, formats: &[ClipboardFormat]) -> Result<(), String> {
    match rich::write_clipboard_rich(text, formats) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::debug!(
                "Rich-text write unsupported on this platform ({}); falling back to plain text",
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
pub fn read_text(app: &AppHandle) -> Result<String, String> {
    app.state::<Clipboard>()
        .read_text()
        .map_err(|e| e.to_string())
}

/// Write clipboard text directly (for manual receive shortcut).
pub fn write_text_direct(app: &AppHandle, text: String) -> Result<(), String> {
    write_text(app, text)
}

pub fn start_monitor(app_handle: AppHandle, state: AppState, transport: Transport) {
    let app_handle_worker = app_handle.clone();

    let (cmd_tx, cmd_rx) = mpsc::channel::<()>();
    let (res_tx, res_rx) = mpsc::channel::<ClipboardContent>();

    // Worker Thread
    thread::spawn(move || {
        // Init arboard once for the lifetime of the worker thread. If init
        // fails (e.g. no X server), image clipboard reads are silently
        // disabled — text and file paths still work via tauri-plugin-clipboard.
        let mut arboard_clip: Option<arboard::Clipboard> = match arboard::Clipboard::new() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(
                    "arboard init failed; clipboard image reads disabled: {}",
                    e
                );
                None
            }
        };

        while cmd_rx.recv().is_ok() {
            let content = read_clipboard(&app_handle_worker, arboard_clip.as_mut());
            if res_tx.send(content).is_err() {
                break;
            }
        }
    });

    // Monitor Thread
    thread::spawn(move || {
        let mut last_content = ClipboardContent::None;

        loop {
            if state.is_shutdown() {
                tracing::info!("Clipboard monitor received shutdown signal, exiting.");
                break;
            }

            if cmd_tx.send(()).is_err() {
                tracing::error!("Clipboard worker thread died.");
                break;
            }

            let current_content = match res_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(c) => c,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    tracing::warn!(
                        "Clipboard read timed out (possible deadlock/lock). Skipping cycle."
                    );
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::error!("Clipboard worker disconnected.");
                    break;
                }
            };

            if common::should_process_content(&current_content, &last_content) {
                last_content = current_content.clone();
                common::process_clipboard_change(
                    current_content,
                    &app_handle,
                    &state,
                    &transport,
                );
            } else if current_content != ClipboardContent::None {
                // Update last_content for ignored/echo matches
                let ignored = IGNORED_CONTENT.lock().unwrap();
                if *ignored == ClipboardContent::None && current_content != last_content {
                    // Content didn't change from an ignored echo perspective
                } else if matches!(&*ignored, ClipboardContent::Text(t) if matches!(&current_content, ClipboardContent::Text(c) if c == t))
                    || matches!(&*ignored, ClipboardContent::Files(f) if matches!(&current_content, ClipboardContent::Files(c) if c == f))
                    || matches!(&*ignored, ClipboardContent::Image(b) if matches!(&current_content, ClipboardContent::Image(c) if c == b))
                {
                    last_content = current_content;
                }
            }

            thread::sleep(Duration::from_millis(500));
        }
    });
}
