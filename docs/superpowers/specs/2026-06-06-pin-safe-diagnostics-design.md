# PIN-Safe Diagnostics — Design

**Source:** Email from @mdunphy ("Issue 5"), sub-project 2 of 2. Branch `dunphy-mail`.
**Date:** 2026-06-06
**Depends on:** sub-project 1 (Settings sidebar refactor) — the panel lives in the
new Diagnostics category (`src/components/settings/DiagnosticsSettings.tsx`).

## Problem

Pairing PINs can reach the on-disk log file (a leak vector), yet the verbose
pairing/mTLS detail is genuinely useful for debugging. Goal: the **file log
never contains an unredacted PIN**, while an **in-memory-only diagnostics panel**
in the UI surfaces the full unredacted pairing + mTLS detail (never written to
disk), at a verbosity the user selects.

## Decisions

1. **File-log redaction:** no PIN value is ever written to the `tracing` file log.
2. **Separate in-memory channel:** a bounded ring buffer in `AppState` + a
   `diagnostic-event` emit to the UI, distinct from `tracing`. Sensitive detail
   flows only here.
3. **Always-on capture** at full (Debug) detail; the panel's level dropdown is a
   **display filter** (default **Minimal**).
4. **Levels:** Minimal / Detailed / Debug.
5. **Keep the "Verbose pairing logs" toggle**, now governing only the *file*
   log's failure-reason detail (§H7) — never PINs.
6. **Panel actions:** Clear, Copy all (level-filtered), Auto-scroll (follow tail,
   pause on manual scroll-up), Pause (freezes the *display*; backend capture
   stays always-on and the view re-syncs on resume).
7. **Buffer cap:** 1000 events, FIFO eviction, memory-only (cleared on restart).

## Design

### A. File-log redaction (the security fix, folded in)

- `storage.rs` `load_network_pin` generated-PIN log (current ~line 395): remove
  the PIN value — log e.g. `"Generated a new network PIN"` with no value.
- `lib.rs` `perform_factory_reset` log (current ~line 793): change
  `(PIN: {})` to `(PIN: <redacted>)` (drop `new_pin_val` from the format args).
- `pairing/mod.rs` PIN-byte dump (current ~line 620, gated by
  `pairing_debug_logs`): **remove the `tracing::warn!` entirely**; emit the same
  detail as a **Debug-level diagnostic event** instead (in-memory only).
- The `pairing_debug_logs` toggle and the generic-vs-detailed failure line in
  `log_pairing_failure` (`pairing/mod.rs` ~441–450) stay as-is — they control
  the *file* log's failure-reason verbosity (§H7), and never include a PIN.

### B. In-memory diagnostics channel

New module `src-tauri/src/diagnostics.rs`:

```rust
pub enum DiagLevel { Minimal, Detailed, Debug }   // serde rename_all = "snake_case"

pub struct DiagnosticEvent {
    pub ts_ms: u64,            // SystemTime epoch millis
    pub level: DiagLevel,
    pub kind: String,          // e.g. "pairing", "connect", "drop"
    pub peer: Option<String>,  // peer addr/id when known
    pub message: String,       // may contain sensitive detail for Debug events
}
```

- `AppState` gains `diagnostics: Arc<Mutex<VecDeque<DiagnosticEvent>>>` (cap 1000).
- Helper `push_diagnostic(state, app_handle, level, kind, peer, message)`:
  builds the event (timestamp from `SystemTime::now()`), pushes to the buffer
  (pop_front when over cap), and `app_handle.emit("diagnostic-event", &event)`.
  Always-on; no gating. A pure helper `diag_cap()`/eviction logic is unit-tested.

