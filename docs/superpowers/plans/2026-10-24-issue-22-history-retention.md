# Issue #22: History Retention Settings, and Clear/Delete That Really Clear

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** let users decide how long clipboard history lives and how much of it there is, including none at all (issue #22, mtudor), and make Clear and Delete remove the stored content rather than just the list entries.

**Status:** done 2026-10-24, Keith tested; merged to main.

## Decisions (Keith, 2026-10-24)

- In: the retention settings (off, expiry, item limit) and the Clear/Delete fixes.
- Out: skipping copies that password managers mark as secret.

## How history works today (checked 2026-10-24)

- **Not persistent:** the History list lives in the window's memory (`clipboardHistory` in `App.tsx`), capped at a hard-coded 50 (`slice(0, 50)`). Re-callable content lives in the backend `HistoryStore`: RAM for small items, files in `temp_downloads/` for large ones, under the 200 MB `history_store_max_bytes` budget. Quitting clears all of it. The issue's "persistent" means "for as long as ClusterCut runs", which for a tray app can be weeks.
- **Clear History** only empties the frontend list; the store keeps every entry's content until the budget evicts it or ClusterCut quits.
- **Delete** purges locally, and peers drop the entry from their list (`HistoryDelete` emits `history-delete`) but keep its content in their store.
- `record_and_emit` stores content for every event, including manual-mode local copies (`clipboard-monitor-update`) and pending received items (`clipboard-pending`), which never appear in History and are never recalled (recall is by History id only). For large items the stored entry is also the staged file peers fetch the content from.

## Design

### Settings

- `history_retention`: `off` | `15m` | `1h` | `1d` | `1w` | `until_quit`. Default `until_quit` (today's behaviour).
- `history_max_items`: default 50; Settings offers 10, 25, 50, 100, 250, 500.
- `history_store_max_bytes` stays as it is.
- UI: General settings, beside "History storage limit": "Keep History" (select) and "Keep At Most" (select, hidden when off). With history off, the History view says so and links to Settings.

### Backend (`history_store.rs`, `common.rs`)

- `StoredEntry` gains `listed` (shown in History, i.e. recorded for `clipboard-change`) and `stored_at` (`Instant`).
- `record_and_emit` stores content only when it can be recalled or is needed for serving:
  - listed and history isn't off: stored, `listed = true`;
  - otherwise only `Disk` entries, `listed = false`: the staged files peers fetch large items from, with their lifetime still bound by the byte budget, as today.
  - So no RAM copy of manual-mode copies, pending items, or anything while history is off.
- `HistoryStore::apply_limits(max_items, max_age)` evicts listed entries beyond the count (oldest first) and listed entries older than the age. `clear_listed()` removes every listed entry.
- Evictions from retention, Clear and Delete go through one cleanup (delete the file unless it's mid-fetch or being served, drop `local_clipboard_blobs`, as `handle_evictions` does) and emit `history-delete`, so the entry leaves the list. Byte-budget evictions keep emitting `history-backing-evicted` (entry stays, can't be recalled).
- A sweep every 30 s applies the limits; saving settings applies them at once (switching to off clears).

### Frontend (`App.tsx`)

- `HistoryItem.added_at` (local `Date.now()`; `ts` is the sender's clock).
- A ref with the retention settings (like `isAutoReceiveRef`): don't add while off, slice to `history_max_items`, and a 30 s interval drops items older than the retention. Settings changes trim at once. Items with no stored content (files, unfetched large items) only exist here, hence the frontend timer too.
- Clear History calls the new `clear_history` command, then empties the list.

### Fixes

- `clear_history` command: `clear_listed()` plus the cleanup.
- Peer `HistoryDelete`: purge the local store entry and its file too. Shared `delete_history_entry(app, state, id)` for `delete_history_item` and the handler.

## Tasks

- [x] Settings fields: `HistoryRetention` (serde names above, default `until_quit`), `history_max_items` (default 50, clamped to 1..=1000 in `history_limits()`, which gives 0 items while off); tests for missing fields and the limits.
- [x] `HistoryStore`: `listed`, `stored_at`, `insert(.., listed)`, `apply_limits`, `clear_listed`; tests (count, age, unlisted left to the budget, clear).
- [x] `record_and_emit`: stores listed content (not while off) and unlisted `Disk` entries only, applies the limits after each insert; `release_evicted` shared by `handle_evictions` (`history-backing-evicted`) and `drop_history_entries` (`history-delete`); `delete_history_entry` (delete button and peer `HistoryDelete`); `clear_history` command; `apply_history_retention` on settings save; 30 s `spawn_history_sweep` from startup. Checked: no caller holds the settings lock across `record_and_emit`.
- [x] Frontend: `added_at`, `historyLimitsRef` + `trimHistory` on add, settings load/change and a 30 s interval; Clear calls `clear_history`; HistoryView says when history is off.
- [x] Settings UI: Keep History and Keep At Most selects beside the storage limit.
- [x] 175 Rust tests pass; frontend builds. Not run end to end here: listed entries only come from sends and receives, which would have reached Keith's paired machines.
- [x] Keith: tested and works (2026-10-24).
- [x] CHANGELOG (Added, Changed, Fixed), credit @mtudor.
- [ ] Reply on #22.

## Known limits

- Outgoing large items staged for peers to fetch aren't History; they stay until the byte budget pushes them out or ClusterCut quits.
- When a large item expires, its staged file goes, so a peer that hasn't fetched it yet can't.
