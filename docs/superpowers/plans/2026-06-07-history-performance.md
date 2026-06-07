# History Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the History view display large clipboard entries instantly by shipping a light preview to the frontend instead of the full payload, and add backend id-keyed re-call from a budgeted memory+disk content store.

**Architecture:** A new backend `HistoryStore` retains re-callable clipboard content keyed by payload id — small items (≤10 MB) in RAM, large items (>10 MB) as pointers to the existing `temp_downloads/` staging — bounded by a configurable byte budget (default 200 MB, evict oldest-first). Every frontend `emit(...)` of clipboard content is replaced by a `ClipboardPreview` (truncated text or thumbnail + metadata + id). Re-call buttons call new id-keyed commands that read the store. The peer-to-peer wire protocol is untouched.

**Tech Stack:** Rust (Tauri v2 backend), React/TypeScript frontend, `image` crate 0.25 (thumbnails, already a dep), `base64` crate (already a dep), std `HashMap`/`VecDeque` (no new crates).

**Spec:** `docs/superpowers/specs/2026-06-07-history-performance-design.md`

---

## File Structure

**New files:**
- `src-tauri/src/clipboard/history_store.rs` — the `HistoryStore` (budget + eviction), `StoredContent`, `StoredEntry`, `Evicted`. Pure, no IO; unit-tested.
- `src-tauri/src/clipboard/preview.rs` — `ClipboardPreview`/`BlobPreview`/`FormatPreview` structs, `text_preview_str`, `make_thumbnail`, `build_preview`/`preview_parts`. Unit-tested for truncation + thumbnail bounds.

**Modified backend:**
- `src-tauri/src/clipboard/mod.rs` — declare the two new modules.
- `src-tauri/src/state.rs` — add `history_store` field + init.
- `src-tauri/src/storage.rs` — add `history_store_max_bytes` setting + default.
- `src-tauri/src/commands/settings.rs` — apply the cap live on save.
- `src-tauri/src/app.rs` — set the store cap from loaded settings at startup; register the two new commands.
- `src-tauri/src/clipboard/common.rs` — `record_and_emit` helper + `stored_content_for_payload`; swap the two emits in `broadcast_clipboard`.
- `src-tauri/src/handlers.rs` — receiver: stage fetched bytes, record, emit preview (two arms).
- `src-tauri/src/commands/clipboard.rs` — swap emits in `send_clipboard`/`confirm_pending_clipboard`/`promote_pending_rich`; drop store entry in `delete_history_item`; add `recall_copy_history_item`/`recall_send_history_item`.

**Modified frontend:**
- `src/types.ts` — extend `HistoryItem`, `ClipboardBlobPreview`, `AppSettings`.
- `src/lib/protocol.ts` — `blobPreviewFromPreview` (thumbnail-based, no full-bytes `atob`).
- `src/App.tsx` — listeners read preview fields; new `history-backing-evicted` listener.
- `src/components/HistoryView.tsx` — re-call by id; disable when `!has_backing`; render thumbnail + size.
- `src/components/settings/GeneralSettings.tsx` — "History storage limit (MB)" input.

---

## Task 1: HistoryStore core (budget + eviction)

**Files:**
- Create: `src-tauri/src/clipboard/history_store.rs`
- Test: same file, `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Create `src-tauri/src/clipboard/history_store.rs` with the test module first:

```rust
//! Backend retention of re-callable clipboard content, keyed by the
//! originating `ClipboardPayload.id`. Small items live in RAM; large items
//! are pointers to files already staged under `temp_downloads/` by the
//! descriptor path. A single byte budget spans both tiers and evicts
//! oldest-first. Pure data structure — no IO, no Tauri; eviction returns the
//! affected entries so the caller can delete files and emit events.

use crate::protocol::ClipboardFormat;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

/// Re-callable content for one history item.
#[derive(Debug, Clone)]
pub enum StoredContent {
    /// Plain text held in RAM (≤ wire threshold).
    Text(String),
    /// Rich text + alternate formats held in RAM.
    Rich {
        text: String,
        formats: Vec<ClipboardFormat>,
    },
    /// Image bytes held in RAM (≤ wire threshold).
    Image {
        mime: String,
        bytes: Vec<u8>,
        width: Option<u32>,
        height: Option<u32>,
    },
    /// Large text or image staged on disk (reuses the `temp_downloads/<id>`
    /// file registered in `AppState.local_clipboard_blobs`).
    Disk {
        mime: String,
        path: PathBuf,
        width: Option<u32>,
        height: Option<u32>,
        size: u64,
    },
}

impl StoredContent {
    /// Bytes this entry charges against the budget.
    pub fn size(&self) -> u64 {
        match self {
            StoredContent::Text(s) => s.len() as u64,
            StoredContent::Rich { text, formats } => {
                text.len() as u64
                    + formats.iter().map(|f| f.data.len() as u64).sum::<u64>()
            }
            StoredContent::Image { bytes, .. } => bytes.len() as u64,
            StoredContent::Disk { size, .. } => *size,
        }
    }

