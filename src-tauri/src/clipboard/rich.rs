/// OS-specific HTML/RTF clipboard reads/writes for the `plugin` backend
/// (X11/Windows/macOS). The wlroots Wayland and GNOME-extension backends
/// have their own format-aware paths and don't use this module.
///
/// X11 is intentionally omitted — supporting rich-text on X11 would require
/// a third selection-owner thread alongside `tauri-plugin-clipboard` and
/// `arboard`, with all the lazy-paste / `SelectionRequest` complexity that
/// brings. X11 keeps plain-text + files + images via the existing paths.
use crate::protocol::ClipboardFormat;

/// Read all rich-text formats currently on the OS clipboard. Returns an
/// empty Vec if the platform doesn't support rich-text reads (X11) or
/// nothing is on the clipboard.
pub fn read_clipboard_rich_formats() -> Vec<ClipboardFormat> {
    #[cfg(target_os = "windows")]
    {
        windows::read_all()
    }
    #[cfg(target_os = "macos")]
    {
        macos::read_all()
    }
    #[cfg(all(target_os = "linux", not(target_os = "windows")))]
    {
        // X11 is intentionally unsupported. The `plugin` backend on X11
        // keeps text/files/images through tauri-plugin-clipboard + arboard.
        Vec::new()
    }
}

/// Write plain text plus alternate formats onto the OS clipboard as a
/// single atomic offering, so the destination app can pick the best
/// representation.
pub fn write_clipboard_rich(text: &str, formats: &[ClipboardFormat]) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        windows::write_all(text, formats)
    }
    #[cfg(target_os = "macos")]
    {
        macos::write_all(text, formats)
    }
    #[cfg(all(target_os = "linux", not(target_os = "windows")))]
    {
        let _ = (text, formats);
        Err("rich-text clipboard write not supported on X11".to_string())
    }
}

