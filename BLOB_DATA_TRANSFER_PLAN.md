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

### 3.3 Streaming or deferred-download for very large blobs

The current 10 MB inline cap is plenty for screenshots and typical web images, but it cuts off:
- large editor exports (e.g. a poster from Affinity Designer)
- multi-second audio/video clips
- huge formatted documents

The fix reuses the **existing file-transfer channel** (`FileStreamHeader` / `clustercut-file` ALPN) and the **existing `max_auto_download_size` setting** (50 MB by default — already used to gate auto-download of files). Three tiers based on the encoded payload size:

#### Tier 1 — ≤ 10 MB: inline blob (no change)

Existing path. Sender broadcasts the bytes inside `ClipboardPayload.blob`, encrypted, in a single `Message::Clipboard` over the `clustercut-transport` ALPN. Receiver decrypts and immediately writes to the OS clipboard. Paste is instant. **Nothing changes for this tier — the threshold split is invisible to typical users.**

#### Tier 2 — > 10 MB and ≤ `max_auto_download_size`: descriptor + auto-fetch

Sender broadcasts a *descriptor* over the existing `Message::Clipboard` path — same encryption, same dedup signature shape — but with `ClipboardBlob` containing a `fetch_id`, `mime_type`, `width`, `height`, and `total_size` instead of the bytes themselves. `data` is empty. (`fetch_id` reuses or mirrors the existing `id` shape from `FileRequestPayload`.)

Receiver, on seeing a fetch-style blob:
1. Emits a `"Downloading data…"` notification + a placeholder history entry showing dimensions and size with a download progress bar (same UI vocabulary as large file transfers, but the destination is the clipboard rather than disk).
2. Opens a `clustercut-file` ALPN stream to the sender, pulls the bytes (ALPN, not the inline transport — it's already engineered for multi-MB streaming with `stopped()`-based ACK and a 64 MB-per-message cap doesn't apply because file streams are uni and chunked).
3. Once download completes, decodes/decrypts the bytes, writes to the OS clipboard via the existing `set_clipboard_image` path (the same one Tier 1 uses), and emits a `"Data available to paste"` notification.
4. From then on, paste behaves identically to a Tier 1 image.

UX requirement: the two notifications above are essential. Without them, users will hit Ctrl+V mid-download, paste the previous clipboard contents instead of the newly-arriving image, and conclude that sync is broken. The "Downloading data…" toast prevents that confusion.

**Concurrency / race**: if the user copies something else (text, smaller image, etc.) on the *receiver* while a Tier 2 download is in flight, the newest copy wins — the receiver's clipboard monitor has already detected the local change, and the in-progress download (if it eventually completes) is discarded rather than overwriting the user's intent. Easy to encode: each download tracks the `id` it was started for; if `state.last_clipboard_content` no longer matches that id when the download finishes, drop the bytes.

#### Tier 3 — > `max_auto_download_size`: file-transfer fallback (no clipboard restock)

For genuinely huge clipboard payloads (40K wallpapers, raw camera dumps, multi-second uncompressed audio), the descriptor goes onto the wire as today's **large-file** notification, **not** as a clipboard event. The receiver gets the same notification + history entry it gets for any large file transfer, the user clicks "Accept" to download (or doesn't), and the result lands as a file in the configured download location.

**Important UX shift**: Tier 3 is no longer a "clipboard sync" experience. The data ends up as a *file* the user can open or drag/copy into another app. It does **not** auto-restock the OS clipboard. This is the same model files already use today, and consistent with how big files arrive — but worth being explicit about, because the user copied something expecting it to be on the clipboard, and at this size that expectation isn't met. The notification text needs to be specific so the user understands: "Large clipboard image from {sender} ({size}) — too large to auto-paste. Click to download as a file."

Future option (not in scope of this doc): a clipboard-side "restock" button on the Tier 3 history entry that, after the user has explicitly accepted the file, decodes it and pushes onto the OS clipboard. Trades manual click for the missing auto-paste. Reasonable v2.

#### macOS optimisation (defer)

NSPasteboard supports lazy/promise-based pasteboard owners (`NSPasteboardWriting` + `pasteboard:item:provideDataForType:`). On macOS specifically, the receiver could put a *promise* on the pasteboard immediately on Tier 2 descriptor receipt and only pull the bytes when an app actually pastes. That would make Ctrl+V "just work" with on-demand fetch and skip the awkward "wait for the toast to flip" UX for tier 2. Windows and X11/Wayland don't have an equivalent, so this is purely a macOS gloss layered on top of Model A above — out of scope until tier 2 itself ships.

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
| 3.3 v1 | Inline cap raised to a real **35 MB** + large-blob notification + base64 outer wrapping (was claimed 60 MB but actual usable was ~13 MB pre-fix because `Vec<u8>` JSON int-array inflated random encrypted bytes 3.57×; base64 brings outer inflation to 1.33×, total ~1.78× vs the 64 MB transport cap) | ✅ complete (test plan: T-3.3.1–T-3.3.5) |
| 3.3 v2 | Descriptor + auto-fetch over `clustercut-file` ALPN, race protection, Tier 3 file-fallback | ⏸ deferred (test plan: T-3.3.6) |
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

