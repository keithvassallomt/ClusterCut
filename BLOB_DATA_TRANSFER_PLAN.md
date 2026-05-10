# Clipboard Rich-Data Transfer

## Part 3 — Other deferred work

These are smaller and lower-priority than rich-text. Listed roughly in expected order.

### 3.1 Multi-format simultaneous preservation

Closely related to rich-text and somewhat solved by the design above: once `formats` exists, "preserve all the things the source had" is a question of how aggressive the smart-capture allowlist is. v1 sticks to text/html + text/rtf + the existing image/files paths; v2 could expand the allowlist to include other formats the source actually puts on the clipboard — `image/svg+xml`, `application/x-vnd.oasis.opendocument…`, etc.

**ClusterCut's role is faithful relay, not synthesis.** We pass through whatever formats the source clipboard had, with the existing skip-list suppressing vendor-internal junk (Word's `Native` blob, Chromium's `x-renderer-taint`, etc.). We do *not* synthesise new representations from existing ones — e.g. no auto-rasterising SVG to a PNG companion, no rendering RTF to HTML to bridge format gaps. If a source app only emits SVG, the receiver gets SVG; whether the destination app paints it is the destination app's problem, same as it would be if the user copied locally without ClusterCut in the loop.

#### Implementation outline (SVG specifically — most likely first v2 addition)

SVG is text-shaped UTF-8 XML, so the architectural fit is awkward — image content conceptually but transports like rich text. Two paths considered:

- **Option A — `ClipboardBlob` with `mime_type = "image/svg+xml"`** (recommended): same shape as the existing PNG path, just verbatim bytes. Sender bypasses PNG normalisation when source MIME is `image/svg+xml` and stores the SVG bytes (base64-encoded since `ClipboardBlob.data` is a `String`). Receiver writes bytes verbatim under `image/svg+xml` via the OS-direct path. Keeps the mental model clean: `blob` = single primary image with a MIME label, `formats` = alternate text representations alongside plain text. ~30 lines per backend.
- **Option B — add `image/svg+xml` to the `formats[]` allowlist**: ~5 lines per backend, almost no diff. Semantically wrong — `formats[]` assumes "alongside plain text", and an SVG-only copy has no plain-text companion. Reject.

Per-backend work for Option A:
- **Wayland wlroots**: extend [common.rs::normalize_image_blob_from_bytes](src-tauri/src/clipboard/common.rs) with a SVG fast path — detect mime, skip `image::load_from_memory_with_format`, base64-encode the original bytes into `ClipboardBlob.data`. Receiver write: `wl_clipboard_rs` already supports arbitrary MIMEs.
- **GNOME extension**: add `image/svg+xml` to the `IMAGE_MIME_PRIORITY` in `extension.js` (or a separate vector list) and preserve the MIME through `_readAndEmitBlob`. Receiver via `St.Clipboard.set_content(CLIPBOARD, "image/svg+xml", bytes)`.
- **Windows / macOS via arboard**: arboard's `get_image()` decodes to RGBA — for SVG we have to *bypass* arboard. Read `image/svg+xml` (Windows: registered format atom; macOS: `public.svg-image` UTI) directly via `clipboard-win::raw::get_vec` / `NSPasteboard::dataForType`, before falling through to the existing arboard probe. Receiver write: same MIME via direct OS calls.

### 3.2 Animated GIFs

Currently, an `image/gif` source gets decoded to `RgbaImage` (frame 0 only) and re-encoded to PNG. Animation is lost. To keep animation, `ClipboardBlob.mime_type` would need to be allowed to stay `image/gif` and the data sent verbatim — skipping the decode/re-encode round-trip.

Receiver writes the bytes to the OS clipboard under `image/gif`:
- **Wayland / macOS**: native MIME-based clipboards, the format is recognised directly.
- **Windows**: no built-in `CF_GIF`, but a registered format atom (`RegisterClipboardFormat("image/gif")`) holds the bytes. Chromium-based / Electron apps read the registered format; classic Win32 apps that only know about `CF_BITMAP`/`CF_DIB` won't.

