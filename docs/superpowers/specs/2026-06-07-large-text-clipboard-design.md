# Large plain-text clipboard sync (out-of-band) — design

Date: 2026-06-07

## Problem

Plain-text clipboard sync is the only clipboard path with **no size cap**. Rich
text caps at 16 MB, images cap and switch to an out-of-band transfer above
10 MB, but plain text is inlined as a JSON string inside
`Message::Clipboard(ClipboardPayload)` regardless of size. Consequences:

- A large text payload (e.g. a 31 MB log) is cloned 6–8× across the
  decrypt → handle → broadcast-relay → echo-guard → worker pipeline, plus a 2×
  expansion to UTF-16 on the Windows clipboard write — a memory storm.
- Anything approaching the 64 MB transport cap (`MESSAGE_BYTE_CAP`) is dropped
  mid-stream as `TooLong`, silently.

The Windows receiver heap-corruption crash that motivated this investigation
was a **separate** concurrency bug, already fixed on `main`
(commit `9f7df80`). This work is **not** about crash-safety; it is about
handling genuinely large text gracefully — a large text file is a legitimate
thing to copy — without the memory storm or a silent drop.

## Goals

- Sync large plain text (up to a sane ceiling) without dropping it.
- Preserve the paste experience exactly: the recipient hits **CTRL+V and gets
  text**, never a file.
- Bound resource use so a pathological paste can't OOM or wedge the receiver.
- Reuse the existing large-content machinery; avoid a parallel transport.

## Non-goals

- Fixing the History page slowness on large payloads (tracked separately).
- Changing rich-text or image behavior.
- Compressing or chunking inline (small) text — it stays exactly as today.

## Decisions (agreed during brainstorming)

1. **Approach:** route large text through the existing descriptor +
   out-of-band file-transfer path that big images use (`DeliveryTarget::Clipboard`),
   **not** the copied-files path. The streaming is a transport detail; the
   receiver still lands the bytes on the clipboard as text.
2. **Inline → out-of-band threshold:** **10 MB**, matching the image inline
   threshold (`MAX_CLIPBOARD_IMAGE_WIRE_BYTES`). 10 MB of text (~5,000 pages)
   covers essentially every real copy; only file-dump-scale text streams.
3. **Absolute ceiling:** **100 MB**. Above this the sender does not broadcast;
   it notifies "clipboard too large to share." (Chosen below the image 500 MB
   cap because text doubles to UTF-16 on the Windows clipboard write and the
   History page is heavier per byte for text.) Raisable later once History is
   fixed.
4. **History entry:** the received large text is stored in History in full,
   consistent with small text (so "re-copy from history" works). Large-row
   rendering is left to the dedicated History-perf work.

## Architecture / data flow

### Sender — `process_clipboard_change`, `ClipboardContent::Text` branch (`common.rs`)

Add the same inline-vs-descriptor decision images already use:

- `len ≤ 10 MB` → inline `ClipboardPayload` with `text`, broadcast as today.
  **No change to the common case.**
- `10 MB < len ≤ 100 MB` → stage the text as a temp blob with MIME
  `text/plain` via the existing `stage_clipboard_blob_temp_file`, and broadcast
  a **descriptor** `ClipboardBlob` (`ClipboardBlob::descriptor`, mime
  `text/plain`, `fetch_id = msg_id`, size, `width/height = None`) instead of
  inlining.
- `len > 100 MB` → do not broadcast; log + a "clipboard too large to share"
  notification surfaced in the **History** view (via `send_notification` with
  the `history` category, like other clipboard events), so there's a durable
  record of what was skipped — not just a transient toast.

The whitespace-only skip and echo-guard behavior are unchanged.