### §3.3 — Large-blob inline transfer (v1 — cap raised to 60 MB)

> **Scope note**: §3.3 ships in two phases. **v1 (this commit)** raises the inline cap from 10 MB to 60 MB and adds a "Large Image Received" notification. Big blobs ride the same `Message::Clipboard` path as small ones — no descriptor, no fetch indirection. Test cases below validate the v1 path.
>
> **v2 (deferred)** is the descriptor + auto-fetch design from earlier in this document — separate `clustercut-file` ALPN stream, "Downloading data…" → "Data available to paste" UX, race protection (newest copy wins), and Tier 3 file-fallback for >50 MB. Test cases below labeled **(v2 — deferred)** describe what *should* be tested once that lands; they're not actionable yet.

#### T-3.3.1 — 11–60 MB image transfers cleanly

1. Find or generate a high-resolution PNG that encodes to 15–25 MB (e.g. a screenshot of a 4K display, a photo edited at maximum quality). Verify the PNG file size on disk first.
2. Open the image in any clipboard-aware app and Copy.
3. Wait a few seconds for the transfer to complete; observe receiver logs and notifications.
4. Paste into a destination app on the receiver.

Expected:
- Sender log: `send_message: connecting … for <N> byte payload` where N is roughly 1.04× the encoded size.
- Receiver log: `Received clipboard image from <peer>: mime=image/png, decoded=<size> bytes, WxH`.n
- A "Large Image Received" notification fires on the receiver with the size in MB.
- Paste produces the original image. No truncation, no corruption.

#### T-3.3.2 — Above the 60 MB cap, sender silently drops

1. Generate or find a PNG that encodes to >60 MB (rare in practice — typically requires uncompressed-style PNG of a very large image).
2. Copy on the sender.

Expected:
- Sender log: `Clipboard image PNG (<N> bytes) exceeds <60 MB> byte wire cap; skipping.`
- Receiver sees nothing. No bounce, no error to user.
- **TODO when v2 lands**: this case should route to Tier 3 file-fallback instead of silently dropping.

#### T-3.3.3 — Existing ≤10 MB path is unchanged

1. Copy a small image (a few hundred KB).
2. Observe normal behaviour.

Expected:
- Receiver log: `Received clipboard image from <peer>: …` as before.
- Notification text is the *original* "Image Received" / "Image copied to clipboard" — **not** "Large Image Received". Confirms the >10 MB threshold gate works.
- No latency change vs. the pre-§3.3 behaviour.

#### T-3.3.4 — Network strain / throughput sanity

1. Copy a 30 MB encoded image on the sender.
2. While transfer is in flight, copy a small text snippet on a *different* peer (so two clipboard events compete on the cluster bus).
3. Observe both arrive.

Expected:
- Both events propagate to all peers eventually. The big blob doesn't block the message bus indefinitely. (Each `Message::Clipboard` is its own QUIC stream, so this should hold even under inline transfer.)
- If you observe one stalling behind the other for >5 s on a healthy LAN, file an issue — that's a transport problem worth digging into.

#### T-3.3.5 — Encryption + transport sanity for large blobs

1. Copy a 50 MB encoded image.
2. On the receiver, watch for any `quinn::ReadToEndError::TooLong` or decrypt-failure log lines.

Expected:
- No errors. ChaCha20Poly1305 handles 50 MB without trouble; QUIC stream `read_to_end` cap is 64 MB so 50 MB raw + ~28 byte AEAD overhead + JSON wrapping (≈1.04× = ~52 MB) fits cleanly.

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

#### T-3.3.6 (v2 — deferred) — Descriptor + auto-fetch path

When the §3.3 v2 work lands, run:

1. Configure `max_auto_download_size = 50 MB` (default).
2. Copy a 25 MB image. Verify the receive UX is **"Downloading data… 12 MB / 25 MB"** progress notification, then **"Data available to paste"** — paste works after the second notification.
3. Copy a 30 MB image while the previous download is still in flight on the receiver. Verify the older download is abandoned (newest-copy-wins race rule), the new descriptor is fetched, and the second image ends up on the clipboard.
4. Configure `max_auto_download_size = 20 MB` and copy a 25 MB image. Verify the receiver routes through the file-transfer path (Tier 3) — the bytes land as a file in the configured download dir, **not** on the clipboard. Notification text reflects this.

Expected: all three Tier 2 / Tier 3 / race scenarios behave as described in §3.3.