    /// The on-disk file backing this entry, if any (for eviction cleanup).
    pub fn disk_path(&self) -> Option<PathBuf> {
        match self {
            StoredContent::Disk { path, .. } => Some(path.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoredEntry {
    pub content: StoredContent,
    pub size: u64,
}

/// An entry removed by eviction/delete. The caller deletes `disk_path` (if
/// any), drops the matching `local_clipboard_blobs` entry, and notifies the UI.
#[derive(Debug, Clone)]
pub struct Evicted {
    pub id: String,
    pub disk_path: Option<PathBuf>,
}

pub struct HistoryStore {
    entries: HashMap<String, StoredEntry>,
    /// Insertion order of live ids, oldest at the front (eviction order).
    order: VecDeque<String>,
    total_bytes: u64,
    max_bytes: u64,
}

impl HistoryStore {
    pub fn new(max_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub fn get(&self, id: &str) -> Option<&StoredEntry> {
        self.entries.get(id)
    }

    /// Insert (or replace) content for `id`, then evict oldest entries until
    /// the budget is satisfied. Returns the entries removed by eviction (the
    /// just-inserted id is never returned unless it alone exceeds the budget
    /// AND there is nothing older to evict, in which case it is kept — see
    /// the spec's "cap smaller than a single item" rule). The freshly
    /// inserted id is exempt from this round's eviction.
    pub fn insert(&mut self, id: String, content: StoredContent) -> Vec<Evicted> {
        // Replace semantics: a repeated id (e.g. pending → confirmed) updates.
        self.remove_internal(&id);
        let size = content.size();
        self.entries
            .insert(id.clone(), StoredEntry { content, size });
        self.order.push_back(id.clone());
        self.total_bytes += size;
        self.evict_to_budget(Some(&id))
    }

    /// Remove a single entry by id (e.g. on delete_history_item).
    pub fn remove(&mut self, id: &str) -> Option<Evicted> {
        self.remove_internal(id)
    }

    /// Lower/raise the budget; evict down if needed. Returns evicted entries.
    pub fn set_max_bytes(&mut self, max_bytes: u64) -> Vec<Evicted> {
        self.max_bytes = max_bytes;
        self.evict_to_budget(None)
    }

    fn remove_internal(&mut self, id: &str) -> Option<Evicted> {
        if let Some(entry) = self.entries.remove(id) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.size);
            if let Some(pos) = self.order.iter().position(|x| x == id) {
                self.order.remove(pos);
            }
            Some(Evicted {
                id: id.to_string(),
                disk_path: entry.content.disk_path(),
            })
        } else {
            None
        }
    }

    /// Evict from the front (oldest) until within budget. `exempt` is never
    /// evicted in this pass (the just-inserted id).
    fn evict_to_budget(&mut self, exempt: Option<&str>) -> Vec<Evicted> {
        let mut evicted = Vec::new();
        while self.total_bytes > self.max_bytes {
            // Find the oldest id that isn't exempt.
            let victim = self
                .order
                .iter()
                .find(|id| Some(id.as_str()) != exempt)
                .cloned();
            match victim {
                Some(id) => {
                    if let Some(e) = self.remove_internal(&id) {
                        evicted.push(e);
                    }
                }
                // Only the exempt entry remains and it alone exceeds budget —
                // keep it (don't drop the content the user just copied).
                None => break,
            }
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(n: usize) -> StoredContent {
        StoredContent::Text("x".repeat(n))
    }

    #[test]
    fn insert_accounts_bytes() {
        let mut s = HistoryStore::new(1000);
        assert!(s.insert("a".into(), text(100)).is_empty());
        assert_eq!(s.total_bytes(), 100);
        assert!(s.get("a").is_some());
    }

    #[test]
    fn evicts_oldest_first_over_budget() {
        let mut s = HistoryStore::new(250);
        s.insert("a".into(), text(100));
        s.insert("b".into(), text(100));
        let evicted = s.insert("c".into(), text(100)); // 300 > 250
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, "a");
        assert!(s.get("a").is_none());
        assert!(s.get("b").is_some());
        assert!(s.get("c").is_some());
        assert_eq!(s.total_bytes(), 200);
    }

    #[test]
    fn replace_same_id_updates_size_and_order() {
        let mut s = HistoryStore::new(1000);
        s.insert("a".into(), text(100));
        s.insert("a".into(), text(300));
        assert_eq!(s.total_bytes(), 300);
        assert_eq!(s.order.len(), 1);
    }

    #[test]
    fn lowering_cap_evicts_down() {
        let mut s = HistoryStore::new(1000);
        s.insert("a".into(), text(100));
        s.insert("b".into(), text(100));
        let evicted = s.set_max_bytes(150);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, "a");
        assert_eq!(s.total_bytes(), 100);
    }

    #[test]
    fn oversized_single_item_is_kept() {
        let mut s = HistoryStore::new(50);
        let evicted = s.insert("big".into(), text(500));
        assert!(evicted.is_empty());
        assert!(s.get("big").is_some());
    }

    #[test]
    fn remove_returns_disk_path_for_disk_entries() {
        let mut s = HistoryStore::new(1000);
        s.insert(
            "d".into(),
            StoredContent::Disk {
                mime: "image/png".into(),
                path: PathBuf::from("/tmp/d.png"),
                width: None,
                height: None,
                size: 100,
            },
        );
        let e = s.remove("d").unwrap();
        assert_eq!(e.disk_path, Some(PathBuf::from("/tmp/d.png")));
    }
}
```

- [ ] **Step 2: Declare the module so it compiles**

In `src-tauri/src/clipboard/mod.rs`, add alongside the other `mod`/`pub mod` lines near the top:

```rust
pub mod history_store;
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib history_store`
Expected: 6 tests pass (`insert_accounts_bytes`, `evicts_oldest_first_over_budget`, `replace_same_id_updates_size_and_order`, `lowering_cap_evicts_down`, `oversized_single_item_is_kept`, `remove_returns_disk_path_for_disk_entries`).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/clipboard/history_store.rs src-tauri/src/clipboard/mod.rs
git commit -m "feat(history): budgeted memory+disk content store with oldest-first eviction"
```

---

## Task 2: Settings field + store wired into AppState

**Files:**
- Modify: `src-tauri/src/storage.rs:507-545` (struct), `:555-577` (Default)
- Modify: `src-tauri/src/state.rs:33-148` (field), `:152-190` (init)
- Modify: `src-tauri/src/clipboard/mod.rs` (re-export type if needed)
- Modify: `src-tauri/src/app.rs:587-588` (apply cap from loaded settings)
- Modify: `src-tauri/src/commands/settings.rs:12-75` (apply cap live on save)

- [ ] **Step 1: Add the setting to the Rust struct**

In `src-tauri/src/storage.rs`, inside `pub struct AppSettings` (after `mdns_advertising` at line 544), add:

```rust
    /// Max bytes of re-callable clipboard content (text + images) the History
    /// content store retains, across RAM + disk tiers. File transfers don't
    /// count. Default 200 MB; oldest entries evict first when exceeded.
    #[serde(default = "default_history_store_max_bytes")]
    pub history_store_max_bytes: u64,
```

Add the default fn next to `default_true` (after line 553):

```rust
fn default_history_store_max_bytes() -> u64 {
    200 * 1024 * 1024
}
```

And in `impl Default for AppSettings` (after `mdns_advertising: true,` at line 574):

```rust
            history_store_max_bytes: 200 * 1024 * 1024,
```

- [ ] **Step 2: Add the store to AppState**

In `src-tauri/src/state.rs`, add a field after `in_flight_clipboard_fetch` (line 99):

```rust
    /// Re-callable clipboard content for the History view, keyed by payload
    /// id. Budgeted; see `clipboard::history_store`.
    pub history_store: Arc<Mutex<crate::clipboard::history_store::HistoryStore>>,
```

In `impl AppState::new()` (after `in_flight_clipboard_fetch:` at line 174), add:

```rust
            history_store: Arc::new(Mutex::new(
                crate::clipboard::history_store::HistoryStore::new(
                    crate::storage::AppSettings::default().history_store_max_bytes,
                ),
            )),
```

- [ ] **Step 3: Apply the loaded cap at startup**

In `src-tauri/src/app.rs`, immediately after line 588 (`*settings_lock = load_settings(app_handle);`), add:

```rust
                state
                    .history_store
                    .lock()
                    .unwrap()
                    .set_max_bytes(settings_lock.history_store_max_bytes);
```

- [ ] **Step 4: Apply the cap live when settings are saved**

In `src-tauri/src/commands/settings.rs`, inside `save_settings`, after line 28 (`*state.settings.lock().unwrap() = settings.clone();`), add:

```rust
    // Re-budget the History content store (may evict down on a lower cap).
    // Evicted disk files are cleaned by the next eviction/exit sweep; we only
    // need the in-memory budget to shrink immediately here.
    let _evicted = state
        .history_store
        .lock()
        .unwrap()
        .set_max_bytes(settings.history_store_max_bytes);
```

- [ ] **Step 5: Verify it builds**

Run: `cd src-tauri && cargo build`
Expected: compiles (warnings OK). The `_evicted` disk files from a live cap-lower are handled lazily; full cleanup happens in Task 5/exit.

- [ ] **Step 6: Add the TS setting field**

In `src/types.ts`, inside `interface AppSettings` (after `mdns_advertising: boolean;` at line 102), add:

```typescript
  history_store_max_bytes: number; // bytes; History content store budget
```

- [ ] **Step 7: Add the Settings UI input**

In `src/components/settings/GeneralSettings.tsx`, inside the General card, after the "Start on Startup" block (after line 45's closing `</div>` of that row, before the card's closing `</div>` at line 45-46), add a new row:

```tsx
          <div className="mt-4 flex flex-col gap-1">
            <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">
              History storage limit (MB)
            </label>
            <input
              type="number"
              min={0}
              className="h-10 w-40 rounded-xl border border-zinc-900/10 bg-white px-3 text-sm text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-white/5 dark:text-zinc-50"
              value={Math.round(settings.history_store_max_bytes / (1024 * 1024))}
              onChange={(e) => {
                const mb = Math.max(0, parseInt(e.target.value || "0", 10));
                setSettings({ ...settings, history_store_max_bytes: mb * 1024 * 1024 });
              }}
            />
            <div className="text-[10px] text-zinc-500">
              How much copied text &amp; image content History keeps for re-copying. Files don&apos;t count.
            </div>
          </div>
```

- [ ] **Step 8: Verify the frontend typechecks**

Run: `npm run build` (or `npx tsc --noEmit`)
Expected: no type errors. (The backend preserves unknown fields, and `get_settings` now returns `history_store_max_bytes`, so the round-trip is complete.)

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/storage.rs src-tauri/src/state.rs src-tauri/src/app.rs src-tauri/src/commands/settings.rs src/types.ts src/components/settings/GeneralSettings.tsx
git commit -m "feat(history): add configurable history_store_max_bytes setting (default 200MB)"
```

---

## Task 3: ClipboardPreview + thumbnail/text-preview helpers

**Files:**
- Create: `src-tauri/src/clipboard/preview.rs`
- Modify: `src-tauri/src/clipboard/mod.rs` (declare module)
- Test: same file, `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the helper module with failing tests**

Create `src-tauri/src/clipboard/preview.rs`:

```rust
//! Light-weight previews sent to the frontend in place of the full
//! `ClipboardPayload`. The frontend renders a 3-line text clamp or a small
//! image thumbnail and re-calls full content by id — so it never needs the
//! whole payload across the IPC bridge.

use crate::clipboard::history_store::StoredContent;
use crate::protocol::ClipboardPayload;
use base64::Engine as _;
use serde::Serialize;
use std::io::Cursor;

/// Max bytes of text shipped for the preview. Enough to fill the 3-line clamp
/// with headroom; truncated on a UTF-8 char boundary.
pub const TEXT_PREVIEW_BYTES: usize = 4096;

/// Max edge (px) of the generated image thumbnail.
pub const THUMB_MAX_EDGE: u32 = 256;

#[derive(Serialize, Clone, Debug)]
pub struct BlobPreview {
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size: u64,
    /// base64 PNG thumbnail, or None for a not-yet-fetched descriptor.
    pub thumbnail: Option<String>,
    /// True when bytes aren't available yet (receiver descriptor pre-fetch).
    pub descriptor: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct FormatPreview {
    pub mime_type: String,
    pub binary: bool,
    pub size: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct ClipboardPreview {
    pub id: String,
    pub sender: String,
    pub sender_id: String,
    pub timestamp: u64,
    /// Truncated text (text/rich items). None for image-only items.
    pub text_preview: Option<String>,
    /// True byte length of the full text (for "31.0 MB" display).
    pub text_len: u64,
    pub blob: Option<BlobPreview>,
    pub formats: Option<Vec<FormatPreview>>,
    pub files: Option<Vec<crate::protocol::FileMetadata>>,
    /// Whether re-call is currently possible (content still in the store).
    pub has_backing: bool,
}

/// UTF-8-safe truncation to TEXT_PREVIEW_BYTES.
pub fn text_preview_str(s: &str) -> String {
    if s.len() <= TEXT_PREVIEW_BYTES {
        return s.to_string();
    }
    let mut end = TEXT_PREVIEW_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Decode an image and emit a small base64 PNG thumbnail. Returns None if the
/// bytes don't decode as an image.
pub fn make_thumbnail(bytes: &[u8]) -> Option<String> {
    let img = image::load_from_memory(bytes).ok()?;
    let thumb = img.thumbnail(THUMB_MAX_EDGE, THUMB_MAX_EDGE);
    let mut out = Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(out.get_ref()))
}

/// (text_preview, text_len, blob_preview) for a backed StoredContent.
/// `thumbnail` is computed by the caller (it has the bytes / file).
pub fn preview_parts(
    content: &StoredContent,
    thumbnail: Option<String>,
) -> (Option<String>, u64, Option<BlobPreview>) {
    match content {
        StoredContent::Text(s) => (Some(text_preview_str(s)), s.len() as u64, None),
        StoredContent::Rich { text, .. } => {
            (Some(text_preview_str(text)), text.len() as u64, None)
        }
        StoredContent::Image {
            mime,
            bytes,
            width,
            height,
        } => (
            None,
            0,
            Some(BlobPreview {
                mime_type: mime.clone(),
                width: *width,
                height: *height,
                size: bytes.len() as u64,
                thumbnail,
                descriptor: false,
            }),
        ),
        StoredContent::Disk {
            mime,
            width,
            height,
            size,
            ..
        } => {
            if mime.starts_with("text/") {
                // Large text: caller passes the file prefix as `thumbnail`'s
                // sibling via a separate read; here we have no text bytes, so
                // text_preview is filled by the caller for Disk-text. We
                // return None and let the caller override when it read a
                // prefix. text_len is the true size.
                (None, *size, None)
            } else {
                (
                    None,
                    0,
                    Some(BlobPreview {
                        mime_type: mime.clone(),
                        width: *width,
                        height: *height,
                        size: *size,
                        thumbnail,
                        descriptor: true,
                    }),
                )
            }
        }
    }
}

/// Build the descriptor preview for a payload with NO backing yet (receiver
/// pending / fetching). Large-text descriptors render as text (size only);
/// image descriptors render as a thumbnail-less blob.
pub fn descriptor_preview(payload: &ClipboardPayload) -> (Option<String>, u64, Option<BlobPreview>) {
    if let Some(b) = payload.blob.as_ref() {
        if b.mime_type.starts_with("text/") {
            return (None, b.total_size.unwrap_or(0), None);
        }
        return (
            None,
            0,
            Some(BlobPreview {
                mime_type: b.mime_type.clone(),
                width: b.width,
                height: b.height,
                size: b.total_size.unwrap_or(0),
                thumbnail: None,
                descriptor: true,
            }),
        );
    }
    // No blob: inline text/rich with no backing only happens for empty content.
    (
        if payload.text.is_empty() {
            None
        } else {
            Some(text_preview_str(&payload.text))
        },
        payload.text.len() as u64,
        None,
    )
}

/// Build the light format summary (no bytes).
pub fn formats_preview(payload: &ClipboardPayload) -> Option<Vec<FormatPreview>> {
    payload.formats.as_ref().filter(|f| !f.is_empty()).map(|fs| {
        fs.iter()
            .map(|f| FormatPreview {
                mime_type: f.mime_type.clone(),
                binary: f.binary,
                size: f.data.len() as u64,
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_preview_passes_short_text() {
        assert_eq!(text_preview_str("hello"), "hello");
    }

    #[test]
    fn text_preview_truncates_long_text_on_char_boundary() {
        let s = "é".repeat(5000); // 2 bytes each = 10000 bytes
        let p = text_preview_str(&s);
        assert!(p.len() <= TEXT_PREVIEW_BYTES);
        // Did not split a multi-byte char:
        assert!(p.chars().all(|c| c == 'é'));
    }

    #[test]
    fn thumbnail_of_small_png_is_bounded() {
        // 1x1 transparent PNG.
        let png: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let t = make_thumbnail(png);
        assert!(t.is_some());
        assert!(!t.unwrap().is_empty());
    }

    #[test]
    fn thumbnail_of_garbage_is_none() {
        assert!(make_thumbnail(b"not an image").is_none());
    }
}
```

- [ ] **Step 2: Declare the module**

In `src-tauri/src/clipboard/mod.rs`, add:

```rust
pub mod preview;
```

- [ ] **Step 3: Run the tests**

Run: `cd src-tauri && cargo test --lib preview`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/clipboard/preview.rs src-tauri/src/clipboard/mod.rs
git commit -m "feat(history): ClipboardPreview + thumbnail/text-preview helpers"
```

---

## Task 4: record_and_emit helper + sender emit sites

**Files:**
- Modify: `src-tauri/src/clipboard/common.rs` (add helpers; swap emits in `broadcast_clipboard` at :991 and :995)
- Modify: `src-tauri/src/commands/clipboard.rs` (swap emits in `send_clipboard:39`, `confirm_pending_clipboard:184`, `promote_pending_rich:237`)

This task introduces the central helper and routes all SENDER-side emits through it. (The receiver side is Task 4b below within the same task block.)

- [ ] **Step 1: Add `stored_content_for_payload` and `record_and_emit` to common.rs**

In `src-tauri/src/clipboard/common.rs`, add these public functions (place them just above `pub fn broadcast_clipboard` at line 982). Note the new imports at the top of the file — add to the existing `use` block:

```rust
use crate::clipboard::history_store::{Evicted, StoredContent};
use crate::clipboard::preview::{
    descriptor_preview, formats_preview, make_thumbnail, preview_parts, text_preview_str,
    ClipboardPreview,
};
```

Functions:

```rust
/// Derive the re-callable `StoredContent` (and an optional base64 thumbnail)
/// for a payload about to be emitted to the frontend. Returns `None` when the
/// payload has no re-callable backing: a files-only payload, or a descriptor
/// whose bytes aren't staged locally (receiver pending pre-fetch).
pub fn stored_content_for_payload(
    state: &AppState,
    payload: &ClipboardPayload,
) -> Option<(StoredContent, Option<String>)> {
    if let Some(blob) = payload.blob.as_ref() {
        if blob.is_descriptor() {
            // Bytes live on disk in local_clipboard_blobs (sender) — point at
            // the staged file. If it isn't staged here, there's no backing.
            let meta = {
                let map = state.local_clipboard_blobs.lock().unwrap();
                map.get(&payload.id).cloned()
            }?;
            let thumb = if meta.mime_type.starts_with("image/") {
                std::fs::read(&meta.path).ok().and_then(|b| make_thumbnail(&b))
            } else {
                None
            };
            return Some((
                StoredContent::Disk {
                    mime: meta.mime_type,
                    path: meta.path,
                    width: meta.width,
                    height: meta.height,
                    size: meta.total_size,
                },
                thumb,
            ));
        }
        // Inline image.
        let bytes = blob.raw_bytes().ok()?;
        let thumb = make_thumbnail(&bytes);
        return Some((
            StoredContent::Image {
                mime: blob.mime_type.clone(),
                bytes,
                width: blob.width,
                height: blob.height,
            },
            thumb,
        ));
    }
    if let Some(formats) = payload.formats.as_ref().filter(|f| !f.is_empty()) {
        return Some((
            StoredContent::Rich {
                text: payload.text.clone(),
                formats: formats.clone(),
            },
            None,
        ));
    }
    if !payload.text.is_empty() {
        return Some((StoredContent::Text(payload.text.clone()), None));
    }
    None
}

/// For a large-text Disk entry, read up to TEXT_PREVIEW_BYTES from the staged
/// file so History can still show a snippet.
fn disk_text_prefix(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; crate::clipboard::preview::TEXT_PREVIEW_BYTES];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    // Drop a possibly-split trailing char.
    Some(String::from_utf8_lossy(&buf).to_string())
}

/// Apply the side effects of an eviction: delete the disk file (unless an
/// in-flight fetch still needs it), drop the local_clipboard_blobs entry, and
/// tell the UI the item's backing is gone.
fn handle_evictions(app: &AppHandle, state: &AppState, evicted: Vec<Evicted>) {
    for e in evicted {
        {
            let mut map = state.local_clipboard_blobs.lock().unwrap();
            map.remove(&e.id);
        }
        if let Some(path) = e.disk_path {
            let in_flight = {
                let slot = state.in_flight_clipboard_fetch.lock().unwrap();
                slot.as_deref() == Some(e.id.as_str())
            };
            if !in_flight {
                let _ = std::fs::remove_file(&path);
            }
        }
        let _ = app.emit("history-backing-evicted", &e.id);
    }
}

/// Persist a payload's content into the History store and emit a light
/// `ClipboardPreview` on `event` (replacing the old full-payload emit).
pub fn record_and_emit(
    app: &AppHandle,
    state: &AppState,
    event: &str,
    payload: &ClipboardPayload,
) {
    let mut evicted = Vec::new();
    let (text_preview, text_len, blob, has_backing) =
        match stored_content_for_payload(state, payload) {
            Some((content, thumb)) => {
                let (mut tp, tl, bp) = preview_parts(&content, thumb);
                // Large-text Disk entry: fill the snippet from the staged file.
                if tp.is_none() && bp.is_none() {
                    if let StoredContent::Disk { path, .. } = &content {
                        tp = disk_text_prefix(path);
                    }
                }
                evicted = state.history_store.lock().unwrap().insert(payload.id.clone(), content);
                (tp, tl, bp, true)
            }
            None => {
                let (tp, tl, bp) = descriptor_preview(payload);
                (tp, tl, bp, false)
            }
        };

    handle_evictions(app, state, evicted);

    let preview = ClipboardPreview {
        id: payload.id.clone(),
        sender: payload.sender.clone(),
        sender_id: payload.sender_id.clone(),
        timestamp: payload.timestamp,
        text_preview,
        text_len,
        blob,
        formats: formats_preview(payload),
        files: payload.files.clone(),
        has_backing,
    };
    let _ = app.emit(event, &preview);
}
```

- [ ] **Step 2: Swap the emits in `broadcast_clipboard`**

In `src-tauri/src/clipboard/common.rs`, replace line 991:

```rust
        let _ = app_handle.emit("clipboard-monitor-update", &payload_obj);
```

with:

```rust
        record_and_emit(app_handle, state, "clipboard-monitor-update", &payload_obj);
```

And replace line 995:

```rust
    let _ = app_handle.emit("clipboard-change", &payload_obj);
```

with:

```rust
    record_and_emit(app_handle, state, "clipboard-change", &payload_obj);
```

- [ ] **Step 3: Swap the emit in `send_clipboard`**

In `src-tauri/src/commands/clipboard.rs`, replace line 39:

```rust
    let _ = app_handle.emit("clipboard-change", &payload_obj);
```

with:

```rust
    crate::clipboard::common::record_and_emit(&app_handle, &state, "clipboard-change", &payload_obj);
```

(`state` is the `State<'_, AppState>`; `record_and_emit` takes `&AppState`, so pass `&state`. `State` derefs to `AppState`, so `&state` coerces — if the borrow checker complains, use `&*state`.)

- [ ] **Step 4: Swap the emit in `confirm_pending_clipboard`**

In `src-tauri/src/commands/clipboard.rs`, replace line 184:

```rust
        let _ = app_handle.emit("clipboard-change", &payload);
```

with:

```rust
        crate::clipboard::common::record_and_emit(&app_handle, &state, "clipboard-change", &payload);
```

- [ ] **Step 5: Swap the emit in `promote_pending_rich`**

In `src-tauri/src/commands/clipboard.rs`, replace line 237:

```rust
    let _ = app_handle.emit("clipboard-change", &payload);
```

with:

```rust
    crate::clipboard::common::record_and_emit(&app_handle, &state, "clipboard-change", &payload);
```

- [ ] **Step 6: Receiver inbound-message emits (`handle_message`)**

The inbound `Message::Clipboard(payload)` handler (`src-tauri/src/handlers.rs:417`, fn `handle_message`) receives inline small payloads from peers and emits the full `payload_obj` at multiple sites. They all share the same two locals — `listener_handle: tauri::AppHandle` and `listener_state: AppState` — and the same payload variable `payload_obj`. Replace **every** emit in this function with `record_and_emit`. The sites and their events (verified) are:

| Line | Event |
|------|-------|
| 508 | `clipboard-change` (inline text/files received) |
| 613 | `clipboard-pending` (descriptor, auto-receive off) |
| 645 | `clipboard-pending` |
| 676 | `clipboard-blob-fetching` (descriptor auto-fetch) |
| 723 | `clipboard-change` (inline image applied) |
| 730 | `clipboard-pending` |
| 810 | `clipboard-change` (rich applied, plain) |
| 813 | `clipboard-change` (rich applied, formats) |
| 821 | `clipboard-pending` |
| 874 | `clipboard-change` |
| 882 | `clipboard-pending` |

For each, replace:

```rust
                                        let _ = listener_handle.emit("<event>", &payload_obj);
```

with:

```rust
                                        crate::clipboard::common::record_and_emit(&listener_handle, &listener_state, "<event>", &payload_obj);
```

keeping the exact `<event>` string and indentation at each site. Inline-received items (`clipboard-change`) carry their bytes in `payload_obj`, so `record_and_emit` records real backing. Descriptor `clipboard-pending`/`clipboard-blob-fetching` sites have no local backing yet → `has_backing=false` automatically (their bytes land later via the fetch path in Task 4b). Confirm none are missed:

```bash
cd src-tauri && grep -n 'emit("clipboard-change"\|emit("clipboard-pending"\|emit("clipboard-blob-fetching"' src/handlers.rs
```

After this step, lines 177 and 209 (inside `handle_incoming_clipboard_blob_stream`) are the ONLY remaining `emit("clipboard-change", …)` calls — those are rewritten in Task 4b.

- [ ] **Step 7: Build**

Run: `cd src-tauri && cargo build`
Expected: compiles. (The two `handle_incoming_clipboard_blob_stream` emits at 177/209 are handled in Task 4b next — they still emit the full payload until then, which is correct but heavy.)

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/clipboard/common.rs src-tauri/src/commands/clipboard.rs src-tauri/src/handlers.rs
git commit -m "feat(history): route sender + pending emits through record_and_emit (light previews)"
```

---

## Task 4b: Receiver — stage fetched bytes, record, emit preview

**Files:**
- Modify: `src-tauri/src/handlers.rs:146-218` (`handle_incoming_clipboard_blob_stream` — both arms)

The receiver drains a fetched blob into `accum`. To make it re-callable (the spec retains received items too) we stage it like the sender does, register it in `local_clipboard_blobs`, insert a `Disk` entry, and emit a light preview.

- [ ] **Step 1: Add a receiver staging helper**

In `src-tauri/src/handlers.rs`, add a helper near the top of the file (after the imports, before `handle_incoming_clipboard_blob_stream`):

```rust
/// Stage received clipboard-blob bytes under `temp_downloads/<id>.<ext>` and
/// register them in `local_clipboard_blobs`, so this receiver can re-copy /
/// re-send the item from History. Mirrors the sender's
/// `stage_clipboard_blob_temp_file` but works from in-memory bytes we already
/// drained. Returns the staged path on success.
fn stage_received_clipboard_blob(
    app: &tauri::AppHandle,
    state: &AppState,
    id: &str,
    mime_type: &str,
    width: Option<u32>,
    height: Option<u32>,
    bytes: &[u8],
) -> Option<std::path::PathBuf> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .ok()?
        .join("temp_downloads");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let ext = crate::clipboard::common::extension_for_clipboard_mime(mime_type);
    let path = cache_dir.join(format!("{}.{}", id, ext));
    std::fs::write(&path, bytes).ok()?;
    state.local_clipboard_blobs.lock().unwrap().insert(
        id.to_string(),
        crate::state::ClipboardBlobMetadata {
            path: path.clone(),
            mime_type: mime_type.to_string(),
            width,
            height,
            total_size: bytes.len() as u64,
        },
    );
    Some(path)
}
```

Confirm `extension_for_clipboard_mime` is `pub` in common.rs:

```bash
cd src-tauri && grep -n "fn extension_for_clipboard_mime" src/clipboard/common.rs
```

If it is not `pub`, change `fn extension_for_clipboard_mime` to `pub fn extension_for_clipboard_mime` in `src/clipboard/common.rs`.

- [ ] **Step 2: Record + light-emit in the text arm**

In `handle_incoming_clipboard_blob_stream`, the text branch currently (lines 158-177) builds `payload_event` with the full `text` and ends with `let _ = app.emit("clipboard-change", &payload_event);`. Replace the emit (line 177) and stage just before it. The text was `text.clone()`d into `payload_event`; we still pass the full text in `payload_event` to `record_and_emit` (it truncates), but we also need a `Disk` backing. Replace the block from the `payload_event` construction through the emit with:

```rust
        // Stage for re-call, then emit a light preview (record_and_emit reads
        // the staged file via local_clipboard_blobs to build the Disk entry).
        let _ = stage_received_clipboard_blob(
            &app, &state, &header.id, &mime_type, None, None, text.as_bytes(),
        );
        let payload_event = crate::protocol::ClipboardPayload {
            id: header.id.clone(),
            text: String::new(),
            files: None,
            blob: Some(crate::protocol::ClipboardBlob::descriptor(
                mime_type.clone(),
                header.id.clone(),
                text.len() as u64,
                None,
                None,
            )),
            formats: None,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: format!("{}", addr),
            sender_id: String::new(),
        };
        if auto_recv {
            crate::clipboard::set_clipboard(&app, text);
        } else {
            let mut pending = state.pending_clipboard.lock().unwrap();
            *pending = Some(payload_event.clone());
        }
        crate::clipboard::common::record_and_emit(&app, &state, "clipboard-change", &payload_event);
```

Note: we set the OS clipboard from the real `text`, but the emitted payload is a `text/plain` descriptor pointing at the staged file. `record_and_emit` → `stored_content_for_payload` sees the descriptor, finds it in `local_clipboard_blobs`, builds a `Disk` text entry, and `record_and_emit` fills the snippet from the file prefix. `text_len` comes from `total_size`.

- [ ] **Step 3: Record + light-emit in the image arm**

In the image branch (lines 186-209), it currently builds a `ClipboardBlob::from_bytes` inline blob into `payload_event` and emits it (line 209). Replace the staging + emit so the receiver retains a `Disk` entry and ships a thumbnail. Replace from the `blob` construction through the emit with:

```rust
        let staged = stage_received_clipboard_blob(
            &app, &state, &header.id, &mime_type, width, height, &accum,
        );
        // Land the image on the OS clipboard (or stash as pending).
        let blob = crate::protocol::ClipboardBlob::from_bytes(mime_type.clone(), &accum, width, height);
        let payload_event = if staged.is_some() {
            // Descriptor payload → record_and_emit builds a Disk entry +
            // thumbnail from the staged file.
            crate::protocol::ClipboardPayload {
                id: header.id.clone(),
                text: String::new(),
                files: None,
                blob: Some(crate::protocol::ClipboardBlob::descriptor(
                    mime_type.clone(),
                    header.id.clone(),
                    accum.len() as u64,
                    width,
                    height,
                )),
                formats: None,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                sender: format!("{}", addr),
                sender_id: String::new(),
            }
        } else {
            // Staging failed — fall back to inline so History still shows it.
            crate::protocol::ClipboardPayload {
                id: header.id.clone(),
                text: String::new(),
                files: None,
                blob: Some(blob.clone()),
                formats: None,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                sender: format!("{}", addr),
                sender_id: String::new(),
            }
        };
        if auto_recv {
            crate::clipboard::set_clipboard_image(&app, blob);
        } else {
            let mut pending = state.pending_clipboard.lock().unwrap();
            *pending = Some(payload_event.clone());
        }
        crate::clipboard::common::record_and_emit(&app, &state, "clipboard-change", &payload_event);
```

- [ ] **Step 4: Build**

Run: `cd src-tauri && cargo build`
Expected: compiles. (`accum` is moved into `from_bytes`/`String::from_utf8` — in the image arm `from_bytes` borrows `&accum`, and `stage_received_clipboard_blob` borrows `&accum`; order them so the borrows end before any move. The text arm moves `text` into `set_clipboard`; stage uses `text.as_bytes()` before that.)

- [ ] **Step 5: Run all lib tests**

Run: `cd src-tauri && cargo test --lib`
Expected: all existing tests + the Task 1/3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/handlers.rs src-tauri/src/clipboard/common.rs
git commit -m "feat(history): receiver stages fetched blobs + emits light preview"
```

---

## Task 5: Re-call commands + delete cleanup

**Files:**
- Modify: `src-tauri/src/commands/clipboard.rs` (add `recall_copy_history_item`, `recall_send_history_item`; extend `delete_history_item:100-124`)
- Modify: `src-tauri/src/app.rs:1167-1168` (register commands)

- [ ] **Step 1: Add a content→clipboard reconstruction helper (TDD)**

The re-call commands share logic: turn a `StoredContent` into bytes/text to write or broadcast. Add a pure helper to `src-tauri/src/clipboard/history_store.rs` with a test. Append to that file (before `#[cfg(test)]`):

```rust
/// What to do with a retrieved entry: the concrete text or image bytes.
/// `recall_*` commands turn this into clipboard writes / broadcasts.
#[derive(Debug, Clone)]
pub enum RecalledContent {
    Text(String),
    Rich {
        text: String,
        formats: Vec<ClipboardFormat>,
    },
    Image {
        mime: String,
        bytes: Vec<u8>,
        width: Option<u32>,
        height: Option<u32>,
    },
}

impl StoredContent {
    /// Materialize the content for re-call. Disk entries are read from their
    /// staged file (text decoded as UTF-8, images kept as raw bytes).
    pub fn recall(&self) -> Result<RecalledContent, String> {
        match self {
            StoredContent::Text(s) => Ok(RecalledContent::Text(s.clone())),
            StoredContent::Rich { text, formats } => Ok(RecalledContent::Rich {
                text: text.clone(),
                formats: formats.clone(),
            }),
            StoredContent::Image {
                mime,
                bytes,
                width,
                height,
            } => Ok(RecalledContent::Image {
                mime: mime.clone(),
                bytes: bytes.clone(),
                width: *width,
                height: *height,
            }),
            StoredContent::Disk {
                mime,
                path,
                width,
                height,
                ..
            } => {
                let bytes = std::fs::read(path)
                    .map_err(|e| format!("read staged content {:?}: {}", path, e))?;
                if mime.starts_with("text/") {
                    let text = String::from_utf8(bytes)
                        .map_err(|e| format!("staged text not UTF-8: {}", e))?;
                    Ok(RecalledContent::Text(text))
                } else {
                    Ok(RecalledContent::Image {
                        mime: mime.clone(),
                        bytes,
                        width: *width,
                        height: *height,
                    })
                }
            }
        }
    }
}
```

Add a test inside the existing `mod tests`:

```rust
    #[test]
    fn recall_text_roundtrips() {
        let c = StoredContent::Text("hello world".into());
        match c.recall().unwrap() {
            RecalledContent::Text(t) => assert_eq!(t, "hello world"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn recall_disk_text_reads_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("cc_recall_test_{}.txt", std::process::id()));
        std::fs::write(&path, b"disk text").unwrap();
        let c = StoredContent::Disk {
            mime: "text/plain".into(),
            path: path.clone(),
            width: None,
            height: None,
            size: 9,
        };
        match c.recall().unwrap() {
            RecalledContent::Text(t) => assert_eq!(t, "disk text"),
            _ => panic!("expected text"),
        }
        let _ = std::fs::remove_file(&path);
    }
```

- [ ] **Step 2: Run the new tests (expect pass after impl already written)**

Run: `cd src-tauri && cargo test --lib history_store`
Expected: 8 tests pass (6 from Task 1 + 2 new).

- [ ] **Step 3: Add the re-call commands**

In `src-tauri/src/commands/clipboard.rs`, add after `set_local_clipboard_files` (after line 97):

```rust
/// Re-copy a History item's retained content to the local OS clipboard,
/// keyed by id. No cluster broadcast.
#[tauri::command]
pub(crate) async fn recall_copy_history_item(
    id: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    use crate::clipboard::history_store::RecalledContent;
    let recalled = {
        let store = state.history_store.lock().unwrap();
        let entry = store
            .get(&id)
            .ok_or_else(|| "Content no longer available".to_string())?;
        entry.content.recall()?
    };
    match recalled {
        RecalledContent::Text(t) => crate::clipboard::set_clipboard(&app_handle, t),
        RecalledContent::Rich { text, formats } => {
            crate::clipboard::set_clipboard_rich(&app_handle, text, formats)
        }
        RecalledContent::Image {
            mime,
            bytes,
            width,
            height,
        } => {
            let blob = crate::protocol::ClipboardBlob::from_bytes(mime, &bytes, width, height);
            crate::clipboard::set_clipboard_image(&app_handle, blob);
        }
    }
    Ok(())
}

/// Re-broadcast a History item's retained content to the cluster, keyed by id.
/// Reconstructs the original clipboard content and runs it through the normal
/// broadcast path, so large items re-descriptor correctly.
#[tauri::command]
pub(crate) async fn recall_send_history_item(
    id: String,
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    use crate::clipboard::common::ClipboardContent;
    use crate::clipboard::history_store::RecalledContent;
    let recalled = {
        let store = state.history_store.lock().unwrap();
        let entry = store
            .get(&id)
            .ok_or_else(|| "Content no longer available".to_string())?;
        entry.content.recall()?
    };
    let content = match recalled {
        RecalledContent::Text(t) => ClipboardContent::Text(t),
        RecalledContent::Rich { text, formats } => ClipboardContent::Rich { text, formats },
        RecalledContent::Image {
            mime,
            bytes,
            width,
            height,
        } => {
            let blob = crate::protocol::ClipboardBlob::from_bytes(mime, &bytes, width, height);
            ClipboardContent::Image(blob)
        }
    };
    crate::clipboard::common::process_clipboard_change(content, &app_handle, &state, &transport);
    Ok(())
}
```

(`ClipboardContent` at `common.rs:252` is `Text(String)` / `Image(ClipboardBlob)` / `Rich { text, formats }` — the constructions above match exactly.)

- [ ] **Step 4: Drop the store entry on delete**

In `delete_history_item` (`src-tauri/src/commands/clipboard.rs:100`), after line 108 (`let _ = app_handle.emit("history-delete", &id);`), add:

```rust
    // Drop retained content + its disk file.
    if let Some(evicted) = state.history_store.lock().unwrap().remove(&id) {
        if let Some(path) = evicted.disk_path {
            let _ = std::fs::remove_file(&path);
        }
        state.local_clipboard_blobs.lock().unwrap().remove(&id);
    }
```

- [ ] **Step 5: Register the commands**

In `src-tauri/src/app.rs`, inside `tauri::generate_handler![ … ]` (after line 1168, `crate::commands::clipboard::delete_history_item,`), add:

```rust
            crate::commands::clipboard::recall_copy_history_item,
            crate::commands::clipboard::recall_send_history_item,
```

- [ ] **Step 6: Build + test**

Run: `cd src-tauri && cargo build && cargo test --lib`
Expected: compiles; all lib tests pass.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/clipboard/history_store.rs src-tauri/src/commands/clipboard.rs src-tauri/src/app.rs
git commit -m "feat(history): id-keyed recall_copy / recall_send commands + delete cleanup"
```

---

## Task 6: Frontend types + listeners + protocol.ts

**Files:**
- Modify: `src/types.ts:41-78` (`ClipboardBlobPreview`, `HistoryItem`)
- Modify: `src/lib/protocol.ts:18-56` (`blobPreviewFromPreview`)
- Modify: `src/App.tsx:485-559` (`clipboard-monitor-update`, `clipboard-change`, `clipboard-pending` listeners) + new `history-backing-evicted` listener

- [ ] **Step 1: Extend the TS types**

In `src/types.ts`, replace `ClipboardBlobPreview` (lines 41-55) with:

```typescript
export type ClipboardBlobPreview = {
  mime_type: string;
  width?: number;
  height?: number;
  size: number;        // byte length, for "12 KB" display
  // base64 PNG thumbnail from the backend (small). Absent for a not-yet-fetched
  // descriptor (no bytes available until the user accepts the transfer).
  thumbnail?: string;
  // §3.3 descriptor — bytes haven't been fetched yet; no thumbnail.
  descriptor?: boolean;
};
```

In `HistoryItem` (lines 68-78), replace the `text: string;` line and add fields:

```typescript
export type HistoryItem = {
  id: string;
  origin: "local" | "remote";
  device: string; // The sender's hostname
  ts: number; // Unix timestamp in seconds
  text: string;        // truncated preview (≤4 KB), NOT the full content
  text_len: number;    // true byte length of the full text
  files?: { name: string; size: number; }[];
  blob?: ClipboardBlobPreview;
  formats?: ClipboardFormatPreview[];
  sender_id?: string;
  has_backing: boolean; // re-call possible (content still retained)
};
```

- [ ] **Step 2: Add the thumbnail-based blob preview builder**

In `src/lib/protocol.ts`, replace `blobFromPayload` (lines 18-56) with a function that reads the preview's `thumbnail` (a data URL from base64 PNG) instead of decoding full bytes. Keep the old name as an alias is unnecessary — rename and update callers in Step 3.

```typescript
// Build the history blob preview from a backend ClipboardPreview.blob. The
// backend ships a small base64 PNG thumbnail (or none for a not-yet-fetched
// descriptor); we wrap it as a data URL. No full-bytes decode happens in the
// WebView anymore — that was the History perf bottleneck.
export function blobPreviewFromPreview(blob: any): ClipboardBlobPreview | undefined {
  if (!blob) return undefined;
  return {
    mime_type: blob.mime_type || "image/png",
    width: typeof blob.width === "number" ? blob.width : undefined,
    height: typeof blob.height === "number" ? blob.height : undefined,
    size: typeof blob.size === "number" ? blob.size : 0,
    thumbnail:
      typeof blob.thumbnail === "string" && blob.thumbnail.length > 0
        ? `data:image/png;base64,${blob.thumbnail}`
        : undefined,
    descriptor: !!blob.descriptor,
  };
}
```

Leave `formatsFromPayload` and `shortRichLabel` as-is (the preview's `formats` array has the same `{mime_type, binary, size}` shape, so `formatsFromPayload` still works).

- [ ] **Step 3: Update the App.tsx listeners**

In `src/App.tsx`, update the imports from `../lib/protocol` to use `blobPreviewFromPreview` instead of `blobFromPayload`.

Replace the `clipboard-monitor-update` handler's `newItem` (lines 490-500) with:

```tsx
      const newItem: HistoryItem = {
        id: p.id,
        origin: "local",
        device: "Me",
        sender_id: p.sender_id,
        ts: p.timestamp,
        text: p.text_preview || "",
        text_len: typeof p.text_len === "number" ? p.text_len : 0,
        files: p.files,
        blob: blobPreviewFromPreview(p.blob),
        formats: formatsFromPayload(p.formats),
        has_backing: !!p.has_backing,
      };
```

The monitor handler's `if (newItem.text) setLocalClipboard(newItem.text);` (lines 503-507): the preview text is truncated, so it must NOT be treated as the real clipboard. Replace with a guard that only mirrors when the full text fits the preview (i.e. small items):

```tsx
      // Only mirror small items whose full text is the preview. Large items
      // are re-called from the backend on demand.
      if (newItem.text && newItem.text_len <= newItem.text.length) {
        setLocalClipboard(newItem.text);
      }
```

Replace the `clipboard-change` handler's `newItem` (lines 518-528) with:

```tsx
      const newItem: HistoryItem = {
        id: p.id,
        origin: isLocal ? "local" : "remote",
        device: p.sender,
        sender_id: p.sender_id,
        ts: p.timestamp,
        text: p.text_preview || "",
        text_len: typeof p.text_len === "number" ? p.text_len : 0,
        files: p.files,
        blob: blobPreviewFromPreview(p.blob),
        formats: formatsFromPayload(p.formats),
        has_backing: !!p.has_backing,
      };
```

In that same handler, the `setLocalClipboard`/`setLastSentClipboard`/`setLastReceivedClipboard` calls (lines 531-542) use `newItem.text`. Guard them the same way so the truncated preview never becomes the tracked clipboard string:

```tsx
      const fullTextAvailable = newItem.text_len <= newItem.text.length;
      if (isLocal) {
        if (newItem.text && fullTextAvailable) {
          setLocalClipboard(newItem.text);
          setLastSentClipboard(newItem.text);
        }
      } else {
        if (newItem.text && fullTextAvailable) {
          setLocalClipboard(newItem.text);
          setLastReceivedClipboard(newItem.text);
        }
      }
```

The dedupe/revoke block (lines 545-558): there are no more `object_url`s to revoke (thumbnails are data URLs, GC'd normally). Replace the block with:

```tsx
      setClipboardHistory((prev) => {
        if (prev.find(i => i.id === newItem.id)) return prev;
        return [newItem, ...prev].slice(0, 50);
      });
```

The `clipboard-pending` handler (lines 561-573) builds a pending preview. Update its `blob` to `blobPreviewFromPreview(p.blob)` and `text` to `p.text_preview || ""` (the pending modal shows a short preview; full text isn't needed pre-accept).

- [ ] **Step 4: Add the `history-backing-evicted` listener**

In `src/App.tsx`, near the other `listen(...)` registrations inside the same `useEffect` (alongside `unlistenDelete` at line 576), add:

```tsx
    const unlistenEvicted = listen<string>("history-backing-evicted", (event) => {
      const id = event.payload;
      setClipboardHistory((prev) =>
        prev.map((it) => (it.id === id ? { ...it, has_backing: false } : it))
      );
    });
```

And add `unlistenEvicted.then(u => u());` to the cleanup return of that `useEffect` (wherever the other `unlisten*` are torn down).

- [ ] **Step 5: Typecheck**

Run: `npm run build` (or `npx tsc --noEmit`)
Expected: no type errors. Fix any remaining `blobFromPayload` references (search: `grep -rn blobFromPayload src/`).

- [ ] **Step 6: Commit**

```bash
git add src/types.ts src/lib/protocol.ts src/App.tsx
git commit -m "feat(history): frontend consumes light ClipboardPreview + eviction event"
```

---

## Task 7: HistoryView — re-call by id, thumbnails, disabled state

**Files:**
- Modify: `src/components/HistoryView.tsx:51-67` (handlers), `:156-176` (render), `:209-232` (buttons)

- [ ] **Step 1: Switch the handlers to id-keyed re-call**

In `src/components/HistoryView.tsx`, replace `handleSend` and `handleLocalCopy` (lines 51-67) with:

```tsx
  const handleSend = async (id: string) => {
    try {
      await invoke("recall_send_history_item", { id });
    } catch (e) {
      console.error("Failed to send:", e);
      alert("Failed to send: " + e);
    }
  };