**That's where ClusterCut's responsibility ends.** The bytes are on the clipboard with the correct MIME. Whether a given destination app picks them up, animates them, or ignores them in favour of a raster fallback is the destination app's concern — same as if the user copied the GIF locally without ClusterCut in the loop. Adding heuristics on our side (auto-rasterise alongside, FileGroupDescriptor fallbacks, etc.) is scope-creep that papers over destination-app limitations rather than ClusterCut limitations.

Sources to be aware of: most desktop apps that "copy a GIF" actually rasterise to PNG locally before the clipboard ever sees GIF bytes (Slack, Discord, Firefox `Copy Image`, Chrome, etc.). So in practice we'll only see real `image/gif` bytes when the user copies from an app that genuinely preserves them — file managers (right-click → Copy on a `.gif` file uses the files path, not this), some image editors, and the GNOME extension if `St.Clipboard` is offering it.

#### Implementation outline

The current image path *normalises everything to PNG via an RGBA round-trip* — that's the part that has to grow a verbatim pass-through branch.

- **Sender**:
  - **Wayland wlroots**: in [common.rs::normalize_image_blob_from_bytes](src-tauri/src/clipboard/common.rs), add a fast path: if source MIME is `image/gif`, skip `image::load_from_memory_with_format` + PNG re-encode, take the bytes verbatim, set `blob.mime_type = "image/gif"`. ~10 lines.
  - **GNOME extension**: `_readAndEmitBlob` currently sends whatever MIME it picked. Add `image/gif` to `IMAGE_MIME_PRIORITY`, preserve the MIME through the chain. ~5 lines JS.
  - **Windows / macOS via arboard**: this is the hard one. `arboard::get_image()` returns RGBA — by the time we see the bytes, the original GIF stream is already gone. To preserve GIF we have to bypass arboard for the GIF probe and read `clipboard-win::raw::get_vec(register_format("image/gif"), …)` on Windows or `NSPasteboard::dataForType("com.compuserve.gif")` on macOS *before* falling through to arboard's RGBA path. ~50 lines per platform.
- **Receiver** (all backends): in `set_clipboard_image`, branch on `blob.mime_type`. If `image/png`, existing arboard / wlroots / extension path. If `image/gif`, write bytes verbatim under that MIME via OS-direct calls (registered format atom on Windows, native MIME on Wayland/macOS). ~30 lines total across backends.

**Total estimate**: ~150 lines + ~50 each for the Win/mac sender pre-arboard probe. Medium risk — the Win/mac side has new clipboard-format enumeration logic that needs platform testing (TIRI-style round-trip oddities possible).

**Worth-it analysis**: most desktop apps rasterise GIFs at copy time anyway, so the user-visible payoff is narrow — animation only survives when both the source app *and* the destination app preserve `image/gif`. Lower priority than 3.1 / 3.4.

### 3.3 Large clipboard blobs ride the existing file-transfer path

After multiple iterations chasing wire-format inflation and ALPN streaming complexity, the cleaner answer is to **reuse the file-transfer machinery** and just add a one-bit hint that says "this transfer should land as a clipboard blob, not as a file on disk." All the hard streaming/chunking/timeout/ack work already exists for files; we just need to teach both ends what to do at the destination.

The 10 MB boundary becomes meaningful again — under it, ride the existing inline `Message::Clipboard` path; over it, take the file-transfer path with a "treat as clipboard" hint.

#### The four cases

| # | What the user does | Wire path | Receiver behaviour |
|---|---|---|---|
| **A** | Copies a 6 MB JPEG (anything ≤ 10 MB inline) | `Message::Clipboard` — existing inline blob path | Decrypt, write to OS clipboard, paste-ready instantly. **No change from today.** |
| **B1** | Copies a 15 MB PNG (over inline cap, under `max_auto_download_size`) | `Message::Clipboard` carries a *descriptor* (fetch_id + mime + total_size + dims) instead of bytes. Sender writes bytes to a temp file and registers it in `local_files` keyed by message id, then bytes ride the existing `clustercut-file` ALPN stream. | Auto-download in background. **"Receiving large clipboard image…"** notification → **"Image available to paste"** notification once bytes land. The bytes are written to the OS clipboard (not disk), so paste-into-Word works. |
| **B2** | Copies a 65 MB mega-PNG (over `max_auto_download_size`) | Same wire shape as B1 — descriptor + file stream. | Receiver checks `total_size` against its `max_auto_download_size`. Over threshold → notify the user with size, **don't auto-download**. User clicks Accept → fetch starts → bytes land on the OS clipboard. Same paste destination as B1, just with a manual gate. |
| **C** | Copies an *image file* in the file manager (right-click → Copy in Nautilus / Files / Finder / Explorer) | Existing `Files(Vec<FileMetadata>)` path — completely unchanged. | Receiver gets a *file reference*. Pasting into a file manager works (file copy); pasting into Word/Photos as an image doesn't. **No change from today.** |

