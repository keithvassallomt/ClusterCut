# Blob Clipboard Data Transfer — Implementation Plan

> **Status**: Design — not yet implemented. Branch `blob-data-transfer`. Tracks GitHub issue (TBD).

## 1. Context

ClusterCut currently syncs two kinds of clipboard data: **text** and **file paths** (with on-demand file content download). It does **not** sync arbitrary in-memory clipboard blobs — most importantly, image bytes that an app writes directly to the clipboard (e.g. right-click → "Copy Image" in Firefox; "Copy" out of an image editor; OS screenshot tools).

Goal: sync image clipboard data between peers so the canonical workflow — *copy an image in Firefox on Linux, paste into Word on Windows* — just works.

## 2. Scope

### In scope (v1)

- **Sync raster image data** (PNG / JPEG / BMP / TIFF / WebP / GIF first frame) between peers.
- **Cross-platform restocking**: image arriving on Windows must be pasteable in Word/Paint; on macOS in Pages/Preview; on Linux in browsers/editors.
- **Single-image-per-paste**. The most recently copied image replaces the receiving clipboard.
- **History UI**: image clipboard items render with a thumbnail in the History view.

### Out of scope (deferred to v2 or later, but protocol must not block them)

- **HTML clipboard data** (`text/html`) and rich-text (`text/rtf`).
- **Multi-format simultaneous preservation** — if the sender's clipboard has PNG + TIFF + HTML, we ship one canonical format (PNG); the receiver re-stocks PNG only.
- **Animated GIFs** (frame 0 only on the wire if encountered as `image/gif`; full GIF support deferred).
- **Audio / video clipboard blobs**.
- **Streaming or deferred-download for very large blobs** (we cap inline blob size — see §6.4).
- **Vector formats** (`image/svg+xml` — could be added trivially since it's text-shaped, but not v1).

## 3. Architecture summary (current state)

Map of the four clipboard backends as they exist today, each in `src-tauri/src/clipboard/`:

| Backend | Runs on | File | Detection | Currently handles | MIME-byte read API exists? |
|---|---|---|---|---|---|
| **A** `plugin.rs` | X11, Windows, macOS | [src-tauri/src/clipboard/plugin.rs](src-tauri/src/clipboard/plugin.rs) | poll 500 ms via `tauri-plugin-clipboard` | text + files | **No** (plugin only exposes `read_text`/`read_files`) |
| **B** `wayland.rs` | Wayland wlroots (KDE/Sway/Hyprland) | [src-tauri/src/clipboard/wayland.rs](src-tauri/src/clipboard/wayland.rs) | poll 500 ms via `wl-clipboard-rs` | text + files (`text/uri-list`) | **Yes** (`PasteMimeType::Specific("…")`) |
| **C** `dbus_clipboard.rs` + GNOME extension | Wayland GNOME | [src-tauri/src/clipboard/dbus_clipboard.rs](src-tauri/src/clipboard/dbus_clipboard.rs), [gnome-extension/extension.js](gnome-extension/extension.js) | D-Bus signals on `Clipboard2` interface | text + files | **Partial** — `St.Clipboard.get_mimetypes()` and `get_content()` exist in the extension JS; **not exposed over D-Bus** |
| **D** *(merged into A)* | Win + macOS | same as A | same | same | same |

Wire payload today, [src-tauri/src/protocol.rs:10-18](src-tauri/src/protocol.rs#L10-L18):

```rust
pub struct ClipboardPayload {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub files: Option<Vec<FileMetadata>>,
    pub timestamp: u64,
    pub sender: String,
    pub sender_id: String,
}
```

The `image = "0.25.9"` crate is already a dependency (used today only by the tray-icon code in [tray.rs](src-tauri/src/tray.rs)) — we will reuse it for clipboard image conversion.

## 4. Wire protocol changes

### 4.1 Extend `ClipboardPayload`

```rust
pub struct ClipboardPayload {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub files: Option<Vec<FileMetadata>>,
    #[serde(default)]
    pub blob: Option<ClipboardBlob>,   // NEW
    pub timestamp: u64,
    pub sender: String,
    pub sender_id: String,
}

pub struct ClipboardBlob {
    pub mime_type: String,             // canonicalised; v1 always "image/png"
    pub data: Vec<u8>,                 // already in mime_type's encoding
    pub width: Option<u32>,             // hint for receiver UI / restock paths
    pub height: Option<u32>,
}
```

### 4.2 Format normalisation on the wire

Sender always normalises to **`image/png`** before transmission. Reasons:

- PNG is lossless — re-encoding from PNG (the most common source format) is a no-op aside from compression.
- Every receiving platform has a path to put PNG on the local clipboard.
- Avoids a translation matrix between every source MIME and every receiver platform.

Conversion is handled by the existing `image` crate: decode any input format → `RgbaImage` → encode PNG.

### 4.3 Backwards compatibility

- `#[serde(default)]` on `blob` lets old peers (≤ 0.2.3) parse new payloads — they'll deserialise it as `None` and ignore.
- New peers receiving from old peers won't see a `blob` field — also fine.
- Peers older than this feature simply won't *send* image blobs — no break.

This is **fully backwards compatible**, unlike the file-transfer compression feature. No warning needed in the UI.

### 4.4 Size cap

Inline blobs are capped at **10 MB on the wire** (post-PNG-encoding). Larger images are silently dropped with a debug log on the sender side. Rationale:

- A 4K screenshot at PNG-fast is typically 3–6 MB. 10 MB covers normal usage.
- Avoids saturating a single unencrypted message with multi-megapixel screenshots from photo-editing apps.
- A future v2 could route oversized blobs through the existing file-transfer deferred-download channel with a synthetic filename like `clipboard-image-<timestamp>.png` — the protocol already supports this.

## 5. Per-backend plans

### 5.1 Backend A — X11 / Windows / macOS (the `plugin.rs` path)

**The hard one.** `tauri-plugin-clipboard` v2 does not expose any way to read non-text non-file MIME types. We can't extend it from outside the crate.

**Recommended approach: add `arboard` as a parallel image-only shim.**

`arboard` ([crates.io](https://crates.io/crates/arboard)) is the de-facto Rust cross-platform clipboard crate. It supports text + image on X11, Windows, macOS:

- `Clipboard::get_image() -> Result<ImageData>` — returns RGBA pixels + dims.
- `Clipboard::set_image(ImageData) -> Result<()>` — re-stocks. Internally:
  - Windows: `CF_DIBV5` + `CF_DIB` (Word, Paint, GIMP all read these).
  - macOS: `NSPasteboardTypePNG` + `NSPasteboardTypeTIFF`.
  - X11: `image/png` selection target.

**Coexistence with tauri-plugin-clipboard:**

- Keep tauri-plugin-clipboard as-is for text + files.
- Add a parallel image-poll thread (same 500 ms cadence) that calls `arboard::Clipboard::get_image()`.
- The two pollers are read-only most of the time. X11 selection reads are non-destructive; CF_DIB / NSPasteboard reads are non-destructive. Coexistence has no race that corrupts state.
- Dedup is handled by content hashing (§6.2) — neither poller needs to know about the other.

**Why not replace tauri-plugin-clipboard with arboard entirely?** arboard does not handle file paths (no `text/uri-list` reading or writing). Replacing the file path would mean rewriting a chunk of working code. Parallel shim is lower risk.

**arboard on Wayland**: do **not** use it. Compile-gate the image shim to `cfg(not(target_os = "linux") OR (target_os = "linux" AND not(is_wayland)))`. On Wayland we use backends B and C.

**Files touched:**
- [src-tauri/Cargo.toml](src-tauri/Cargo.toml) — add `arboard = "3"`.
- [src-tauri/src/clipboard/plugin.rs](src-tauri/src/clipboard/plugin.rs) — add `read_clipboard_image()` and `write_clipboard_image()` calling arboard. Add a separate poll loop, or weave into the existing one (the existing 500 ms loop reads files first, then text — adding image as a third probe is straightforward).
- [src-tauri/src/clipboard/common.rs](src-tauri/src/clipboard/common.rs) — extend `ClipboardContent` enum with `Image(ClipboardBlob)` variant.

### 5.2 Backend B — Wayland wlroots (`wl-clipboard-rs`)

**Cleanest backend.** `wl-clipboard-rs` already supports arbitrary MIME types — we just call them.

**Approach:**

- During the existing poll loop (500 ms in [wayland.rs](src-tauri/src/clipboard/wayland.rs)):
  1. Use `wl_clipboard_rs::paste::get_mime_types(ClipboardType::Regular, Seat::Unspecified)` to enumerate offered types.
  2. If any of `["image/png", "image/jpeg", "image/bmp", "image/tiff", "image/webp", "image/gif"]` is present (preferred order), call `paste::get_contents(.., PasteMimeType::Specific(mime))` and read bytes into memory.
  3. Pass to `common::process_clipboard_change()` as a new `ClipboardContent::Image` variant.
- For setting (re-stocking on the receiving side):
  - `wl_clipboard_rs::copy::Source::Bytes(data.into())` with `MimeType::Specific("image/png".into())`.
  - Use `copy::Options::copy()` (not `copy_multi` — single-mime is fine for v1).

**Edge cases:**

- Some clipboards advertise `image/x-bmp` instead of `image/bmp` — include both in the priority list.
- Browsers sometimes only offer `text/uri-list` with a `data:` URI for images. Out of scope — we treat that as a file URI today; users can keep using existing flow.
- Empty clipboard / clipboard cleared — `get_mime_types` returns Err(NoSeats) or empty; nothing to do.

**Files touched:**
- [src-tauri/src/clipboard/wayland.rs](src-tauri/src/clipboard/wayland.rs) — add image MIME detection + read + write.

### 5.3 Backend C — Wayland GNOME (extension + D-Bus)

**Most invasive backend.** Requires changes on both sides:

> **Implementation note (deviation from original plan):** The original plan proposed bumping the D-Bus interface name from `Clipboard2` to `Clipboard3`. In implementation we instead **added** the new methods + signal to the existing `Clipboard2` interface (D-Bus is additive). This avoids forcing a hard upgrade — old apps with the new extension keep working unchanged, new apps with the old extension silently disable image clipboard sync but text + files keep working. The extension's `version-name` was still bumped to `4.0` in metadata.json to surface the feature externally.

#### 5.3.1 GNOME extension JS changes ([gnome-extension/extension.js](gnome-extension/extension.js))

Extend the existing `app.clustercut.clustercut.Clipboard2` interface (extension version **4.0**) — additive, fully backwards compatible.

**New D-Bus methods:**

```xml
<method name="GetMimetypes">
  <arg type="as" direction="out" name="mimetypes"/>
</method>
<method name="ReadBlob">
  <arg type="s"  direction="in"  name="mime_type"/>
  <arg type="ay" direction="out" name="data"/>
</method>
<method name="WriteBlob">
  <arg type="s"  direction="in"  name="mime_type"/>
  <arg type="ay" direction="in"  name="data"/>
</method>
```

**New signal:**

```xml
<signal name="BlobChanged">
  <arg type="s" name="mime_type"/>
</signal>
```

JS implementation uses APIs already in use today:

- `St.Clipboard.get_mimetypes(St.ClipboardType.CLIPBOARD)` (sync, used at [extension.js:323](gnome-extension/extension.js#L323)) — return as `as`.
- `St.Clipboard.get_content(St.ClipboardType.CLIPBOARD, mime, callback)` — the callback receives a `GLib.Bytes`. Return `bytes.unref_to_array()` as the D-Bus `ay` payload.
- `St.Clipboard.set_content(St.ClipboardType.CLIPBOARD, mime, GLib.Bytes.new(data))` — restock.

**Owner-changed handler** ([extension.js:284](gnome-extension/extension.js#L284)) needs a third branch: after the existing `text/uri-list`/text checks, look for any `image/*` MIME, and emit `BlobChanged(mime)` instead of (not in addition to) the text/files signals — image-bearing copies don't usually have meaningful text alongside.

**Async + D-Bus**: `get_content` is callback-based. The current `ReadFiles` method already deals with this asynchronously via `Gio.DBusMethodInvocation.return_value(...)` from inside the callback — same pattern applies.

**Echo prevention**: when the extension itself calls `set_content` (during `WriteBlob`), the resulting `owner-changed` event must not re-emit `BlobChanged`. The current code does this for text by tracking a `_lastWritten` flag — extend the flag to cover blob writes too.

#### 5.3.2 Rust D-Bus client changes ([src-tauri/src/clipboard/dbus_clipboard.rs](src-tauri/src/clipboard/dbus_clipboard.rs))

- Update `Clipboard2` references → `Clipboard3` everywhere.
- Add Rust wrappers `read_blob(mime: &str) -> Vec<u8>`, `write_blob(mime: &str, data: &[u8])`, `get_mimetypes() -> Vec<String>`.
- Subscribe to `BlobChanged` signal in `start_monitor` alongside the existing `ClipboardChanged` / `FilesChanged` subscriptions.
- On `BlobChanged(mime)`: call `read_blob(mime)`, build `ClipboardContent::Image(...)`, send to `process_clipboard_change`.

#### 5.3.3 Backwards compatibility for the extension

This is the **major compatibility wrinkle**. Old extensions only implement `Clipboard2` and won't have the new methods. The Rust client must:

1. Probe the extension's interface on startup.
2. If `Clipboard3` is present → enable blob support.
3. If only `Clipboard2` is present → log a warning, continue with text + file sync only, do not emit "blob support unavailable" UI errors.
4. The "outdated extension" notification path (already present at [src-tauri/src/lib.rs](src-tauri/src/lib.rs) — search "outdated extension") should pick this up via the existing extension-version check; bump the required version constant.

This mirrors the `Clipboard1` → `Clipboard2` bump done in 0.2.2 (extension version 3.0). The pattern is established.

#### 5.3.4 Extension publishing

- Bump `metadata.json`'s `version` to 4 (extension version 4.0 — they're integer-versioned by EGO).
- `just extension-zip` builds the validated zip.
- Submit to extensions.gnome.org via the existing flow.
- Announce in the app: existing "Clipboard sync paused — outdated extension" notification path catches it.

### 5.4 Backend D — Windows / macOS specifics

These ride on backend A via arboard. A few platform notes worth surfacing:

**Windows**:

- `arboard::Clipboard::set_image()` writes both `CF_DIB` and `CF_DIBV5`. Word/Paint/GIMP read CF_DIB. ✓
- It does **not** write `"PNG"` (a registered private format some apps prefer). Could matter for niche apps. Defer unless user-reported.
- Alpha channel is preserved in CF_DIBV5 but flattened to white in CF_DIB (a Windows GDI quirk). Acceptable.

**macOS**:

- `arboard::Clipboard::set_image()` writes `NSPasteboardTypePNG` and `NSPasteboardTypeTIFF`. Pages, Keynote, Preview all read these. ✓
- Does not write to the legacy `NSPasteboardTypePICT`. No modern app needs this.

## 6. Cross-cutting concerns

### 6.1 Image format conversion (sender side)

When a non-PNG image is read from the clipboard (e.g., `image/jpeg` from Wayland wlr; raw RGBA from arboard on X11/Win/macOS), the sender must encode to PNG before putting bytes in `ClipboardBlob`.

- Use `image::ImageBuffer<Rgba<u8>, _>` as the canonical in-memory form.
- Encode via `PngEncoder` with `CompressionType::Fast` (compression level 1) for speed — clipboard latency matters, and PNG-fast is ~5× faster than PNG-default with only 10–20% larger output.
- Decoding from arboard is trivial (`ImageData::bytes` is already RGBA).
- Decoding from arbitrary MIME via `image::load_from_memory_with_format(...)`.

### 6.2 Echo prevention (dedup)

Critical: when the receiver re-stocks the local clipboard after receiving an image from a peer, the local poll loop must **not** detect that as a new clipboard change and re-broadcast.

The existing dedup in [src-tauri/src/clipboard/common.rs](src-tauri/src/clipboard/common.rs) (`should_process_content`) hashes content and skips already-seen items. Extend the hash:

- For `ClipboardContent::Image(blob)`: hash `(blob.mime_type, blob.data)` with the same fast hash already used for text.
- Receiver, before calling `set_image()` on the local clipboard, registers the hash in the dedup window so the next poll cycle skips it.

This is the same pattern used today for text round-trips. No new mechanism, just an additional content type.

### 6.3 Encryption + auth

`ClipboardPayload` is already JSON-serialised then ChaCha20-Poly1305 encrypted with the cluster key before going on the wire as `Message::Clipboard(Vec<u8>)`. The new `blob` field rides inside the same encrypted envelope — no new auth code.

JSON serialisation of `Vec<u8>` is base64 by default with serde — efficient enough for our 10 MB cap. (Optimisation note: switching to `serde_bytes` or `bincode` for the blob field could halve the on-wire size, but adds complexity. Defer.)

### 6.4 Size handling

- **Sender**: after PNG-encode, if `data.len() > 10 * 1024 * 1024`, log a `tracing::warn!("clipboard blob too large ({} bytes), dropping")` and return early — do not send.
- **Receiver**: trust the sender (10 MB JSON-base64 ≈ 13 MB on the wire, well under QUIC datagram + reasonable for a single message). If a blob ever exceeds a hard 50 MB ceiling, drop it on the receive side as a safety check.

### 6.5 Compression interaction with the file-transfer compression feature (#3)

PNG is already deflate-compressed. Re-running zstd over a PNG yields ~0% saving. The new `compress_file_transfers` setting from issue #3 only applies to the **file transfer** path (`FileStreamHeader`-based streams) — clipboard blobs ride inside `Message::Clipboard` and do not touch that code path. **No interaction.**

### 6.6 Frontend / UX

- [src/App.tsx](src/App.tsx) — extend `HistoryItem` rendering: when an item carries blob data, render an `<img>` with a thumbnail (object-URL the bytes) and a small "image" badge.
- New event from backend: `clipboard-image` (or extend the existing `clipboard-change` payload to carry blob metadata).
- Notification copy for received blobs: *"Image received from \<peer\>"* (matches the existing text/file pattern).
- Settings: no new toggle proposed for v1 — blob sync is on whenever clipboard sync is on. (We could add an "Allow image clipboard sync" toggle later if users complain about bandwidth.)

### 6.7 Memory pressure

Up to 10 MB inline PNG bytes per clipboard event. Held in:

- The sender's `ClipboardPayload` until encrypted + sent.
- Each peer's deserialised `ClipboardPayload` until processed + handed to the OS clipboard.
- The history buffer (existing) — for blob items, store the bytes once; thumbnail is rendered from the same bytes via blob URL.

Caps: keep the history buffer's blob count bounded (existing `MAX_HISTORY` already covers this; if blobs make the buffer large in absolute bytes, add a per-item byte cap and evict old blobs first).

## 7. Risks & known issues

| # | Risk | Likelihood | Mitigation |
|---|---|---|---|
| 1 | Firefox on Wayland sometimes only offers `text/uri-list` with a `data:` URL for images, not raw bytes | High | Document; not a regression. User can use Chromium for image-clipboard-heavy workflows. |
| 2 | arboard's X11 implementation occasionally times out under heavy clipboard churn | Low | `get_image()` returns `Err`; we already handle `Err` gracefully (skip cycle). |
| 3 | GNOME `St.Clipboard.get_content` returns null silently for some MIME types | Low | If `data.is_empty()`, treat as "no blob present this cycle" and skip. |
| 4 | A peer running ClusterCut < 0.2.4 receives a blob payload and shows an empty entry in history | Low | Old clients see `text=""` and `blob=None` and either drop or show empty. Acceptable; not corruption. |
| 5 | Animated GIFs lose animation (we treat as a still image) | Medium | Document. Defer animated GIF support — would require sending raw bytes without re-encoding. |
| 6 | Browsers / apps offering an image only as a `text/html` `<img src="data:…">` blob — we won't pick that up | Medium | Out of scope for v1. Document. |
| 7 | macOS clipboard ownership flickers under arboard polling, causing some apps to "lose" their clipboard ownership briefly | Low | arboard's reads are non-mutating; tested in upstream. Verify in QA. |
| 8 | GNOME extension D-Bus message size limit (~128 MB by default) on `WriteBlob` for very large pastes | Very Low | Our 10 MB cap is well under the limit. |
| 9 | Two pollers (tauri-plugin-clipboard for text/files, arboard for images) racing on X11 selection ownership | Low | Both are read-only for the duration of the poll; X11 selection reads do not block writers. Test in practice. |
| 10 | Re-encoding PNG on every clipboard change is CPU-noticeable on slow ARM | Low | Only encode when we *send* (i.e. when content actually changed). PNG-fast is sub-100 ms on a 4K image even on slow ARM. |
| 11 | Receiver re-encodes PNG → platform format on every paste, even if unchanged | Low | Decoder cache by content hash; skip if the local clipboard is already this image. |
| 12 | Old GNOME extension (Clipboard2 only) — user upgrades app but not extension | Medium | Existing "outdated extension" notification path covers this; bump the required version constant. |

## 8. Implementation phases (suggested order)

Each phase is independently mergeable and testable.

### Phase 1 — Wire protocol + dedup foundation

- Add `ClipboardBlob` struct + `blob` field to `ClipboardPayload`.
- Extend `ClipboardContent` enum with `Image(ClipboardBlob)`.
- Extend dedup hash to cover image content.
- No backend wiring yet — just plumbing.
- **Test**: round-trip a hand-constructed `ClipboardBlob` through serialise → encrypt → decrypt → deserialise.

### Phase 2 — Backend B (Wayland wlroots)

- Easiest backend, fewest moving parts. Validate end-to-end on KDE/Sway.
- Add image read in [wayland.rs](src-tauri/src/clipboard/wayland.rs) poll loop.
- Add image write path.
- **Test**: copy an image in Firefox on KDE, paste in another KDE peer.

### Phase 3 — Backend A (X11, Windows, macOS via arboard)

- Add `arboard` dep.
- Add image-only poll path in [plugin.rs](src-tauri/src/clipboard/plugin.rs).
- Add image write path.
- **Test**: copy image in Firefox on X11 → paste in Word on Windows. Copy in Preview on macOS → paste in Firefox on KDE. Cross-platform matrix.

### Phase 4 — Backend C (GNOME extension + D-Bus)

- Bump extension to `Clipboard3` (version 4.0).
- Add JS methods + signal.
- Add Rust wrappers + signal subscription.
- Update extension version probe.
- Build extension zip; submit to EGO.
- **Test**: copy an image in Firefox on GNOME Wayland, paste in another GNOME Wayland peer; mixed-version peer pair.

### Phase 5 — Frontend / UX

- History UI: image thumbnails.
- Notification copy.
- Optional Settings toggle (defer if not asked for).

### Phase 6 — Polish

- Size cap warning in logs.
- Failure modes (corrupt PNG, empty clipboard, etc.).
- CHANGELOG + metainfo entries.
- Doc page in the user-facing docs site.

## 9. Verification matrix

End-to-end manual test grid before tagging the release. Sender platform × receiver platform, copying a PNG image in the sender's browser, pasting in the receiver's office suite or image viewer:

|  | RX: Linux X11 | RX: Linux KDE | RX: Linux GNOME | RX: Windows | RX: macOS |
|---|:---:|:---:|:---:|:---:|:---:|
| TX: Linux X11 | ✓ | ✓ | ✓ | ✓ | ✓ |
| TX: Linux KDE | ✓ | ✓ | ✓ | ✓ | ✓ |
| TX: Linux GNOME | ✓ | ✓ | ✓ | ✓ | ✓ |
| TX: Windows | ✓ | ✓ | ✓ | ✓ | ✓ |
| TX: macOS | ✓ | ✓ | ✓ | ✓ | ✓ |

Each cell: copy image → wait for "Image received" notification on RX → paste into a known-good app (Word / Preview / Firefox / GIMP) → image renders correctly with no corruption.

Plus:

- **Echo test**: copy an image, confirm `clipboard-change` fires once on TX, once on RX, no infinite loop.
- **Size cap test**: copy a 50 MB image (e.g., a multi-megapixel uncompressed BMP); confirm sender skips with a warning, no peer crashes.
- **Mixed-version test**: TX 0.2.4 → RX 0.2.3. RX should ignore the blob field, show no error, history may show an empty entry (acceptable).
- **Dedup test**: copy image, paste, copy *the same* image again, confirm one and only one round-trip per copy event.
- **Compression interaction test**: enable file-transfer compression (`compress_file_transfers = true`), copy an image; confirm the blob path is unaffected (same wire payload as compression-off, since blobs ride `Message::Clipboard`, not `FileStreamHeader`).

## 10. Open questions to resolve before implementation

- **Optional setting toggle?** Should there be a `sync_clipboard_images: bool` setting (default on) so users on metered links can disable image sync? Recommendation: defer until a user asks for it.
- **History persistence**: do image blobs persist in the on-disk history (binary blobs in JSON), or are they kept in-memory only? Recommendation: in-memory only for v1 — the existing JSON history is text-shaped and adding base64 megabytes per entry would balloon the file.
- **Extension D-Bus interface**: is `Clipboard3` the right next number, or should we jump to `Clipboard4` to avoid confusion with the extension's own version-4.0 metadata? Recommendation: `Clipboard3` — the interface number and extension version are already decoupled (Clipboard2 is in extension v3.0).
- **WebP and AVIF**: priority in the read-priority list? Recommendation: include both, decode → re-encode to PNG. Cost is negligible since they're rare on the clipboard today.

## 11. Phase status

| # | Phase | Status |
|---|---|---|
| 1 | Wire protocol + dedup foundation | ✅ complete |
| 2 | Backend B — Wayland wlroots (`wl-clipboard-rs`) | ✅ complete |
| 3 | Backend A — X11 / Windows / macOS (`arboard` shim) | ✅ complete |
| 4 | Backend C — GNOME extension + D-Bus (`Clipboard2` additive) | ✅ complete |
| 5 | Frontend / UX (history thumbnails, notifications) | ✅ complete |
| 6 | Polish (CHANGELOG, metainfo, docs, edge cases) | ⏸ pending |