**Settings interaction (inherited, by design):** because large text rides the
file-transfer path, the *receiver* gates it exactly like a large image
descriptor ([handlers.rs:543-546](src-tauri/src/handlers.rs#L543)): it is only
fetched if `enable_file_transfer` is on, and it auto-applies only if its size
is within `max_auto_download_size` (otherwise it waits for user confirmation
via the existing UI). This is consistent with large images and is the intended
behavior — no new settings are added. (A small log line that hardcodes
"clipboard image descriptor" should be generalized to cover text.)

### Wire — unchanged machinery

The peer receives the descriptor → sends `Message::FileRequest` → the sender
streams the bytes over the `clustercut-file` ALPN with:

- `FileStreamHeader.delivery_target = DeliveryTarget::Clipboard { mime_type:
  "text/plain", width: None, height: None }`
- `FileStreamHeader.compressed = true` (zstd; text shrinks ~10×, so a 50 MB
  paste is ~5 MB on the wire). NOTE: the clipboard-blob path does not currently
  honor `compressed` on either end (it was image-only, and images aren't worth
  recompressing). This feature adds streaming zstd **encode** to the
  clipboard-blob serve path and **decode** to `handle_incoming_clipboard_blob_stream`,
  mirroring the existing disk file-transfer path (`ZstdDecoder` at
  handlers.rs:273-298; the disk encode path around handlers.rs:1171). Only
  `text/*` blobs are compressed; image blobs keep `compressed: false`.

No protocol/wire-format change: `DeliveryTarget::Clipboard` already carries
`mime_type`, and `delivery_target` defaults to `Disk` for peers that omit it,
so older peers are unaffected.

### Receiver — `handle_incoming_clipboard_blob_stream` (`handlers.rs`)

At the landing point (currently always `set_clipboard_image`), branch on
`mime_type`:

- `text/*` → `String::from_utf8(accum)`:
  - success → auto-receive on: `set_clipboard(text)`; off: stash in
    `pending_clipboard` (existing UI), exactly like the image path.
  - failure (non-UTF-8; near-impossible given mTLS + the size check) → drop
    with a warning rather than paste mojibake.
- `image/*` → `set_clipboard_image`, exactly as today.

The in-flight race guard (`in_flight_clipboard_fetch`), `file-progress`
events, and the History `clipboard-change` emission are reused unchanged. The
History event carries the full text (decision 4).

The defensive drain cap (currently `MAX_CLIPBOARD_IMAGE_BYTES` = 500 MB) becomes
MIME-aware: `text/*` caps at `MAX_CLIPBOARD_TEXT_BYTES` (100 MB) so a buggy or
hostile sender can't push 500 MB of "text."

## Constants (new, in `common.rs` beside the image caps)

- `MAX_CLIPBOARD_TEXT_WIRE_BYTES: usize = 10 * 1024 * 1024` — inline threshold.
- `MAX_CLIPBOARD_TEXT_BYTES: usize = 100 * 1024 * 1024` — absolute ceiling
  (sender skip + receiver defensive drain cap for `text/*`).

Kept separate from the image consts so the two can diverge.

## Edge cases

- **Superseded copy:** a newer clipboard event before the fetch completes →
  existing `in_flight_clipboard_fetch` guard discards the stale bytes.
- **Sender quits mid-stream:** existing file-transfer error handling.
- **Auto-receive off:** stash in `pending_clipboard`, existing confirm UI.
- **Non-UTF-8 bytes:** dropped with a warning.
- **Exactly at a boundary:** `≤ 10 MB` inlines; `> 10 MB` streams; `> 100 MB`
  skips. (Boundary is on the decoded byte length.)

## Backward compatibility

No wire-format change, and `broadcast_clipboard` sends the **same** serialized
payload to every peer (peer `protocol_version` is used only for error
reporting, not payload shaping) — identical to the existing image-descriptor
path. So text inherits the existing version story:

- A peer too old to be protocol-compatible is already flagged `compatible:
  false` (`net_util::is_protocol_compatible`) and handled by that path.
- A peer new enough to support image descriptors but predating *this* text
  feature would fetch the `text/plain` descriptor, receive
  `DeliveryTarget::Clipboard { mime_type: "text/plain" }`, and attempt
  `set_clipboard_image` on text bytes. arboard fails to decode `text/plain` as
  an image, so it logs an error and **does not paste** — a graceful soft
  failure on that one peer, not a crash or corrupted clipboard.

Whether to bump `CLUSTERCUT_PROTOCOL_VERSION` for this capability (so the
soft-failure window is closed via the compatibility flag) is a **release-time
decision left to the maintainer** — not made here, and no version is bumped as
part of this design.

## Testing

- **Unit:** the threshold decision as a pure function — 9 MB → inline,
  11 MB → descriptor, 101 MB → skip.
- **Unit:** `text/plain` `ClipboardBlob::descriptor` round-trips through JSON;
  `DeliveryTarget::Clipboard { mime_type: "text/plain", .. }` round-trips
  (extend the existing descriptor / delivery-target tests).
- **Manual / integration:** copy a 20 MB text on the sender → it pastes as text
  on the receiver (CTRL+V); copy a 150 MB text → "too large" notification, no
  sync; verify a small text still inlines unchanged.

## Touch-points

- `src-tauri/src/clipboard/common.rs` — Text-branch inline/descriptor/skip
  decision; new consts.
- `src-tauri/src/handlers.rs` — landing branch (text vs image); MIME-aware
  drain cap; streaming zstd **decode** in `handle_incoming_clipboard_blob_stream`
  when `header.compressed`; the FileRequest responder setting
  `delivery_target` + `compressed` and zstd **encoding** the stream for staged
  `text/*` blobs.
- `src-tauri/src/compression.rs` — reuse `ZSTD_LEVEL` (no change expected).
- `src-tauri/src/protocol.rs` — no change expected (reuse
  `ClipboardBlob::descriptor` + `DeliveryTarget::Clipboard`).
- Sender "too large" notification — reuse `send_notification`.

## Open questions

- **Protocol-version bump?** Optional, release-time, maintainer's call (see
  Backward compatibility). Not blocking implementation.
- The History-perf interaction (large rows) is explicitly deferred to the
  separate History work.