The split between **B (clipboard intent)** and **C (file intent)** is preserved end-to-end via:
- The sender's clipboard monitor knows whether the source was an image-bytes copy or a file-paths copy and routes accordingly.
- A new `delivery_target` field on `FileStreamHeader` says either `Disk` (existing) or `Clipboard { mime, width, height }` (new).
- The receiver's `clustercut-file` stream listener routes the incoming bytes to disk-write or in-memory-then-OS-clipboard based on `delivery_target`.

#### Why this is structurally better than what we had

Pre-fix, we tried to stuff multi-MB blobs through the inline `Message::Clipboard` path. That hit:
- The 64 MB transport per-message cap once `Vec<u8>` JSON int-array inflation pushed encrypted ciphertext over.
- Fixed with base64 outer wrapping, but still capped at ~35 MB practical.
- Anything over silently dropped, with no UX feedback.
- Slow and unreliable for the multi-MB sizes users actually want.

The file-transfer path doesn't have any of that:
- Uni-directional QUIC stream — no per-message cap.
- Already engineered for multi-MB transfers with `stopped()` ACK and 30 s drain timeout.
- Notifications + accept/reject UX already wired up.
- Race protection (in-flight tracking by id) already exists.

#### Wire-format additions

Two struct extensions, both `#[serde(default)]` for backward compatibility:

```rust
pub struct ClipboardBlob {
    pub mime_type: String,
    pub data: String,                 // base64 — empty when fetch_id is Some
    #[serde(default)] pub width: Option<u32>,
    #[serde(default)] pub height: Option<u32>,
    /// New: descriptor mode. When Some, `data` is empty and the receiver
    /// must fetch bytes via `Message::FileRequest` referencing this id.
    #[serde(default)] pub fetch_id: Option<String>,
    /// New: total raw byte size of the eventual blob — receiver uses this
    /// to decide auto-fetch vs. user-confirm against `max_auto_download_size`.
    #[serde(default)] pub total_size: Option<u64>,
}

pub enum DeliveryTarget {
    Disk,                                          // existing default
    Clipboard {
        mime_type: String,
        width: Option<u32>,
        height: Option<u32>,
    },
}

pub struct FileStreamHeader {
    pub id: String,
    pub file_index: usize,
    pub file_name: String,
    pub file_size: u64,
    pub auth_token: String,
    #[serde(default)] pub compressed: bool,
    /// New: Disk (existing) or Clipboard. Receiver routes the incoming
    /// stream bytes accordingly.
    #[serde(default = "default_delivery_target")] pub delivery_target: DeliveryTarget,
}
```

`DeliveryTarget::Disk` is the default for backward compat — older peers' headers omit the field, deserialise as Disk, behave exactly as today.

#### Race protection

Receiver tracks in-flight clipboard fetches by id. If a new clipboard event arrives (any path) before the fetch completes, the in-flight fetch is abandoned: bytes still drain off the wire to keep QUIC happy, but they don't overwrite the OS clipboard. State key: `state.in_flight_clipboard_fetch: Option<String>` — the id the receiver is currently fetching for clipboard delivery. Cleared on completion or abandonment.

#### Notifications

| State | Notification |
|---|---|
| B1 fetch starts | "Receiving large clipboard image from {sender}…" — info, dismissable |
| B1 fetch completes | "Image available to paste" — same as today's auto-receive notification |
| B2 awaiting confirmation | "Large clipboard image from {sender} ({size} MB) — accept?" — actionable, with Accept button → triggers fetch |
| B2 user accepts → fetching | same as B1 fetch-starts |
| Fetch fails (network, timeout, sender no longer has the bytes) | "Couldn't receive clipboard image from {sender}: {reason}" — dismissable |

#### Cleanup

