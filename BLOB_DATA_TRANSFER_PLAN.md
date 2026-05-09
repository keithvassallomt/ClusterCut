# Clipboard Rich-Data Transfer

> **Status**: 0.3.0 will ship image clipboard sync (implemented) **plus** rich-text format sync (HTML / RTF — in progress). The GNOME extension changes for both ride in a single v4.0 release so EGO only has to verify one submission. This doc has the retrospective for the image work and the roadmap for the rich-text work.

## Part 1 — Image clipboard transfer (implemented, 0.3.0)

### What you can do now

Copy an image — for example with right-click → "Copy Image" in a browser, or from a screenshot tool, or out of an image editor — and it appears on your peers' clipboards, ready to paste into Word, Preview, GIMP, Paint, etc. Works on every clipboard backend: X11, Wayland (KDE/Sway/Hyprland), Wayland GNOME (via the ClusterCut extension), Windows, and macOS. The History view shows a thumbnail with dimensions and size.

### Wire format

`ClipboardPayload` gained an optional `blob` field; everything else is unchanged.

```rust
pub struct ClipboardPayload {
    pub id: String,
    pub text: String,
    #[serde(default)] pub files: Option<Vec<FileMetadata>>,
    #[serde(default)] pub blob: Option<ClipboardBlob>,   // ← new in 0.3.0
    pub timestamp: u64,
    pub sender: String,
    pub sender_id: String,
}

pub struct ClipboardBlob {
    pub mime_type: String,            // always "image/png" today; the field exists so other MIMEs can join later
    pub data: String,                 // base64-encoded bytes — see "lessons" below
    #[serde(default)] pub width: Option<u32>,
    #[serde(default)] pub height: Option<u32>,
}
```

`#[serde(default)]` keeps things forward-compatible: an older 0.2.x peer that doesn't know the field just deserialises it as `None` and ignores it.

The sender always **normalises to `image/png`** before broadcasting — re-encoding from whatever source format the OS clipboard offers (RGBA from arboard, JPEG from a browser, etc.). Receivers see one canonical wire format and don't need a translation matrix between every source MIME and every receiver platform.

A 10 MB cap on the encoded PNG protects against huge clipboard images saturating the cluster; over that, the sender silently drops the broadcast.

### Per-backend implementation

| Backend | Source / sink | How |
|---|---|---|
| **X11 / Windows / macOS** | `tauri-plugin-clipboard` (text + files) **plus** `arboard` (images) | `tauri-plugin-clipboard` doesn't expose non-text MIMEs, so we run `arboard` as a parallel image-only shim in the same monitor thread. Reads via `arboard::Clipboard::get_image()` (RGBA), encoded to PNG. Writes via `arboard::Clipboard::set_image()` which puts CF_DIBV5 + CF_DIB on Windows, NSPasteboardTypePNG + TIFF on macOS, image/png on X11 |
| **Wayland wlroots** (KDE / Sway / Hyprland) | `wl-clipboard-rs` directly | Already supports arbitrary MIMEs. We enumerate via `paste::get_mime_types`, pick the first match from a priority list (PNG → JPEG → WebP → BMP → TIFF → GIF), read with `paste::get_contents`, decode/re-encode to PNG via the `image` crate. Writes use `Source::Bytes(data)` with `MimeType::Specific("image/png")` |
| **Wayland GNOME** | ClusterCut GNOME extension over D-Bus (existing `Clipboard2` interface) | Extension v4.0 added three methods (`GetMimetypes`, `ReadBlob`, `WriteBlob`) and a `BlobChanged` signal — additive on `Clipboard2`, fully backwards compatible. JS uses `St.Clipboard.get_content(mime, callback)` and `St.Clipboard.set_content(mime, GLib.Bytes)`. Old apps + new extension and new apps + old extension both keep working — only image clipboard requires both ends to be on 4.0 |

The dispatch order in every backend is **files → image → text**: a `text/uri-list` paste (file copy from a file manager) wins over an image probe so the file-transfer flow we spent so long getting right is unaffected.

### Frontend / UX

