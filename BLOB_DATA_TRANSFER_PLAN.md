# Clipboard Rich-Data Transfer


#### T-3.3.2 — > 10 MB image auto-fetches via descriptor (Tier B1)

> **Re-test note**: Windows previously hit a 16 MB read cap on the passthrough-image probe (the rich-text cap was being applied to image reads). Bumped to 500 MB to match `MAX_CLIPBOARD_IMAGE_BYTES`. Re-run on Windows specifically.

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
- Two notifications fire on the receiver (when `data_received` is enabled):
  - "Receiving Clipboard Image — Receiving X.Y MB image from <sender>…" at fetch start.
  - "Image Available to Paste — X.Y MB image is now on the clipboard." on completion.
- Pasting on the receiver into an image-aware app shows the original image. The history view shows the entry with a thumbnail and size.
- **No** `Clipboard image/jpeg (<bytes>) exceeds 16777216 byte cap; skipping.` warnings on the sender.

#### T-3.3.3 — > `max_auto_download_size` requires user accept (Tier B2)

> **Re-test note**: descriptors weren't surfacing in the pending UI because `blobFromPayload` returned `undefined` for empty `data`. Fixed by branching on `fetch_id` and producing a thumbnail-less preview. Notification was gated on `data_received` (default false) — switched to `notify_large_files` (default true) so the actionable accept prompt fires. Re-run on Windows + macOS.

1. Lower `max_auto_download_size` to 20 MB in Settings → File Transfer.
2. Copy a 25 MB image on the sender.
3. Observe the receiver.

Expected:
- Receiver log:
  - `Received clipboard image descriptor from <sender>: mime=…, total=…`
  - `[ClipboardBlob] Descriptor <bytes> exceeds auto-download limit <bytes> bytes — awaiting accept`
- "Large Clipboard Image — X.Y MB image from <sender> — accept to receive." actionable notification fires (default-on via `notify_large_files`).
- The descriptor appears as a pending entry in the receiver's pending-receive UI (same lower-bar as auto-receive=off). The preview shows a 🖼️ placeholder + "Large image (not yet fetched)" + size.
- User accepts via the **Apply to Clipboard** button → `confirm_pending_clipboard` triggers the fetch → bytes land on the OS clipboard, paste works.
- If the user never accepts, the descriptor sits in `pending_clipboard` until the next clipboard event displaces it. No silent drop.
