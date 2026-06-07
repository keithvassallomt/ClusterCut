# History Performance — Light Previews + Backend Re-call Store

**Date:** 2026-06-07
**Status:** Design approved, plan pending

## Problem

When a large clipboard payload (e.g. 31 MB of text, or a large image) is copied or
received, the History view takes 10–20 s to display the entry — on **both** the
sender and the receiver.

Root cause: the same `ClipboardPayload` (protocol.rs, field `text: String` + base64
`blob.data`) that travels peer-to-peer is also handed to the frontend via
`emit("clipboard-change", …)` (and the related `clipboard-monitor-update`,
`clipboard-pending`, `clipboard-blob-fetching` events, plus the large-text receiver
emit in handlers.rs). So a 31 MB payload is serialized by serde_json, shipped across
the Tauri IPC bridge, stored in full in React state, and rendered into a DOM node —
even though the History card only ever shows a ~3-line text clamp or a small image
thumbnail. The dominant cost is the serialize + IPC transit of the full payload.

The History list is in-memory React state, capped at 50 entries, non-virtualized.
Virtualization is **not** the bottleneck — payload size is.

Re-call matters: History entries have **Copy to Clipboard** and **Send to Cluster**
buttons that currently operate on the full `it.text` held in the frontend. A large
history item you cannot re-call is pointless, so truncate-and-discard is not
acceptable — the full content must remain available for re-call.

## Key insight

The peer-to-peer wire protocol (`Message::Clipboard`, inline ≤10 MB / out-of-band
descriptor >10 MB) is **separate** from the frontend `emit(...)` path. The
performance bug lives entirely in the emit path. We can fix it without touching the
peer protocol: make the frontend emit carry only a light preview, and move re-call
to backend commands keyed by the item id.

## Goals

- History displays large clipboard entries instantly (no 10–20 s stall) on both
  sender and receiver.
- Re-call (Copy to Clipboard / Send to Cluster) still works for large items, by
  reading content the backend retains — for items both **received and sent**.
- Bound retained content to a configurable budget (default **200 MB**), so memory
  and disk usage stay predictable.

## Non-goals

- No change to the peer-to-peer wire protocol or the large-text/descriptor transfer
  path (shipped separately, working).
- No persistence of history across app restart (the list is already session-scoped).
- File-transfer contents are **not** retained and do **not** count toward the budget
  (only direct clipboard text + image content does); files remain references/metadata.

## Architecture

### Data flow

At every frontend-emit site for clipboard content, instead of shipping the full
`ClipboardPayload`:

1. **Persist** the full content (text bytes or image bytes) into the backend content
   store, keyed by the payload `id`.
2. **Emit a light preview** (`ClipboardPreview`) — truncated text + true byte length
   + id, or image thumbnail + mime/dims/size + id. Never the full bytes.

Re-call buttons stop passing `it.text`; they call id-keyed backend commands that read
from the store.

Affected emit sites (all switch to the light preview): `clipboard-change`,
`clipboard-monitor-update`, `clipboard-pending`, `clipboard-blob-fetching`, and the
large-text receiver emit in `handle_incoming_clipboard_blob_stream`.

### Backend content store

A new module (e.g. `src-tauri/src/clipboard/history_store.rs`) holding an id-keyed,
**memory + disk hybrid** store:

- **Small items** (≤10 MB, the existing wire threshold): full bytes in a RAM
  `HashMap<id, StoredContent>`.
- **Large items** (>10 MB): on disk, reusing the existing `temp_downloads/` staging
  (`local_clipboard_blobs`). The store records a pointer to the staged file rather
  than duplicating bytes.
- **Budget accountant:** a single byte counter spanning both tiers. Default
  **200 MB**, configurable. On an insert that would exceed the cap, evict
  **oldest-first** until it fits. Eviction frees the RAM entry or deletes the disk
  file — but never a file an in-flight peer `FileRequest` still needs (guard via the
  existing `in_flight_clipboard_fetch` / a refcount before unlink).