History items render an `<img>` thumbnail (max 12 rem tall, contained) with a caption: `Image • W×H • size`. The bytes ride inside the `clipboard-change` event; on the JS side we `atob()` once at receive time and turn the result into a `URL.createObjectURL`, which the `<img>` references. URLs are revoked on history-delete and on the slice eviction past 50 items.

`ManualSyncModal` (the FAB-driven manual-receive flow) shows the same thumbnail in its Receive column when a pending image is queued.

### Lessons learned during real-world testing

A handful of subtle bugs only surfaced once two real machines were sharing images. Each fix is captured by a test or a comment, but they're worth remembering because they're easy to fall back into:

1. **Don't let `serde_json` encode `Vec<u8>` as a JSON integer array.** Default behaviour is `[123,45,67,…]` — about 3.5 chars per byte. A 5 MB blob becomes ~18 MB on the wire, and that's *before* the encrypted ciphertext is wrapped in `Message::Clipboard(Vec<u8>)` and serialised again. Solution: `ClipboardBlob.data` is a base64 `String`, not `Vec<u8>`. Caps the bloat at 1.33×.
2. **The transport's `read_to_end` had a 10 MB cap** that was fine for text but immediately rejected even modest images. Bumped to 64 MB.
3. **`SendStream::stopped()` is the right way to wait for delivery, not a fixed sleep.** `send_message` used to `sleep(500ms)` after `finish()`, which raced multi-MB images: the function returned and the connection was torn down while bytes were still in flight, surfacing as "connection lost" on the receiver. `stopped()` resolves once the peer ACKs all stream data, with a fallback drain sleep if that future doesn't yield.
4. **Windows' `arboard::set_image` makes multiple `SetClipboardData` calls in sequence** and any one of them can fail with `ERROR_CLIPBOARD_NOT_OPEN` (1418) if another clipboard-aware process — most likely `tauri-plugin-clipboard`'s own monitor — grabs the lock between calls. Wrapped the whole `Clipboard::new + set_image` in a 6-attempt retry with 50→400 ms exponential backoff. The RGBA buffer is borrowed via `Cow::Borrowed` between attempts so we don't re-clone multi-megabyte pixel data per try.
5. **The receiver's loop-dedup signature has to match the sender's.** Originally the receiver computed its `content_signature` as `FILES:…` for files, otherwise `text.clone()` — for a blob-only payload (`text=""`, `files=None`) that collapsed to an empty string, which matched the empty initial `last_clipboard_content`, and the handler returned early before the BLOB HANDLING block ever ran. Symptom: bytes arrive, no errors, image silently drops on the floor. Fix: extracted a single `payload_signature()` helper used by both sender's broadcast dedup and receiver's loop guard, so they can't drift again.
6. **Don't hold the entire clipboard data path on a single PartialEq comparison of multi-megabyte blobs.** Dedup uses a fingerprint (mime + base64 length + first/last 16 bytes hex), not raw byte equality.

---

## Part 2 — Rich-text formats (HTML, RTF) — also 0.3.0

### Why this matters: what happens when you copy from Word

Right now ClusterCut only carries `text/plain`. If you copy `Hello **bold** world` from Microsoft Word and paste it into Word on a peer, you get `Hello bold world` — the formatting is gone. To understand why, look at what the OS clipboard actually contains when you copy from Word:

