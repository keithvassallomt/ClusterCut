/// OS-specific HTML/RTF clipboard reads/writes for the `plugin` backend
/// (X11/Windows/macOS). The wlroots Wayland and GNOME-extension backends
/// have their own format-aware paths and don't use this module.
///
/// Also covers vector-image (SVG) clipboard reads/writes for the same
/// reason — arboard's `get_image()`/`set_image()` is RGBA-only so we can't
/// preserve SVG bytes through it.
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

/// Read a passthrough image (SVG vector or animated GIF) from the OS
/// clipboard if one is present. Returns `(mime, bytes)`. The plugin backend
/// calls this *before* arboard's RGBA probe so passthrough representations
/// beat raster fallbacks when sources offer both. arboard would otherwise
/// lose the original format entirely (its API is RGBA-only — SVG can't
/// round-trip through it; GIF loses animation).
pub fn read_clipboard_passthrough_image() -> Option<(String, Vec<u8>)> {
    #[cfg(target_os = "windows")]
    {
        windows::read_passthrough_image()
    }
    #[cfg(target_os = "macos")]
    {
        macos::read_passthrough_image()
    }
    #[cfg(all(target_os = "linux", not(target_os = "windows")))]
    {
        None
    }
}

/// Write a passthrough image (SVG / animated GIF) to the OS clipboard
/// verbatim under its source MIME. The plugin backend's
/// `set_clipboard_image` branches on `blob.mime_type` and calls this for
/// passthrough MIMEs; raster MIMEs continue through arboard.
pub fn write_clipboard_passthrough_image(mime: &str, bytes: &[u8]) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        windows::write_passthrough_image(mime, bytes)
    }
    #[cfg(target_os = "macos")]
    {
        macos::write_passthrough_image(mime, bytes)
    }
    #[cfg(all(target_os = "linux", not(target_os = "windows")))]
    {
        let _ = (mime, bytes);
        Err("passthrough-image clipboard write not supported on X11".to_string())
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

    fn html_format_id() -> Option<u32> {
        raw::register_format("HTML Format").map(|f| f.get())
    }

    fn rtf_format_id() -> Option<u32> {
        raw::register_format("Rich Text Format").map(|f| f.get())
    }

    fn svg_format_id() -> Option<u32> {
        raw::register_format("image/svg+xml").map(|f| f.get())
    }

    fn gif_format_id() -> Option<u32> {
        raw::register_format("image/gif").map(|f| f.get())
    }

    /// Probe order for passthrough-image MIMEs on Windows. Each entry
    /// returns `(mime_label, format_id_resolver)`. SVG first since when both
    /// are offered, vector beats animated.
    fn passthrough_image_atoms() -> [(&'static str, fn() -> Option<u32>); 2] {
        [
            ("image/svg+xml", svg_format_id),
            ("image/gif", gif_format_id),
        ]
    }

    /// Read both HTML and RTF (if present) from a single clipboard open so
    /// the snapshot is consistent — otherwise a fast-changing clipboard
    /// could give us HTML from one copy event and RTF from the next.
    ///
    /// `IsClipboardFormatAvailable` is checked first so we only open the
    /// clipboard when rich text is actually present. The 500 ms monitor poll
    /// otherwise piles three opens (files + image + rich) onto every cycle,
    /// starving concurrent writers (e.g. arboard's set_image retries) and
    /// surfacing as ERROR_CLIPBOARD_NOT_OPEN on the setter side.
    pub fn read_all() -> Vec<ClipboardFormat> {
        let html_id = html_format_id();
        let rtf_id = rtf_format_id();

        let html_avail = html_id.map(raw::is_format_avail).unwrap_or(false);
        let rtf_avail = rtf_id.map(raw::is_format_avail).unwrap_or(false);
        if !html_avail && !rtf_avail {
            return Vec::new();
        }

        let mut out = Vec::new();
        let _clip = match Clipboard::new_attempts(ATTEMPTS) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("clipboard-win open failed: {}", e);
                return out;
            }
        };

        // HTML: clipboard-win's `Html` Getter handles the CF_HTML byte-offset
        // header for us, returning the HTML fragment as a String. `Html::new`
        // registers the CF_HTML format atom — None means the registration call
        // failed, in which case there's nothing to read.
        if html_avail {
            let mut html_buf = String::new();
            match Html::new() {
                Some(html) => match html.read_clipboard(&mut html_buf) {
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
                },
                None => {
                    tracing::debug!("Couldn't register CF_HTML format atom");
                }
            }
        }

        if let (true, Some(rtf_id)) = (rtf_avail, rtf_id) {
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
        // that only consume plain text get something. The `Setter<T>` impl is
        // `impl<T: AsRef<str>> Setter<T> for Unicode`, and `T` defaults to
        // `Sized` — passing `text: &str` directly would make `T = str` which
        // is unsized. `&text` makes `T = &str`, which is sized.
        Unicode
            .write_clipboard(&text)
            .map_err(|e| format!("CF_UNICODETEXT: {}", e))?;

        let rtf_id = rtf_format_id();
        for f in formats {
            let bytes = f.raw_bytes()?;
            match f.mime_type.as_str() {
                "text/html" => {
                    // Html setter takes the unwrapped fragment and adds the
                    // CF_HTML header for us. `Html::new` registers the format
                    // atom; None means registration failed.
                    let html_str = std::str::from_utf8(&bytes)
                        .map_err(|e| format!("text/html not UTF-8: {}", e))?;
                    let html = Html::new()
                        .ok_or_else(|| "couldn't register CF_HTML format atom".to_string())?;
                    html.write_clipboard(&html_str)
                        .map_err(|e| format!("CF_HTML: {}", e))?;
                }
                "text/rtf" => {
                    let id = rtf_id.ok_or_else(|| {
                        "couldn't register Rich Text Format atom".to_string()
                    })?;
                    RawData(id)
                        .write_clipboard(&bytes)
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

    /// Read passthrough-image bytes (SVG or GIF) from the clipboard if any
    /// of the registered passthrough atoms are present. Wrapped in the same
    /// `is_format_avail` precheck pattern as the rich-text path so we don't
    /// open the clipboard on every poll when nothing's present (Windows
    /// clipboard contention is real). Probe order matches
    /// `passthrough_image_atoms()` — SVG before GIF, so vector beats
    /// animated when both are offered.
    pub fn read_passthrough_image() -> Option<(String, Vec<u8>)> {
        // Resolve all atoms first so we can do the cheap is_format_avail
        // probe without opening the clipboard.
        let atoms: Vec<(&'static str, u32)> = passthrough_image_atoms()
            .iter()
            .filter_map(|(mime, resolver)| resolver().map(|id| (*mime, id)))
            .collect();
        let (mime, id) = atoms.iter().copied().find(|(_, id)| raw::is_format_avail(*id))?;

        let _clip = match Clipboard::new_attempts(ATTEMPTS) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("clipboard-win open failed for {} read: {}", mime, e);
                return None;
            }
        };
        let mut buf: Vec<u8> = Vec::new();
        match RawData(id).read_clipboard(&mut buf) {
            Ok(_) if !buf.is_empty() => {
                if buf.len() > MAX_RICH_TEXT_BYTES {
                    tracing::warn!(
                        "Clipboard {} ({} bytes) exceeds {} byte cap; skipping.",
                        mime,
                        buf.len(),
                        MAX_RICH_TEXT_BYTES
                    );
                    return None;
                }
                Some((mime.to_string(), buf))
            }
            _ => None,
        }
    }

    /// Write passthrough-image (SVG / GIF) bytes to the clipboard verbatim
    /// under a registered format atom matching the source MIME. Same retry
    /// pattern as the rich-text writer for clipboard-manager contention.
    pub fn write_passthrough_image(mime: &str, bytes: &[u8]) -> Result<(), String> {
        const MAX_ATTEMPTS: u32 = 6;
        let mut last_err: Option<String> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match try_write_passthrough(mime, bytes) {
                Ok(()) => {
                    if attempt > 1 {
                        tracing::info!(
                            "Clipboard {} write succeeded on attempt {}",
                            mime,
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
                            "Clipboard {} write attempt {}/{} failed: {}. Retrying in {} ms",
                            mime,
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
        Err(last_err.unwrap_or_else(|| "passthrough-image write failed for unknown reason".to_string()))
    }

    fn try_write_passthrough(mime: &str, bytes: &[u8]) -> Result<(), String> {
        let id = match mime {
            "image/svg+xml" => svg_format_id()
                .ok_or_else(|| "couldn't register image/svg+xml format atom".to_string())?,
            "image/gif" => gif_format_id()
                .ok_or_else(|| "couldn't register image/gif format atom".to_string())?,
            other => return Err(format!("unsupported passthrough MIME on Windows: {}", other)),
        };
        let _clip = Clipboard::new_attempts(ATTEMPTS)
            .map_err(|e| format!("clipboard-win open: {}", e))?;
        raw::empty().map_err(|e| format!("EmptyClipboard: {}", e))?;
        RawData(id)
            .write_clipboard(bytes)
            .map_err(|e| format!("SetClipboardData({}): {}", mime, e))?;
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

    /// Map a wire MIME to the macOS UTI used on the pasteboard.
    fn passthrough_image_uti(mime: &str) -> Option<&'static str> {
        match mime {
            // public.svg-image: registered UTI for SVG; modern macOS
            // (Big Sur+) handles it. Apps that look for the raw MIME instead
            // would need a parallel `image/svg+xml` declaration — out of
            // scope until a real-world app surfaces that limitation.
            "image/svg+xml" => Some("public.svg-image"),
            // com.compuserve.gif: registered UTI for GIF since macOS 10.x.
            // Most apps that handle animated GIFs on the pasteboard look
            // for this UTI.
            "image/gif" => Some("com.compuserve.gif"),
            _ => None,
        }
    }

    /// Probe order for passthrough-image MIMEs on macOS. SVG first since
    /// when both are offered, vector beats animated.
    const PASSTHROUGH_IMAGE_PROBE: &[(&str, &str)] = &[
        ("image/svg+xml", "public.svg-image"),
        ("image/gif", "com.compuserve.gif"),
    ];

    /// Read passthrough-image bytes (SVG / GIF) from the pasteboard. Probes
    /// each registered UTI in priority order; first one with data wins.
    pub fn read_passthrough_image() -> Option<(String, Vec<u8>)> {
        let pb = pasteboard();
        for (mime, uti) in PASSTHROUGH_IMAGE_PROBE {
            let uti_ns = NSString::from_str(uti);
            let data: Option<Retained<NSData>> = unsafe { pb.dataForType(&uti_ns) };
            let Some(data) = data else { continue };
            let len = unsafe { data.length() };
            if len == 0 {
                continue;
            }
            if len > MAX_RICH_TEXT_BYTES {
                tracing::warn!(
                    "Clipboard {} ({} bytes) exceeds {} byte cap; skipping.",
                    mime,
                    len,
                    MAX_RICH_TEXT_BYTES
                );
                continue;
            }
            let bytes = unsafe {
                let ptr = data.bytes();
                std::slice::from_raw_parts(ptr.as_ptr() as *const u8, len).to_vec()
            };
            return Some((mime.to_string(), bytes));
        }
        None
    }

    /// Write passthrough-image bytes verbatim under the appropriate macOS
    /// UTI. Atomic single-format publish via `clearContents` +
    /// `declareTypes:owner:` + `setData:forType:`.
    pub fn write_passthrough_image(mime: &str, bytes: &[u8]) -> Result<(), String> {
        let uti = passthrough_image_uti(mime)
            .ok_or_else(|| format!("unsupported passthrough MIME on macOS: {}", mime))?;
        let pb = pasteboard();
        let uti_ns = NSString::from_str(uti);
        let types_array = NSArray::from_retained_slice(&[uti_ns.clone()]);
        unsafe {
            pb.clearContents();
            pb.declareTypes_owner(&types_array, None);
        }
        let data = NSData::with_bytes(bytes);
        let ok = pb.setData_forType(Some(&data), &uti_ns);
        if !ok {
            return Err(format!(
                "setData:forType: returned false for passthrough image ({})",
                mime
            ));
        }
        Ok(())
    }
}