  const handleLocalCopy = async (id: string) => {
    try {
      await invoke("recall_copy_history_item", { id });
    } catch (e) {
      console.error("Failed to copy:", e);
      alert("Failed to copy: " + e);
    }
  };
```

- [ ] **Step 2: Render the thumbnail + size**

In the blob block (lines 158-176), replace `it.blob.object_url` with `it.blob.thumbnail`:

```tsx
                    {it.blob && (
                      <div className="mt-2 flex flex-col gap-1 rounded-lg bg-zinc-50 p-2 dark:bg-zinc-800">
                        {it.blob.thumbnail ? (
                          <img
                            src={it.blob.thumbnail}
                            alt="Clipboard image"
                            className="max-h-48 max-w-full rounded-md object-contain"
                          />
                        ) : (
                          <div className="flex h-24 w-full items-center justify-center rounded-md bg-zinc-200 text-3xl text-zinc-500 dark:bg-zinc-700">
                            🖼️
                          </div>
                        )}
                        <div className="text-[11px] text-zinc-500">
                          {it.blob.descriptor && !it.blob.thumbnail ? "Large image (not yet fetched)" : "Image"}
                          {it.blob.width && it.blob.height ? ` • ${it.blob.width}×${it.blob.height}` : ""}
                          {` • ${formatBytes(it.blob.size)}`}
                        </div>
                      </div>
                    )}
```

- [ ] **Step 3: Show large-text size and guard the text display**

Right after the text line (line 156), the `it.text` clamp still works (it's the truncated preview). Add a size hint for large text. Replace line 156 with:

```tsx
                    {it.text && <div className="mt-2 line-clamp-3 whitespace-pre-wrap text-sm text-zinc-900 dark:text-zinc-50">{it.text}</div>}
                    {it.text && it.text_len > it.text.length && (
                      <div className="mt-1 text-[11px] text-zinc-500">Large text • {formatBytes(it.text_len)}</div>
                    )}
```

- [ ] **Step 4: Re-call buttons by id, disabled when no backing**

Replace the Copy button (lines 210-214) and Send button (lines 230-232). The Copy button should appear for any text-or-blob item (not just `it.text`), gated on `has_backing`:

```tsx
                    {(it.text || it.blob) && (
                      <IconButton
                        label={it.has_backing ? "Copy to Clipboard" : "Content no longer available"}
                        onClick={() => it.has_backing && handleLocalCopy(it.id)}
                        disabled={!it.has_backing}
                      >
                        <Copy className={`h-4 w-4 ${it.has_backing ? "text-zinc-600 dark:text-zinc-300" : "text-zinc-300 dark:text-zinc-600"}`} />
                      </IconButton>
                    )}
```

```tsx
                    {(it.text || it.blob) && (
                      <IconButton
                        label={it.has_backing ? "Send to Cluster" : "Content no longer available"}
                        onClick={() => it.has_backing && handleSend(it.id)}
                        disabled={!it.has_backing}
                      >
                        <Send className={`h-4 w-4 ${it.has_backing ? "text-emerald-600 dark:text-emerald-400" : "text-emerald-600/30 dark:text-emerald-400/30"}`} />
                      </IconButton>
                    )}
```

Confirm `IconButton` accepts a `disabled` prop. Run:

```bash
grep -n "IconButton" src/components/ui.tsx | head
```

If `IconButton` does not forward `disabled`, add `disabled?: boolean` to its props and pass it to the underlying `<button>` (set `disabled={disabled}` and add `disabled:cursor-not-allowed disabled:opacity-50` to its className).

- [ ] **Step 5: Typecheck + build the frontend**

Run: `npm run build`
Expected: no type errors.

- [ ] **Step 6: Commit**

```bash
git add src/components/HistoryView.tsx src/components/ui.tsx
git commit -m "feat(history): re-call by id, render thumbnails, disable when backing evicted"
```

---

## Task 8: Cleanup verification + full build + manual device test

**Files:** none new — verification task.

- [ ] **Step 1: Confirm cleanup is covered**

The disk tier reuses `temp_downloads/`, already wiped by `clear_cache` on startup (`app.rs:440`) and on exit (`app.rs:1245`). The RAM tier and the `HistoryStore` map are reconstructed empty on each launch (`AppState::new`). No new cleanup code is required. Verify by reading:

```bash
cd src-tauri && sed -n '340,358p' src/app.rs && sed -n '1241,1246p' src/app.rs
```

Confirm `temp_downloads` is the dir both clear and that `AppState::new` starts the store empty. Document this in the commit if any gap is found.

- [ ] **Step 2: Full workspace build**

Run: `cd src-tauri && cargo build && cargo test --lib`
Then: `npm run build`
Expected: clean build, all lib tests pass.

- [ ] **Step 3: Lint/clippy (match repo norm)**

Run: `cd src-tauri && cargo clippy --all-targets 2>&1 | tail -30`
Expected: no new warnings introduced by these changes (pre-existing warnings OK).

- [ ] **Step 4: Manual device test (two hosts / cluster)**

Build a dev binary and verify on the running app:
1. Copy a **31 MB text file's contents** on the sender. History entry appears **instantly** (no 10–20 s stall) on both sender and receiver, showing a snippet + "Large text • 31.0 MB".
2. Copy a **large image (>10 MB)**. History shows a **thumbnail** on both sides within ~1 s.
3. On the receiver, click **Copy to Clipboard** on the large text item → paste elsewhere → full text is present.
4. Click **Send to Cluster** on a large image item → it re-broadcasts and lands on peers.
5. Lower **Settings → General → History storage limit (MB)** below the current total → oldest items' Copy/Send buttons disable (eviction event), and `temp_downloads` shrinks.
6. Quit the app → `temp_downloads` is emptied; relaunch → History is empty (session-scoped, expected).

- [ ] **Step 5: CHANGELOG entry (terse, house style)**

Add under `## [Unreleased]` → `### Changed` in `CHANGELOG.md`:

```markdown
- History view no longer stalls on large clipboard items: the UI receives a light preview (truncated text or thumbnail) while full content stays in a budgeted backend store (200 MB default, configurable under Settings → General). Copy/Send re-call large items by id.
```

- [ ] **Step 6: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: changelog entry for History performance (light previews + re-call store)"
```

---

## Notes for the implementer

- **Do not bump version numbers** (app or GNOME extension) — out of scope unless Keith confirms.
- **Peer wire protocol is untouched** — `Message::Clipboard` still inlines ≤10 MB and descriptors >10 MB. Only the frontend `emit(...)` changed.
- **Borrow ordering in handlers.rs Task 4b:** stage from `&accum` / `text.as_bytes()` *before* moving the value into `set_clipboard_image` / `set_clipboard`.
- **`State<'_, AppState>` vs `&AppState`:** `record_and_emit` takes `&AppState`. From a command with `state: State<'_, AppState>`, pass `&state` (Tauri's `State` derefs); use `&*state` if the coercion fails.