- **What counts:** received **and** sent direct clipboard content (text + image).
  File transfers never enter the store and never count toward the budget.
- **Lifetime:** session-scoped. Cleared on graceful quit (a quit-time sweep) and on
  startup (extend the existing `clear_cache`, which already wipes `temp_downloads`).
  A history *preview* in the 50-item list may outlive its backing once evicted; its
  re-call buttons then disable.

### Light preview payload shape

A new struct `ClipboardPreview` replaces the full `ClipboardPayload` on the IPC bridge:

- `id`, `origin`, `sender`, `sender_id`, `timestamp` — as today.
- **Text:** `text_preview` (first ~4 KB, UTF-8-safe truncation — enough to fill the
  3-line clamp with headroom) + `text_len` (true byte count, for size display).
- **Image:** `thumbnail` (small, ~256 px max edge, base64 PNG/JPEG, a few KB) +
  `mime`, `width`, `height`, `size`. No full bytes. For a not-yet-fetched descriptor
  (auto-receive off) there is no thumbnail yet — placeholder as today; the thumbnail
  fills in after the blob is fetched.
- **Formats:** mime + binary flag + size only (already light today).
- **Files:** metadata only (unchanged).
- `has_backing: bool` — whether re-call is currently possible (false once evicted).

Thumbnails use the `image` crate (already a dependency, 0.25.x).

Frontend `HistoryItem` / `ClipboardBlobPreview` (types.ts) gain `text_len`,
`thumbnail`, `has_backing`, and stop relying on a full `text` / an object URL built
from full bytes.

### Re-call commands & UI

Two new backend commands, keyed by id, reading from the store:

- `recall_copy_history_item { id }` → write stored content to the local OS clipboard
  (text → `set_clipboard`, image → `set_clipboard_image`).
- `recall_send_history_item { id }` → re-broadcast stored content to the cluster by
  re-entering the normal `process_clipboard_change` / broadcast path, so large items
  re-descriptor correctly.

HistoryView buttons call these with `it.id` instead of `it.text`. When `has_backing`
is false, both buttons are disabled with an explanatory tooltip. `delete_history_item`
also drops the corresponding store entry.

### Settings

Add `history_store_max_bytes: u64` (default `200 * 1024 * 1024`) to `AppSettings`
(storage.rs) and the TS `AppSettings`. Surface it under **General** in SettingsView as
a MB number input ("History storage limit (MB)"). On save, the store re-reads the
budget and evicts down if the new cap is lower. Plumbed through `get_settings` /
`save_settings` like the other size settings.

## Error handling & edge cases

- **Evicted backing:** preview survives, `has_backing = false`, buttons disabled. A
  re-call command on a missing id returns a clear error (the frontend should not call
  it in this state).
- **Descriptor not yet fetched (receiver, auto-receive off):** no backing yet — the
  existing accept/fetch flow applies; thumbnail and re-call become available after the
  blob lands.
- **Peer-serve interaction:** the >10 MB disk file is shared between the peer-fetch
  path and the store pointer. Eviction must not delete a file an in-flight
  `FileRequest` still needs (guard before unlink).
- **Cap smaller than a single item:** the item is stored but is first to evict on the
  next insert; nothing is silently dropped from the *display*.
- **Cleanup:** graceful-quit sweep + startup `clear_cache` extension, both covering
  the store dir.

## Testing

- **Rust unit tests:** budget accounting (insert/evict oldest-first, cap-change
  re-eviction); tier placement (≤10 MB RAM vs >10 MB disk pointer); files excluded
  from budget; thumbnail generation produces a small, bounded image;
  eviction-respects-in-flight-fetch.
- **Round-trip:** store insert → `recall_copy` / `recall_send` reproduces the original
  bytes.
- **Manual device test:** copy 31 MB text and a large image; confirm History appears
  instantly (no 10–20 s stall) on both sender and receiver; thumbnails render;
  Copy/Send re-call works; the 200 MB cap evicts oldest-first.