// ── Windows ────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows {
    use crate::protocol::ClipboardFormat;
    use clipboard_win::{
        formats::{Html, RawData, Unicode},
        raw, Clipboard, Getter, Setter, SysResult,
    };

    /// 16 MB cap matches the wlroots backend; protects against pathological
    /// HTML/RTF without artificially limiting real-world Word/browser content.
    const MAX_RICH_TEXT_BYTES: usize = 16 * 1024 * 1024;

    /// Number of times to retry opening the clipboard. tauri-plugin-clipboard
    /// runs its own monitor that can briefly hold the lock; same retry budget
    /// the image path uses.
    const ATTEMPTS: usize = 10;

    fn rtf_format_id() -> Option<u32> {
        raw::register_format("Rich Text Format").map(|f| f.get())
    }

    /// Read both HTML and RTF (if present) from a single clipboard open so
    /// the snapshot is consistent — otherwise a fast-changing clipboard
    /// could give us HTML from one copy event and RTF from the next.
    pub fn read_all() -> Vec<ClipboardFormat> {
        let mut out = Vec::new();
        let _clip = match Clipboard::new_attempts(ATTEMPTS) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("clipboard-win open failed: {}", e);
                return out;
            }
        };

        // HTML: clipboard-win's `Html` Getter handles the CF_HTML byte-offset
        // header for us, returning the HTML fragment as a String.
        let mut html_buf = String::new();
        match Html.read_clipboard(&mut html_buf) {
            Ok(_) if !html_buf.is_empty() => {
                if html_buf.len() > MAX_RICH_TEXT_BYTES {
                    tracing::warn!(
                        "Clipboard HTML ({} bytes) exceeds {} byte cap; skipping format.",
                        html_buf.len(),
                        MAX_RICH_TEXT_BYTES
                    );
                } else {
                    out.push(ClipboardFormat::from_text("text/html", html_buf));
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!("Html getter returned: {}", e);
            }
        }

        if let Some(rtf_id) = rtf_format_id() {
            let mut rtf_buf: Vec<u8> = Vec::new();
            match RawData(rtf_id).read_clipboard(&mut rtf_buf) {
                Ok(_) if !rtf_buf.is_empty() => {
                    if rtf_buf.len() > MAX_RICH_TEXT_BYTES {
                        tracing::warn!(
                            "Clipboard RTF ({} bytes) exceeds {} byte cap; skipping format.",
                            rtf_buf.len(),
                            MAX_RICH_TEXT_BYTES
                        );
                    } else {
                        // RTF is 7-bit ASCII per spec; if Word emits something
                        // outside that, drop the format rather than send corrupt
                        // text — receiver wouldn't know what to do with it.
                        match String::from_utf8(rtf_buf) {
                            Ok(s) => out.push(ClipboardFormat::from_text("text/rtf", s)),
                            Err(e) => {
                                tracing::warn!("RTF did not decode as UTF-8: {}", e);
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!("RTF getter returned: {}", e);
                }
            }
        }

        out
    }

    /// Write plain text + rich formats atomically. SetClipboardData calls
    /// inside a single OpenClipboard / EmptyClipboard / CloseClipboard pair
    /// publish all formats as a single offering — destination apps get the
    /// full set and pick whichever they understand.
    pub fn write_all(text: &str, formats: &[ClipboardFormat]) -> Result<(), String> {
        const MAX_ATTEMPTS: u32 = 6;
        let mut last_err: Option<String> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match try_write(text, formats) {
                Ok(()) => {
                    if attempt > 1 {
                        tracing::info!(
                            "Clipboard rich write succeeded on attempt {}",
                            attempt
                        );
                    }
                    return Ok(());
                }
                Err(e) => {
                    last_err = Some(e.clone());
                    if attempt < MAX_ATTEMPTS {
                        let backoff_ms = 50_u64 * (1 << (attempt - 1)).min(8);
                        tracing::warn!(
                            "Clipboard rich write attempt {}/{} failed: {}. Retrying in {} ms",
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
        Err(last_err.unwrap_or_else(|| "rich write failed for unknown reason".to_string()))
    }

    fn try_write(text: &str, formats: &[ClipboardFormat]) -> Result<(), String> {
        let _clip = Clipboard::new_attempts(ATTEMPTS)
            .map_err(|e| format!("clipboard-win open: {}", e))?;
        // Empty first so the rich offering replaces whatever was there;
        // calling set_clipboard_data without empty will fail.
        raw::empty().map_err(|e| format!("EmptyClipboard: {}", e))?;

        // Plain text must be set first via Unicode (CF_UNICODETEXT) so apps
        // that only consume plain text get something. `Setter<str>` takes
        // `&str` so we pass `text` directly, not `&text`.
        Unicode
            .write_clipboard(text)
            .map_err(|e| format!("CF_UNICODETEXT: {}", e))?;

        let rtf_id = rtf_format_id();
        for f in formats {
            let bytes = f.raw_bytes()?;
            match f.mime_type.as_str() {
                "text/html" => {
                    // Html setter takes the unwrapped fragment and adds the
                    // CF_HTML header for us.
                    let html_str = std::str::from_utf8(&bytes)
                        .map_err(|e| format!("text/html not UTF-8: {}", e))?;
                    Html.write_clipboard(html_str)
                        .map_err(|e| format!("CF_HTML: {}", e))?;
                }
                "text/rtf" => {
                    let id = rtf_id.ok_or_else(|| {
                        "couldn't register Rich Text Format atom".to_string()
                    })?;
                    RawData(id)
                        .write_clipboard(bytes.as_slice())
                        .map_err(|e| format!("CF_RTF: {}", e))?;
                }
                other => {
                    tracing::debug!(
                        "Skipping unsupported rich-text MIME on Windows: {}",
                        other
                    );
                }
            }
        }
        let _: SysResult<()> = Ok(());
        Ok(())
    }
}

// ── macOS ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    use crate::protocol::ClipboardFormat;
    use objc2::rc::Retained;
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSArray, NSData, NSString};

    /// 16 MB cap, same as Windows / wlroots.
    const MAX_RICH_TEXT_BYTES: usize = 16 * 1024 * 1024;

    fn pasteboard() -> Retained<NSPasteboard> {
        NSPasteboard::generalPasteboard()
    }

    /// Read HTML and RTF if present. Single pasteboard handle keeps the
    /// snapshot consistent across both reads.
    pub fn read_all() -> Vec<ClipboardFormat> {
        let pb = pasteboard();
        let mut out = Vec::new();

        for (uti, mime) in [("public.html", "text/html"), ("public.rtf", "text/rtf")] {
            let uti_ns = NSString::from_str(uti);
            let data: Option<Retained<NSData>> = pb.dataForType(&uti_ns);
            let Some(data) = data else { continue };
            let len = data.length();
            if len == 0 {
                continue;
            }
            if len > MAX_RICH_TEXT_BYTES {
                tracing::warn!(
                    "Clipboard {} ({} bytes) exceeds {} byte cap; skipping format.",
                    mime,
                    len,
                    MAX_RICH_TEXT_BYTES
                );
                continue;
            }
            match String::from_utf8(data.to_vec()) {
                Ok(s) => out.push(ClipboardFormat::from_text(mime, s)),
                Err(e) => {
                    tracing::warn!("Clipboard {} did not decode as UTF-8: {}", mime, e);
                }
            }
        }

        out
    }

    /// Write plain text + rich formats atomically. NSPasteboard's
    /// `clearContents` then `setData:forType:` for each MIME publishes them
    /// as a single ownership change — destination apps see the full buffet.
    ///
    /// **Declaration order matters.** NSPasteboard treats the first type in
    /// `declareTypes:owner:` as the canonical type. Apps like TextEdit iterate
    /// every available type and pick the richest they understand, but apps
    /// like Pages prefer the canonical one — so we put rich formats first
    /// and plain text last, otherwise Pages pastes as plain even though HTML
    /// is on the clipboard.
    pub fn write_all(text: &str, formats: &[ClipboardFormat]) -> Result<(), String> {
        let pb = pasteboard();

        let mut types_vec: Vec<Retained<NSString>> = Vec::new();
        for f in formats {
            let uti = match f.mime_type.as_str() {
                "text/rtf" => "public.rtf",
                "text/html" => "public.html",
                other => {
                    tracing::debug!("Skipping unsupported rich-text MIME on macOS: {}", other);
                    continue;
                }
            };
            types_vec.push(NSString::from_str(uti));
        }
        // Plain text last so it acts as the fallback for plain-only consumers
        // without becoming the canonical type when richer formats are present.
        types_vec.push(NSString::from_str("public.utf8-plain-text"));
        let types_array = NSArray::from_retained_slice(&types_vec);

        unsafe {
            pb.clearContents();
            pb.declareTypes_owner(&types_array, None);
        }

        // Plain text first so apps that only consume text/plain still work.
        let text_uti = NSString::from_str("public.utf8-plain-text");
        let text_data = NSData::with_bytes(text.as_bytes());
        let ok = pb.setData_forType(Some(&text_data), &text_uti);
        if !ok {
            return Err("setData:forType: returned false for plain text".to_string());
        }

        for f in formats {
            let uti = match f.mime_type.as_str() {
                "text/html" => "public.html",
                "text/rtf" => "public.rtf",
                _ => continue,
            };
            let bytes = f.raw_bytes()?;
            let uti_ns = NSString::from_str(uti);
            let data = NSData::with_bytes(&bytes);
            let ok = pb.setData_forType(Some(&data), &uti_ns);
            if !ok {
                tracing::warn!("setData:forType: returned false for {}", uti);
            }
        }

        Ok(())
    }
}
