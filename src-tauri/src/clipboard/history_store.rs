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
    ///
    /// The accounting is intentionally approximate: for `Rich`, it counts
    /// `ClipboardFormat.data` directly — which is already base64-encoded for
    /// binary formats, so the byte count is the wire size, not the raw size.
    /// Small fixed overhead (e.g. `mime_type` strings) is ignored; with the
    /// 200 MB default cap a few hundred extra bytes per entry are immaterial.
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
    /// Byte count cached at insert time (computed from `content.size()`).
    /// Because the API is append-only (insert replaces, never mutates in
    /// place), this never diverges from the live content.
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
}