Sender's temp file: written under `temp_downloads`/`<id>.<ext>` (existing temp dir). Cleaned up on:
1. Successful FileRequest response from a peer (file served, bytes delivered).
2. Sender's app exit (existing `clear_cache` already wipes this dir on startup; could also wipe on quit).
3. Periodic sweep — files older than 1 hour get deleted.

For now, just rely on (1) and (2). Add (3) only if leakage shows up in testing.

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
| 1 | Wire-format plumbing (`ClipboardFormat`, `formats` field, signature, round-trip tests) | ✅ complete |gdbus introspect --session --dest org.gnome.Shell --object-path /org/gnome/Shell/Extensions/ClusterCut | grep -E "WriteFormats|FormatsChanged"

| 2 | Backend B — Wayland wlroots (extend MIME probe + multi-MimeSource write) | ✅ complete |
| 3 | Backend A — Windows + macOS (HTML/RTF reads/writes; CF_HTML wrap/strip helpers). **X11 intentionally out of scope** — too costly for a declining platform | ✅ complete (needs verification on real Win/macOS builds) |
| 4 | Backend C — GNOME extension v4.0 (add format methods to the same v4.0 release as image blobs) | ✅ complete |
| 5 | Frontend — Rich-format badge in history + manual-sync modal (no inlinessh -o ServerAliveInterval=15 mimir 'tail -F -n +1 /tmp/clustercut-mimir.log' > /tmp/clustercut-mimir.log
 HTML rendering — would need DOMPurify; current strict CSP keeps it conservative) | ✅ complete |
| 6 | Smart-capture allowlist + cross-app polish (`;charset=utf-8` MIME variants, 16 MB per-format cap on the GNOME extension to match other backends, explicit allowlist comments documenting which vendor blobs are intentionally skipped) | ✅ complete |

### Part 3 — deferred work