- `text/plain` — `Hello bold world`
- `text/html` (or Windows' `CF_HTML`) — a fragment of HTML with `<strong>` / `<span style=…>` plus a lot of Office-specific metadata
- `text/rtf` (or `CF_RTF`) — a full RTF document `{\rtf1 \ansi … {\b bold} …}`
- (sometimes) `image/png` — a screenshot of the rendered text, for apps that can only paste images

When the user pastes, **the destination app picks** which of those formats to consume. Word picks RTF or HTML, gets full formatting. Notepad picks plain text, drops formatting. Firefox's address bar picks plain text. Pages on macOS picks RTF. The clipboard is a multi-format buffet, and ClusterCut is currently only carrying the plain bread.

To fix this, ClusterCut needs to carry **multiple representations of the same copy event** and re-stock them all on the receiver, so the destination app sees the same buffet the source did.

### Wire-format change

The minimal, backwards-compatible extension is to add a list of additional formats to `ClipboardPayload`, each a `(mime, encoding, bytes)` triple:

```rust
pub struct ClipboardPayload {
    pub id: String,
    pub text: String,                         // text/plain — primary, always present, unchanged semantics
    #[serde(default)] pub files: Option<Vec<FileMetadata>>,
    #[serde(default)] pub blob: Option<ClipboardBlob>,
    #[serde(default)] pub formats: Option<Vec<ClipboardFormat>>,   // ← new
    pub timestamp: u64,
    pub sender: String,
    pub sender_id: String,
}

pub struct ClipboardFormat {
    pub mime_type: String,            // "text/html", "text/rtf", "application/x-vnd.oasis.opendocument…", etc.
    pub data: String,                 // UTF-8 (text/*) or base64 (binary), based on `binary` flag
    pub binary: bool,                 // true → data is base64-encoded bytes
}
```

Key properties:

- `text` is still the primary. A 0.3.0 peer that ignores the new `formats` field still gets a usable plain-text paste from a 0.4.0 sender. Older peers fall back to `text/plain` semantics — exactly the same regression as today, no worse.
- The list can hold any MIME, not just HTML/RTF. Once the multi-format channel exists, future formats (SVG, ODF chunks, etc.) are additive.
- The image case (`blob`) stays its own field rather than being merged into `formats` — image-only copies (no text alongside) have a clean shape, and the receiver's image-write retry loop already exists. We *could* merge it later but there's no urgency.

A short hand-rolled signature in `payload_signature()` already exists for blobs; for `formats` it'd be `FORMATS:<mime1>:<len1>;<mime2>:<len2>;…` — same idea, deterministic and small.

### Per-platform format mapping

The MIME types each OS exposes don't agree, so ClusterCut needs a translation layer. Based on the pattern already used for files (`text/uri-list` ↔ `x-special/gnome-copied-files`) the rules are:

| Logical format | Linux (X11 / Wayland) | Windows | macOS |
|---|---|---|---|
| Plain text | `text/plain;charset=utf-8` (or `UTF8_STRING`) | `CF_UNICODETEXT` | `public.utf8-plain-text` |
| HTML | `text/html` | **`CF_HTML`** — wraps the HTML fragment in a Microsoft-specific header with `Version:`, `StartHTML:`, `EndHTML:`, `StartFragment:`, `EndFragment:` byte offsets | `public.html` |
| RTF | `text/rtf` | `CF_RTF` (or `Rich Text Format`) | `public.rtf` |

The asymmetric one is Windows' `CF_HTML`. Linux and macOS use raw HTML bytes; Windows expects (and produces) a header like:

```
Version:0.9
StartHTML:00000099
EndHTML:00000299
StartFragment:00000131
EndFragment:00000260
<html>
<body>
<!--StartFragment-->Hello <strong>bold</strong> world<!--EndFragment-->
</body>
</html>
```

Going Linux → Windows we have to compute and prepend that header before writing. Going Windows → Linux we have to strip it. The byte offsets are recomputed because they reference into the same buffer; can't be left as-is. **Two helpers** — `wrap_cf_html(html: &str) -> Vec<u8>` and `strip_cf_html(bytes: &[u8]) -> Option<String>` — handle the round-trip; both sides of any cross-platform sync hit one of them.

RTF is uniform: same bytes everywhere, no wrapping required.

### Per-backend implementation sketch

The four backends, with the additions:

| Backend | Read | Write |
|---|---|---|
| **Windows / macOS** | Add MIME-aware reads to the existing arboard-shim thread. arboard 3.x doesn't expose `get_html` or arbitrary MIMEs, so this is direct calls into `clipboard-win` (Windows) and `objc2`/`NSPasteboard` (macOS) — both small, well-documented APIs | Same wrappers, `set_html` / `set_rtf`. Windows write needs `wrap_cf_html` (compute the `Version:`/`StartHTML:`/`EndHTML:`/`StartFragment:`/`EndFragment:` byte-offset header). macOS just sets `public.html` / `public.rtf` |
| **X11** | **Out of scope** for rich-text. Plain-text + files + images keep working unchanged via tauri-plugin-clipboard + the arboard image shim — no regression. X11 selection ownership requires a persistent owner thread responding to `SelectionRequest` events, and arboard's API doesn't let you add MIME targets to its existing owner; layering a third selection-owner alongside `tauri-plugin-clipboard` and `arboard` is high-risk. X11 is also a declining platform, so the cost/benefit doesn't justify the work | **Out of scope** as above. **Documentation reminder**: when user-facing docs are written, explicitly note that rich-text (HTML/RTF) clipboard sync is not supported on X11 — plain text, files, and images sync as normal |
| **Wayland wlroots** | `wl_clipboard_rs::paste::get_mime_types` already used; add `text/html` and `text/rtf` to the priority probe, alongside the image MIMEs | `copy::Source::Bytes` with multiple `MimeSource` entries (already do this for `text/uri-list` + `x-special/gnome-copied-files`); add `text/html` and `text/rtf` rows |
| **GNOME extension** | Add a generic `ReadAllFormats(in as mimes, out a(say) blobs)` D-Bus method, or extend the existing `BlobChanged` to carry a list. Folded into the **v4.0** release alongside the image blob methods so EGO only verifies once — v4.0 isn't shipped/submitted until both feature sets are in | Generic `WriteAllFormats(in a(say))` — JS calls `clipboard.set_content(type, mime, bytes)` for each. Echo prevention reuses the existing `_ignoreUntil` window |

### Smart capture rules

Some apps put **eight or more representations** on the clipboard. Most are redundant or actively useless to sync. A small allowlist keeps the wire payload sane:

- Always: `text/plain` (the existing path)
- Add when present: `text/html`, `text/rtf`, `text/uri-list` (existing files path), one image MIME (existing blob path)
- **Skip**: vendor-specific blobs like `application/x-qt-windows-mime;value="Native"`, `chromium/x-renderer-taint`, `org.chromium.web-custom-data`, screenshot duplicates, `text/_moz_htmlcontext`, etc. They're either 1) entirely OS-internal, 2) huge metadata Word attaches that doesn't help paste behaviour, or 3) duplicate of the plain-text version

