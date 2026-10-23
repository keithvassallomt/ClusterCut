# Full-Content Manual Send Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In manual mode (Auto-Send off), sending should carry everything Auto-Send would: plain text, rich text (HTML/RTF), images (large ones via the descriptor/file channel) and copied files. Today every manual trigger sends plain text only: the shortcut and D-Bus `ManualSend` re-read the clipboard as text, and the in-app "Broadcast to Cluster" button sends the UI's text (`send_clipboard`). History's "Send" on an item doesn't send at all in manual mode: it goes through the Auto-Send gate.

**Key observation:** the clipboard monitor already runs in manual mode. On every copy, `process_clipboard_change` builds the full `ClipboardPayload` (staging large text/images for peers to fetch) and only `broadcast_clipboard` stops at `if !auto_send`, recording it as a `clipboard-monitor-update`. The payload is ready; it just isn't kept.

**Why not re-read the clipboard on demand:** the plugin backend (X11, Windows, macOS) reads raster images through an `arboard` instance owned by its worker thread (mandatory on Windows), so an on-demand full read needs new worker plumbing on platforms that can't be tested here. Keeping the monitor's payload needs no per-backend code.

## Design

- `Broadcast::{IfAutoSend, Always}` on `broadcast_clipboard` and a new `process_clipboard_change_with` (`process_clipboard_change` stays as the `IfAutoSend` wrapper for the monitors).
- `PENDING_MANUAL_SEND` (module static in `clipboard/common.rs`): set when `IfAutoSend` stops at the gate; cleared when an `IfAutoSend` broadcast goes out (Auto-Send on) and by `set_ignored` (every ClusterCut self-write, e.g. applying received content), so a manual send never resends an older local copy.
- `shortcuts::manual_send`: send the pending payload with `Always`; if none (copied before start, Auto-Send was on, clipboard holds a self-write), fall back to the plain-text read, now through `process_clipboard_change_with(Text, Always)` so large text gets the descriptor path and History is recorded.
- New command `manual_send` replaces `send_clipboard` (the button path, which also oddly wrote the text back to the local clipboard). `recall_send_history_item` uses `Always`.
- UI: track the last captured local item (`clipboard-monitor-update`); the Send panel shows it (image thumbnail, file list, or text) and the FAB/button work for images and files, not just text.

## Tasks (DONE 2026-10-23)

- [x] Backend (`clipboard/common.rs`): `Broadcast`, `PENDING_MANUAL_SEND` + `pending_manual_send()`, `process_clipboard_change_with`, gate keeps the payload, `set_ignored` clears it. Unit test: a self-write drops the kept copy.
- [x] `shortcuts::manual_send` sends the kept payload with `Always` (logs its kind: text / rich text / image / large text / files); fallback is the plain-text read through `process_clipboard_change_with(Text, Always)`. New `manual_send` command (via `spawn_blocking`) replaces `send_clipboard`; `recall_send_history_item` uses `Always`.
- [x] Frontend: `pendingLocalItem` (from `clipboard-monitor-update`) + `lastSentId`; FAB pending rule uses it; cleared on a different local send and on remote content when Auto-Receive is on (`isAutoReceiveRef`), mirroring the backend. Send panel renders image thumbnail, file list or text (with Rich badge); button invokes `manual_send`.
- [x] Tests: 154 Rust tests pass; `npm run build` (tsc + vite) passes. Local Hyprland, manual mode, `wl-copy` then `ManualSend`: logs show "Manual send of the last copy" for image, files and rich text, each sent to every peer. Receiving side not exercised: no paired peers available (all "no pinned fingerprint").
- [x] CHANGELOG: Changed (full-content manual send), Fixed (History "Send" with Auto-Send off).
- [x] Keith: manual send of a copied web selection with an image works (2026-10-23).
- [x] "Nothing to send" (Keith, 2026-10-23): when nothing is kept and the clipboard has no (non-whitespace) text, `manual_send` shows a "Manual Send: Nothing to send. Copy something first." notification (unconditional, like "Manual Receive: No pending content."). An empty clipboard (`ClipboardContent::None` in `should_process_content`) now also drops the kept copy, so a manual send matches what could be pasted. Unit test added; 155 pass.
- [ ] Keith: check the notification (empty clipboard, then Ctrl+Alt+C); look at the Send panel with an image and with files copied.

**Known edge:** after "Copy" on a History item (a ClusterCut self-write), nothing is kept, so a manual send falls back to the clipboard's plain text; for a recalled image that means nothing is sent. History's own "Send" covers that case.