- **Capture points** (each tagged with a level):
  - *Minimal*: pairing started; pairing succeeded; pairing failed (generic);
    mTLS connection established; mTLS connection dropped.
  - *Detailed*: per-step pairing progress (T0–T5); pairing failure reason; peer
    addresses; mTLS handshake-failed reason.
  - *Debug*: PIN on failure (the detail removed from the file in §A); cert
    fingerprints; AEAD specifics.
  Sites: `pairing/mod.rs` (initiator `start_pairing` + responder
  `handle_pairing_connection` milestones/failures/lockout) and `transport.rs`
  (QUIC accept success ~238, handshake failure ~313, connection drop).

### C. Commands + event

- `get_diagnostic_events() -> Vec<DiagnosticEvent>` — snapshot for panel mount.
- `clear_diagnostic_events()` — empties the buffer.
- Event `diagnostic-event` (payload `DiagnosticEvent`) — live stream.
- Register both commands in the `invoke_handler!` in `app.rs`.

### D. Frontend panel (in `DiagnosticsSettings.tsx`)

Rendered below the existing Verbose toggle:
- TS types `DiagLevel` + `DiagnosticEvent` in `src/types.ts`.
- On mount: `invoke("get_diagnostic_events")` to seed local state, then
  `listen("diagnostic-event")` appending live (unless Paused).
- **Level dropdown** (Minimal default), Tailwind `dark:` classes on `<select>`
  and `<option>` so it's readable in dark mode. Filtering rule: an event shows
  when its level ≤ the selected level (Minimal⊆Detailed⊆Debug).
- Scrollable monospace list, newest last; row = time · level/kind badge · peer ·
  message.
- Actions:
  - **Clear** → `invoke("clear_diagnostic_events")` + clear local list.
  - **Copy all** → copy the currently-filtered rows to the clipboard.
  - **Auto-scroll** (default on) → follow the tail; disengages when the user
    scrolls up, re-engages at bottom.
  - **Pause** → stop appending live events to the display; on resume,
    re-fetch via `get_diagnostic_events` so nothing is missed.

### Out of scope

- Persisting the diagnostics buffer (memory-only by design).
- Double-logging non-sensitive events to the file (panel is the rich surface).
- Clipboard/file-transfer activity events (pairing + mTLS only).
- Changing the `tracing` setup beyond the three redactions.

## Testing

- **Rust unit tests** (`diagnostics.rs`): buffer eviction caps at 1000 (push
  1001, len==1000, oldest dropped); `DiagLevel` serde round-trips to
  `"minimal"/"detailed"/"debug"`.
- **Redaction**: grep confirms no `tracing` call formats a PIN value after the
  change (the three sites are redacted/removed).
- **Frontend**: `npm run build` passes; level-filter predicate (event.level ≤
  selected) is a small pure function — unit-test if a test setup exists,
  otherwise manual.
- **Manual:** trigger a pairing (success + a wrong-PIN failure) and an mTLS
  connect/drop across two devices; confirm: Minimal shows milestones only,
  Detailed adds reasons/steps, Debug shows the PIN/fingerprints; the file log
  (`{temp}/ClusterCutLogs/clustercut.log`) contains **no** PIN at any setting;
  Clear empties the panel; Copy all yields the filtered rows; Pause freezes then
  resumes correctly; buffer survives navigating away/back but not an app restart.

## File anchors (for the plan)

- `src-tauri/src/diagnostics.rs` (new) — types, buffer helper, eviction, tests.
- `src-tauri/src/lib.rs` — `mod diagnostics;`; factory-reset PIN redaction.
- `src-tauri/src/state.rs` — `diagnostics` ring buffer field + default.
- `src-tauri/src/storage.rs` — redact generated-PIN log.
- `src-tauri/src/pairing/mod.rs` — remove PIN-byte file dump; add capture points.
- `src-tauri/src/transport.rs` — mTLS connect/handshake-fail/drop capture points.
- `src-tauri/src/commands/` — `get_diagnostic_events`, `clear_diagnostic_events`.
- `src-tauri/src/app.rs` — register the two commands.
- `src/types.ts` — `DiagLevel`, `DiagnosticEvent`.
- `src/components/settings/DiagnosticsSettings.tsx` — the panel.