### History UI implications

A history entry that carries `text` + `text/html` shouldn't show two cards — it's one copy event with multiple representations. Render decisions:

- If `formats` includes `text/html`, render the HTML in a sandboxed `<div>` (with content-security cleanup) for preview, instead of showing `text` plain.
- A small "rich" badge next to the size, hover for "plain text, HTML, RTF" tooltip.
- Plain-only clipboard items continue to render as today.

### Suggested phasing

1. Wire-format plumbing: `ClipboardFormat` struct, `formats` field on `ClipboardPayload`, signature update, round-trip tests. No backend wiring yet — same shape as Phase 1 of the image rollout.
2. Wayland wlroots backend — easiest, reuses the existing MIME probe path. Validate with a Word document copied via Wayland-GNOME → KDE.
3. Windows + macOS backend — direct calls into `clipboard-win` and `NSPasteboard` for HTML/RTF reads and writes, plus the `wrap_cf_html`/`strip_cf_html` helpers for Windows' Microsoft-specific HTML wrapper. **X11 is intentionally out of scope** for rich-text — see the per-backend table above for why. X11 keeps text/files/images via the existing paths, no regression.
4. GNOME extension — add the rich-text format methods to the **same v4.0** D-Bus interface that already carries the image blob methods. EGO submission for v4.0 is held until this lands so the extension goes through verification once for the whole 0.3.0 cycle.
5. Frontend — render rich previews, distinguish plain-vs-rich items.
6. Smart-capture allowlist + edge-case polish — Word, Apple Mail, Outlook, Notion, VS Code (each puts wildly different things on the clipboard).

---

## Part 3 — Other deferred work

These are smaller and lower-priority than rich-text. Listed roughly in expected order.

### 3.1 Multi-format simultaneous preservation

Closely related to rich-text and somewhat solved by the design above: once `formats` exists, "preserve all the things the source had" is a question of how aggressive the smart-capture allowlist is. v1 sticks to text/html + text/rtf + the existing image/files paths; v2 could expand to e.g. `image/svg+xml` alongside a rasterised `image/png` so vector-aware apps get the SVG and others get the bitmap.