| # | Item | Status |
|---|---|---|
| 3.1 | SVG (vector image) clipboard sync — verbatim pass-through | ✅ complete (test plan: T-3.1.x) |
| 3.2 | Animated GIF preservation — verbatim pass-through | ✅ complete (test plan: T-3.2.x) |
| 3.2b | JPEG passthrough (avoids 5-30× PNG re-encode inflation for photos) | ✅ complete (test plan: T-3.2.6) |
| 3.3 a | Inline cap restored to honest 10 MB; base64 outer wrapping kept (still useful for the inline path's wire size) | ✅ complete |
| 3.3 b | Descriptor on `Message::Clipboard` + `delivery_target` on `FileStreamHeader` for blob intent over the file-transfer path. Tier B1 (auto-fetch) and B2 (user-confirm above `max_auto_download_size`) | ✅ complete (test plan: T-3.3.x) |
| TIRI-stale | IGNORED guard auto-expires after 10 s if echo never arrives — fixes stuck `Image(svg+xml)` driving spurious "variant differs" broadcasts on every later clipboard event | ✅ complete (test plan: T-TIRI-stale) |

### Release polish (after Parts 1 + 2 + 3 are in)

| # | Phase | Status |
|---|---|---|
| 7 | CHANGELOG, metainfo, **docs (note rich-text not supported on X11)**, end-to-end testing on Win/macOS/GNOME, EGO submission for extension v4.0 | ⏸ remaining |

---

## Part 3 — Test Plan

Manual test cases for the Part 3 deferred-work features. Run these end-to-end on a multi-machine cluster (e.g. Linux + macOS + Windows) once Part 3 is fully implemented, before tagging 0.3.0. Each case includes the expected behaviour and a quick sanity check.

### §3.1 — SVG (vector image) clipboard sync

#### T-3.1.1 — SVG round-trip preserves vector data

> **Source-app caveat**: Most desktop SVG editors **rasterise on Copy**. Inkscape's `Edit → Copy` puts `image/png` on the public clipboard (it uses a proprietary `image/x-inkscape-svg` MIME for in-app round-trips, *not* the canonical `image/svg+xml`). Affinity Designer and most browser "Copy SVG" actions also rasterise. **Use the methods below** to put real `image/svg+xml` bytes on the clipboard for testing — they exercise the actual passthrough path.

1. On a Linux/wlroots peer, put canonical SVG bytes on the clipboard:
   - **Wayland (KDE/Sway/Hyprland)**: `wl-copy --type image/svg+xml < /path/to/test.svg`
   - **X11**: `xclip -selection clipboard -t image/svg+xml -i /path/to/test.svg`

   (Any small `.svg` file works — even a one-line `<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>` saved to a file.)
2. On the receiving peer, paste into a text editor (the simplest verification — the raw XML should appear) or into Boxy SVG / a vector-aware app for visual confirmation.

Expected:
- Receiver log: `Received clipboard image from <peer>: mime=image/svg+xml, decoded=<N> bytes` (no dimensions — SVG has no intrinsic raster dims).
- Pasting into a text editor produces the original `<svg>…</svg>` XML verbatim, **not** rasterised bytes.
- ClusterCut's history view shows the entry tagged `image/svg+xml` rather than `image/png`.

**Real-world sources that *do* put `image/svg+xml` on the clipboard** (worth a follow-up test if any are installed):
- Boxy SVG
- Krita with the SVG-aware copy mode
- Some GIMP versions when a path is selected and copied
- Firefox when copying a `<svg>` element selected via the inspector (hit-or-miss)

#### T-3.1.2 — SVG beats raster fallback when source offers both

1. On a peer with a source that puts BOTH `image/svg+xml` and `image/png` on the clipboard at once (some browsers do this when "Copy Image" is used on an inline SVG), copy.
2. Observe the receive event on the receiving peer.

Expected:
- Receive log shows `mime=image/svg+xml`, not `image/png`. ClusterCut prefers vector when available.
- Pasting into a vector-aware app gives the SVG; pasting into a raster-only app falls back via the destination app's own format negotiation (or fails, depending on the destination).

#### T-3.1.3 — SVG TIRI check (no image-rebroadcast bouncing)

1. With dev logs streaming on both peers, copy an SVG on the sender.
2. After the receiver picks it up, watch the receiver's log for ~5 seconds.

Expected:
- Receiver logs `Received clipboard image from <sender>: mime=image/svg+xml, …`.
- **No** subsequent `Sent clipboard to <sender>` line appears within ~3 seconds. SVG bytes are UTF-8 stable across the OS clipboard layer, so `image_blob_eq_stable`'s byte-equality fallback (when `width`/`height` are `None`) suppresses the echo cleanly.

#### T-3.1.4 — Cross-backend coverage

Repeat T-3.1.1 with sender/receiver pairs across all four supported backend combinations:
- Linux wlroots (KDE) → Linux GNOME (extension)
- Linux GNOME → macOS (plugin/NSPasteboard)
- macOS → Windows (plugin/clipboard-win)
- Windows → Linux wlroots

Expected: SVG arrives intact on all four. **X11 sender/receiver intentionally not in scope** — X11 falls back to the existing raster-PNG image path or no image at all.

---

### §3.2 — Animated GIF clipboard sync

#### T-3.2.1 — Animated GIF preserves animation

1. Find or create a small animated GIF (e.g. `wget` a sample, or any `.gif` file from your downloads). Open the file in a GIF-aware app — Firefox in image-view mode, an image editor that natively reads GIF, the GIMP "open" dialog, etc. — that will put the GIF bytes (not a rasterised frame) on the clipboard when copied.
   - On GNOME/Wayland: open the GIF in `eog` (Eye of GNOME) or right-click in Files → "Copy" the file (note: this uses the **files** path, not blob — for the blob path you need an app that copies image bytes, not a path). Inkscape with an SVG canvas containing an embedded GIF works.
   - Pragmatic shortcut: most desktop apps rasterise GIFs to PNG before they hit the clipboard. To exercise the path reliably, `xclip -i -selection clipboard -t image/gif < animation.gif` on Linux X11/wlroots, or PowerShell `Set-Clipboard` with a registered "image/gif" format on Windows. macOS: drag from a file manager into a `pbcopy`-style helper isn't trivial; use a test app or run a small Python/Swift snippet that writes `com.compuserve.gif` to NSPasteboard.
2. On a peer running ClusterCut, paste into a GIF-aware app — Discord, Slack, Telegram desktop client, a web browser address bar.
3. The pasted result animates.

Expected:
- Receiver-side log: `Received clipboard image from <peer>: mime=image/gif, decoded=<N> bytes` (no dimensions reported — same shape as SVG).
- Pasting into Chromium/Electron-based apps (Discord, Slack, Edge/Chrome) preserves animation.
- Pasting into "classic" raster-only apps (Paint on Windows, Preview on macOS) ignores the GIF and either pastes nothing or an existing raster fallback the source app may have provided.
- **No silent rasterisation by ClusterCut.** The wire format stays `image/gif`; we do not synthesise a PNG companion.

#### T-3.2.2 — GIF beats PNG when source offers both

1. Use a source that offers both `image/gif` and `image/png` (a few image apps do this: the GIF for GIF-aware destinations, the PNG as a still-frame fallback). If you don't have one handy, simulate by manually putting both formats on the clipboard via the OS's clipboard CLI tools.
2. Observe the receive event on the peer.

Expected:
- Receive log shows `mime=image/gif`, not `image/png`. ClusterCut prefers passthrough (animated) when available.
- The PNG companion is not relayed.

#### T-3.2.3 — GIF TIRI check

1. With dev logs streaming on both peers, copy a GIF on the sender.
2. Watch the receiver's log for ~5 seconds after the receive event.

Expected:
- Receiver logs `Received clipboard image from <sender>: mime=image/gif, …`.
- **No** subsequent `Sent clipboard to <sender>` line within ~3 seconds. GIF bytes on the OS clipboard layer round-trip stably (NSPasteboard keeps them verbatim under `com.compuserve.gif`; Windows registered format atom is bytes-in-bytes-out; wlroots `image/gif` selection round-trip is stable). `image_blob_eq_stable`'s byte-equality fallback (when dims are absent) suppresses the echo.

#### T-3.2.4 — Cross-backend coverage

Repeat T-3.2.1 with the same backend pairs as T-3.1.4. Expected: GIF arrives intact on all combinations, animation preserved when the destination app supports `image/gif` paste, plain-bytes fallback otherwise. **X11 not in scope.**

#### T-3.2.6 — JPEG passthrough (avoids PNG inflation on photos)

1. Find a JPEG photo around 20–40 MB on disk (a high-resolution camera shot works well).
2. On a Wayland/wlroots peer:
   ```bash
   wl-copy --type image/jpeg < /path/to/photo.jpg
   ```
3. Watch the sender log.

Expected:
- Receiver log: `Received clipboard image from <peer>: mime=image/jpeg, decoded=<N> bytes` (no dimensions — JPEG passthrough doesn't populate them).
- **No** `Clipboard image PNG (… bytes) exceeds … byte wire cap; skipping.` warning. Pre-fix, a 30 MB JPEG decoded → re-encoded as PNG would balloon to ~143 MB and silently drop. Post-fix, the JPEG bytes ride the wire as `image/jpeg` (≈30 MB), well under the cap.
- Pasting on the receiver into a JPEG-aware app (Photos, Preview, browsers, image editors) shows the original photo.

#### T-3.2.5 — Common-case regression check (Slack/Discord/Firefox copy-image)

1. From Firefox/Chrome on Linux, right-click a regular *static* PNG image on a webpage → **Copy Image**.
2. Receive on a peer.

Expected:
- Wire MIME is `image/png` (browsers rasterise static images at copy time, not GIF — confirmed earlier).
- The existing raster-PNG path handles the receive; no regression from the GIF passthrough work.
- `mime=image/png` in the receive log, with `width`/`height` populated as before.

---

### §3.3 — Large-blob descriptor + file-transfer-with-clipboard-hint

> **Design recap**: ≤ 10 MB images ride inline on `Message::Clipboard` (Tier A, unchanged). > 10 MB images: sender writes the bytes to a temp file under `temp_downloads/<id>.<ext>`, registers them in `state.local_clipboard_blobs`, and broadcasts a *descriptor* on `Message::Clipboard` with empty data plus `fetch_id`, `mime_type`, `total_size`, and dims. Receivers route into one of two paths based on `total_size` vs. `max_auto_download_size`:
>
> - **Tier B1** (≤ `max_auto_download_size`): auto-fetch via the existing `clustercut-file` ALPN stream. The new `delivery_target = Clipboard { mime, w, h }` field on `FileStreamHeader` tells the receiver to land the bytes on the OS clipboard rather than in `temp_downloads`. Notifications fire at fetch start ("Receiving Clipboard Image…") and on completion ("Image Available to Paste"). Race protection: a newer clipboard event clears `state.in_flight_clipboard_fetch`, and bytes for the older fetch — though still drained off the wire to keep QUIC happy — are dropped instead of overwriting the OS clipboard.
> - **Tier B2** (> `max_auto_download_size`): same wire shape, but the receiver does not fetch automatically. Instead the descriptor is stashed in `state.pending_clipboard` and an actionable notification fires. The user accepts via the existing pending-clipboard UI, which calls `confirm_pending_clipboard`; that command detects descriptor mode and triggers the FileRequest.
>
> **Tier C** (file from file manager) is unchanged — `Files()` payload, `delivery_target = Disk`, paste lands as a file reference.

#### T-3.3.1 — Inline path unchanged for ≤ 10 MB images

1. Copy a small image (a few hundred KB) on the sender.
2. Observe normal behaviour on the receiver.

Expected:
- Receiver log: `Received clipboard image from <peer>: mime=image/png, decoded=<size> bytes, WxH`.
- "Image Received" notification fires with body "Image copied to clipboard" — **not** "Receiving Clipboard Image…" or "Large Image Received". Confirms the threshold gate works.
- Paste produces the original image, no latency change vs. pre-§3.3.

#### T-3.3.2 — > 10 MB image auto-fetches via descriptor (Tier B1)

1. Configure `max_auto_download_size = 50 MB` (default).
2. Find or generate a PNG that encodes to ~25 MB (e.g. a 4K screenshot saved as PNG with no quality knob, or copy a 25 MB JPEG photo via `wl-copy --type image/jpeg`).
3. Copy on the sender.
4. Observe the receiver.

Expected:
- Sender log:
  - `[ClipboardBlob] Large blob detected (<bytes>, mime=…) — broadcasting descriptor (id=<uuid>)`
  - On peer FileRequest: `Opening QUIC Stream to <addr> for clipboard-blob '<id>.<ext>' (<bytes> bytes, mime=…)`
  - `Clipboard-blob sent successfully: <path>`
- Receiver log:
  - `Received clipboard image descriptor from <sender>: mime=…, total=<bytes>, fetch_id=<uuid>`
  - `[ClipboardBlob] Auto-fetching descriptor (<bytes>, mime=…)`
  - `Receiving Clipboard Blob: mime=…, <bytes> bytes, id=<uuid>, from=<addr>`
  - `Clipboard-blob stream complete: <bytes> in <duration>`
- Two notifications fire on the receiver:
  - "Receiving Clipboard Image — Receiving X.Y MB image from <sender>…" at fetch start.
  - "Image Available to Paste — X.Y MB image is now on the clipboard." on completion.
- Pasting on the receiver into an image-aware app shows the original image. The history view shows the entry with a thumbnail and size.

#### T-3.3.3 — > `max_auto_download_size` requires user accept (Tier B2)

1. Lower `max_auto_download_size` to 20 MB in Settings → File Transfer.
2. Copy a 25 MB image on the sender.
3. Observe the receiver.

Expected:
- Receiver log:
  - `Received clipboard image descriptor from <sender>: mime=…, total=…`
  - `[ClipboardBlob] Descriptor <bytes> exceeds auto-download limit <bytes> bytes — awaiting accept`
- "Large Clipboard Image — X.Y MB image from <sender> — accept to receive." actionable notification fires.
- The descriptor appears as a pending entry in the receiver's history view (same UI as auto-receive=off).
- User accepts via the history UI → `confirm_pending_clipboard` triggers the fetch → bytes land on the OS clipboard, paste works.
- If the user never accepts, the descriptor sits in `pending_clipboard` until the next clipboard event displaces it. No silent drop.

#### T-3.3.4 — Race: newest clipboard event wins

1. With `max_auto_download_size = 50 MB`, copy a 25 MB image on the sender.
2. Within a second of the receiver's "Receiving Clipboard Image…" notification, copy a *different* image (or any text) on a *third* peer (so both events arrive at the receiver while the first fetch is in flight).
3. Observe the receiver.

Expected:
- Receiver log:
  - First fetch starts: `[ClipboardBlob] Auto-fetching descriptor`
  - Second event arrives: `Received clipboard image (or text) from <other-sender>` (the new BLOB HANDLING block clears `in_flight_clipboard_fetch`)
  - When the first fetch's bytes finish landing: `[ClipboardBlob] Discarding fetched bytes for id=<old-uuid> — superseded by a newer clipboard event`
- Final clipboard contents on the receiver = the newer (second) event, never the older (first) one. **Newest copy wins**, regardless of fetch ordering.

#### T-3.3.5 — `enable_file_transfer = false` blocks descriptor fetches

1. In Settings → File Transfer, disable file transfers.
2. Copy a 25 MB image on the sender.
3. Observe the receiver.

Expected:
- Receiver log: `File transfer disabled in settings. Ignoring large clipboard descriptor.`
- No fetch is attempted, no clipboard write happens, no notification fires.
- The user-visible result is silent drop. Acceptable: file-transfer-off explicitly opts out of all file-shaped wire activity, of which descriptor-mode is one.

#### T-3.3.6 — Backward compat: pre-§3.3 peer

1. Pair a peer running ClusterCut 0.3.0-alpha-2 (no descriptor support) with a peer running this build.
2. Copy a > 10 MB image on the new-build peer.
3. Observe the old-build peer.

Expected:
- The descriptor `Message::Clipboard` deserialises on the old peer with `blob.fetch_id = None` and `blob.data = ""`. The old peer's blob handler tries to decode empty data, gets nothing, and the receive is a silent no-op rather than a crash. **Acceptable graceful degradation.**
- Going the other direction: an old peer copying a small image inline still works on the new peer (the new peer's `is_descriptor()` check returns false, the inline path runs as before).

#### T-3.3.7 — File path is unchanged

1. Copy a regular file (e.g. a PDF) from the file manager on the sender (right-click → Copy in Nautilus / Files / Finder / Explorer).
2. Observe the receiver.

Expected:
- Wire payload uses `Files(Vec<FileMetadata>)`, **not** the descriptor blob path. `delivery_target` on the file-transfer header is `Disk`.
- Receiver log: existing `Receiving File: <name> (<bytes>) [ID: <id>]` followed by the regular download progress, ending with `File Sent Successfully` / `File Transfer Verified OK`.
- File lands in `temp_downloads`, paste behaves as today (file reference, file-manager paste copies the file).

### T-TIRI-stale — IGNORED guard auto-expires after 10 s

This validates the fix for the stuck-IGNORED bug surfaced during §3.1 testing: a Sender that received an SVG paste held `IGNORED_CONTENT = Image(svg+xml)` indefinitely, so every subsequent unrelated clipboard event (text, files, image of a different MIME) hit the "variant differs" branch and broadcast unnecessarily.

#### T-TIRI-stale.1 — Stale guard expires

1. With dev logs streaming, copy any image on the sender (e.g. `wl-copy --type image/svg+xml < some.svg`). Verify `[Echo] Set IGNORED guard -> Image(...)` appears in the log.
2. Wait at least 10 seconds.
3. Copy something completely different on the sender — e.g. a plain text snippet (`echo hello | wl-copy`).

Expected:
- Sender log shows `[Echo] IGNORED guard expired after <≈10s> — clearing stale Image(...) guard` immediately before processing the new copy.
- The new copy hits the `IGNORED is None` branch (no spurious "variant differs" miss) and broadcasts cleanly.
- No follow-up bouncing for that text on subsequent unrelated copies.

#### T-TIRI-stale.2 — Fast unrelated copies don't accidentally clear a still-valid guard

1. Copy an image on the sender.
2. Within ~2 seconds (before TTL expires), copy unrelated text on the sender.
3. Watch the receiver.

Expected:
- The IMAGE echo from step 1 is still suppressed (IGNORED was set, the bytes round-trip back through the monitor poll, MATCH fires, IGNORED clears via the normal MATCH path).
- The TEXT copy from step 2 broadcasts as a fresh event.
- Both peers end up with the text on their clipboards (the image was the sender's transient state).
- No infinite bouncing.

_(Earlier deferred T-3.3.6 — descriptor + auto-fetch — is superseded by T-3.3.2 through T-3.3.7 above and removed.)_