### 3.2 Animated GIFs

Currently, an `image/gif` source gets decoded to `RgbaImage` (frame 0 only) and re-encoded to PNG. Animation is lost. To keep animation, `ClipboardBlob.mime_type` would need to be allowed to stay `image/gif` and the data sent verbatim — skipping the decode/re-encode round-trip. The receiver would need to set `image/gif` on the local clipboard (Wayland and macOS handle this fine; Windows has no native GIF clipboard format and would need to fall back to a raster preview).

### 3.3 Audio / video clipboard blobs

These are rare but real (Audacity, video editors, some screen-recording tools). Mechanically the same as image blobs — bytes + MIME — with the catch that file sizes can be large enough that the inline-blob path doesn't make sense. See §3.5.

### 3.4 Vector formats (SVG)

`image/svg+xml` is text-shaped (UTF-8 XML), so it just needs to be added to the format allowlist. The wire size is usually small. Receiver can either set `image/svg+xml` directly on the clipboard (if the destination app understands it) or rasterise to PNG as a fallback. Mostly trivial; held for v2 because few apps actually consume SVG from the clipboard.

### 3.5 Streaming or deferred-download for very large blobs

The current 10 MB inline cap is plenty for screenshots and typical web images, but it cuts off:
- large editor exports (e.g. a poster from Affinity Designer)
- multi-second audio/video clips
- huge formatted documents

Path: when a payload exceeds the inline cap, route it through the existing **file transfer** channel (the `FileStreamHeader` / `clustercut-file` ALPN) with a synthetic filename like `clipboard-image-<timestamp>.png`. The receiver materialises a temp file, sets the OS clipboard to point at it (or, for image data, decodes and sets `set_image`). The plumbing exists; this is mainly a UX question — large copies become "downloads" rather than instantaneous.

### 3.6 Format negotiation between peers

Today both ends just trust that the wire shape matches. With more formats in flight, having a tiny capability handshake — "I can write CF_HTML, I can write text/rtf, I cannot write image/svg+xml" — would let senders avoid shipping formats the receiver can't restock. mDNS or the existing pair handshake are both candidate places to advertise. Worth doing once the format list grows past three or four entries.

---

## 0.3.0 phase status

### Image clipboard sync (Part 1)

| # | Phase | Status |
|---|---|---|
| 1 | Wire protocol + dedup foundation | ✅ complete |
| 2 | Backend B — Wayland wlroots (`wl-clipboard-rs`) | ✅ complete |
| 3 | Backend A — X11 / Windows / macOS (`arboard` shim) | ✅ complete |
| 4 | Backend C — GNOME extension + D-Bus (`Clipboard2` v4.0 — image blob methods) | ✅ complete (extension v4.0 not yet submitted to EGO; held until rich-text is in) |
| 5 | Frontend / UX (history thumbnails, notifications) | ✅ complete |

### Rich-text formats (Part 2)

| # | Phase | Status |
|---|---|---|
| 1 | Wire-format plumbing (`ClipboardFormat`, `formats` field, signature, round-trip tests) | ✅ complete |
| 2 | Backend B — Wayland wlroots (extend MIME probe + multi-MimeSource write) | ✅ complete |
| 3 | Backend A — Windows + macOS (HTML/RTF reads/writes; CF_HTML wrap/strip helpers). **X11 intentionally out of scope** — too costly for a declining platform | ⬜ not started |
| 4 | Backend C — GNOME extension v4.0 (add format methods to the same v4.0 release as image blobs) | ⬜ not started |
| 5 | Frontend — rich previews, plain-vs-rich badge | ⬜ not started |
| 6 | Smart-capture allowlist + cross-app edge cases (Word, Apple Mail, Outlook, Notion, VS Code) | ⬜ not started |

### Release polish (after Parts 1 + 2 are both in)

| # | Phase | Status |
|---|---|---|
| 7 | CHANGELOG, metainfo, **docs (note rich-text not supported on X11)**, end-to-end testing, EGO submission for extension v4.0 | ⏸ deferred until rich-text lands |
