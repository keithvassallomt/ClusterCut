# PIN-Safe Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop writing PINs to the file log, and add an always-on in-memory diagnostics channel surfaced as a level-filtered event-log panel in the Diagnostics settings category.

**Architecture:** A new `diagnostics` module owns a `DiagnosticEvent` type + a bounded ring buffer in `AppState`; `push_diagnostic(...)` appends + emits `diagnostic-event`. Pairing and transport code call it at tagged levels (Minimal/Detailed/Debug). Three `tracing` sites are redacted so the file log never holds a PIN. Two commands expose the buffer; the panel lives in `DiagnosticsSettings.tsx`.

**Tech Stack:** Rust (Tauri 2, serde), React/TypeScript.

**Reference spec:** `docs/superpowers/specs/2026-06-06-pin-safe-diagnostics-design.md`

**Test command (Rust):** `cargo test --manifest-path src-tauri/Cargo.toml`
**Build (Rust):** `cargo build --manifest-path src-tauri/Cargo.toml`
**Build (frontend):** `npm run build`

---

## Task 1: `diagnostics` module — types, ring buffer, helper, tests

**Files:**
- Create: `src-tauri/src/diagnostics.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod diagnostics;`)
- Modify: `src-tauri/src/state.rs` (buffer field + default)

- [ ] **Step 1: Create `src-tauri/src/diagnostics.rs` with tests**

```rust
//! In-memory, never-persisted diagnostics channel. Sensitive pairing/mTLS
//! detail (PINs, fingerprints) flows ONLY here — never to the `tracing` file
//! log. A bounded ring buffer holds recent events; each is also emitted to the
//! UI as `diagnostic-event`. See the PIN-safe-diagnostics spec.

use serde::Serialize;
use std::collections::VecDeque;
use tauri::Emitter;

/// Max events retained in memory (FIFO eviction). Memory-only; cleared on restart.
pub const DIAG_CAP: usize = 1000;

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DiagLevel {
    Minimal,
    Detailed,
    Debug,
}

#[derive(Serialize, Clone, Debug)]
pub struct DiagnosticEvent {
    pub ts_ms: u64,
    pub level: DiagLevel,
    pub kind: String,
    pub peer: Option<String>,
    pub message: String,
}

/// Push an event into the buffer, evicting the oldest when at capacity. Pure
/// (no I/O) so it is unit-testable.
pub(crate) fn push_capped(buf: &mut VecDeque<DiagnosticEvent>, ev: DiagnosticEvent, cap: usize) {
    while buf.len() >= cap {
        buf.pop_front();
    }
    buf.push_back(ev);
}

/// Record a diagnostic event: append to the in-memory buffer and emit it to the
/// UI. Always-on (no gating); the panel's level dropdown filters the display.
pub fn push_diagnostic(
    state: &crate::state::AppState,
    app_handle: &tauri::AppHandle,
    level: DiagLevel,
    kind: &str,
    peer: Option<String>,
    message: String,
) {
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let ev = DiagnosticEvent { ts_ms, level, kind: kind.to_string(), peer, message };
    {
        let mut buf = state.diagnostics.lock().unwrap();
        push_capped(&mut buf, ev.clone(), DIAG_CAP);
    }
    let _ = app_handle.emit("diagnostic-event", &ev);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(msg: &str) -> DiagnosticEvent {
        DiagnosticEvent { ts_ms: 0, level: DiagLevel::Minimal, kind: "test".into(), peer: None, message: msg.into() }
    }

    #[test]
    fn evicts_oldest_at_capacity() {
        let mut buf = VecDeque::new();
        for i in 0..5 {
            push_capped(&mut buf, ev(&i.to_string()), 3);
        }
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.front().unwrap().message, "2"); // 0,1 evicted
        assert_eq!(buf.back().unwrap().message, "4");
    }

    #[test]
    fn level_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&DiagLevel::Minimal).unwrap(), "\"minimal\"");
        assert_eq!(serde_json::to_string(&DiagLevel::Detailed).unwrap(), "\"detailed\"");
        assert_eq!(serde_json::to_string(&DiagLevel::Debug).unwrap(), "\"debug\"");
    }
}
```

- [ ] **Step 2: Declare the module**

In `src-tauri/src/lib.rs`, add `mod diagnostics;` after `mod compression;` (line 5):

```rust
mod compression;
mod diagnostics;
```

- [ ] **Step 3: Add the buffer to `AppState`**

In `src-tauri/src/state.rs`, add the import near the top if needed (`use std::collections::VecDeque;` — check; `HashMap` is already imported from `std::collections`, so extend that or add a line). Add the field to the `AppState` struct (near `settings`):

```rust
    /// In-memory diagnostics ring buffer (pairing/mTLS events). Never persisted;
    /// surfaced in the Diagnostics panel. See diagnostics.rs.
    pub diagnostics: Arc<Mutex<VecDeque<crate::diagnostics::DiagnosticEvent>>>,
```

And initialize it in the constructor/default alongside the other fields:

```rust
            diagnostics: Arc::new(Mutex::new(VecDeque::new())),
```

- [ ] **Step 4: Run tests + build**

Run: `cargo test --manifest-path src-tauri/Cargo.toml diagnostics`
Expected: PASS (2 tests).
Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds (a `dead_code` warning for `push_diagnostic` is acceptable until later tasks call it; `push_capped`/types are exercised by tests + the helper).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/diagnostics.rs src-tauri/src/lib.rs src-tauri/src/state.rs
git commit -m "feat: in-memory diagnostics ring buffer + event type + tests"
```

---

## Task 2: Commands to read/clear the buffer

**Files:**
- Modify: `src-tauri/src/commands/` (add a `diagnostics` command set — see note) 
- Modify: `src-tauri/src/app.rs` (register commands)

- [ ] **Step 1: Add the commands**

Find where command modules live (`src-tauri/src/commands/mod.rs` lists submodules like `peers`, `settings`, `identity`). Create `src-tauri/src/commands/diagnostics.rs`:

```rust
//! Commands backing the in-memory diagnostics panel.

use crate::diagnostics::DiagnosticEvent;
use crate::state::AppState;
use tauri::State;

#[tauri::command]
pub(crate) fn get_diagnostic_events(state: State<'_, AppState>) -> Vec<DiagnosticEvent> {
    state.diagnostics.lock().unwrap().iter().cloned().collect()
}

#[tauri::command]
pub(crate) fn clear_diagnostic_events(state: State<'_, AppState>) {
    state.diagnostics.lock().unwrap().clear();
}
```

Add `pub(crate) mod diagnostics;` to `src-tauri/src/commands/mod.rs` alongside the existing submodule declarations.

- [ ] **Step 2: Register the commands**

In `src-tauri/src/app.rs`, in the `tauri::generate_handler![ ... ]` list, add (next to other command registrations):

```rust
            crate::commands::diagnostics::get_diagnostic_events,
            crate::commands::diagnostics::clear_diagnostic_events,
```

- [ ] **Step 3: Build**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds. (`push_diagnostic` may still warn as unused until Tasks 4–5.)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/diagnostics.rs src-tauri/src/commands/mod.rs src-tauri/src/app.rs
git commit -m "feat: get/clear diagnostic events commands"
```

---

## Task 3: Redact PINs from the file log (non-pairing sites)

**Files:**
- Modify: `src-tauri/src/storage.rs` (generated-PIN log)
- Modify: `src-tauri/src/lib.rs` (factory-reset log)

- [ ] **Step 1: storage.rs generated-PIN log**

In `src-tauri/src/storage.rs`, change (current ~line 395):

```rust
    tracing::info!("Generated New Network PIN: {}", pin);
```

to:

```rust
    tracing::info!("Generated a new network PIN.");
```

- [ ] **Step 2: lib.rs factory-reset log**

In `src-tauri/src/lib.rs`, the factory-reset log (current ~line 792-797) formats the PIN. Change the format string + args so the PIN is not emitted. Current:

```rust
        tracing::info!(
            "Reset to New Network: {} (PIN: {}, cluster {})",
            new_name_val,
            new_pin_val,
            new_cluster_id
        );
```

to:

```rust
        tracing::info!(
            "Reset to New Network: {} (PIN: <redacted>, cluster {})",
            new_name_val,
            new_cluster_id
        );
```

(`new_pin_val` is still used to set `*np`, so it stays defined — only the log arg is removed.)

- [ ] **Step 3: Build + confirm no PIN in these logs**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds, no unused-variable warning for `new_pin_val` (still assigned to state).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/storage.rs src-tauri/src/lib.rs
git commit -m "security: stop writing PIN values to the file log (storage + factory reset)"
```

---

## Task 4: Pairing capture points (+ move the PIN dump in-memory)

**Files:**
- Modify: `src-tauri/src/pairing/mod.rs`

All these sites already have `state: &AppState` (or `state`) and `app_handle` in scope (the existing `emit("pairing-*")` calls and `log_pairing_failure(&state, ...)` prove it). Use `crate::diagnostics::push_diagnostic(state, app_handle, level, "pairing", Some(peer.to_string()), msg)`.

- [ ] **Step 1: Replace the PIN-byte file dump with a Debug diagnostic event**

In `handle_pairing_connection`, the block (current ~line 619-625):

```rust
            if state.settings.lock().map(|s| s.pairing_debug_logs).unwrap_or(false) {
                tracing::warn!(
                    "Responder PIN at T2-AEAD-failure: len={} bytes={:02x?}",
                    pin.len(),
                    pin.as_bytes()
                );
            }
```

Replace with (always capture to memory at Debug; no file write, no gating):

```rust
            crate::diagnostics::push_diagnostic(
                &state,
                &app_handle,
                crate::diagnostics::DiagLevel::Debug,
                "pairing",
                Some(peer_addr.to_string()),
                format!(
                    "Responder PIN at T2-AEAD-failure: len={} bytes={:02x?}",
                    pin.len(),
                    pin.as_bytes()
                ),
            );
```

Confirm `app_handle` is in scope in `handle_pairing_connection` (it is — used for `emit` later). Confirm the variable holding the responder's PIN here is `pin`.

- [ ] **Step 2: Add Minimal/Detailed milestones in `start_pairing` (initiator)**

In `start_pairing` (`src-tauri/src/pairing/mod.rs:59`), add diagnostic calls at these points (use the peer address/target available in that function — match the variable name actually in scope, e.g. `peer_addr`):
- Right after pairing begins (after the target address is resolved): `push_diagnostic(&state, &app_handle, DiagLevel::Minimal, "pairing", Some(addr), "Pairing started".into())`.
- At the success emit (line 431 `emit("pairing-success", ...)`): add `push_diagnostic(... DiagLevel::Minimal, "pairing", Some(responder_device_id-or-addr), "Pairing succeeded".into())`.
- At EACH `emit("pairing-failed", <msg>)` site (lines 158, 168, 202, 223, 228, 236, 267, 341, 353): add a Detailed event carrying the specific failure message string, e.g. `push_diagnostic(&state, &app_handle, DiagLevel::Detailed, "pairing", Some(addr), format!("Pairing failed: {}", <the same human message>))`. Also add ONE Minimal "Pairing failed" event per failure (so Minimal shows the outcome without the reason) — to avoid duplication, emit the Minimal generic line + the Detailed reason at each failure site.

Keep it mechanical: at each failure `emit`, add two `push_diagnostic` calls (Minimal generic + Detailed reason). At success, one Minimal. At start, one Minimal.

NOTE: confirm `state` and `app_handle` identifiers in `start_pairing` (the function takes them as params/State). If `state` is a `State<'_, AppState>`, deref with `&state` / `&*state` as the existing code does for its own calls — match the surrounding usage exactly.

- [ ] **Step 3: Add Detailed per-step + failure events in `handle_pairing_connection` (responder)**

In `handle_pairing_connection`, at each `log_pairing_failure(&state, peer_addr, detail)` call site, ALSO push a Detailed diagnostic: `push_diagnostic(&state, &app_handle, DiagLevel::Detailed, "pairing", Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", detail))`. Add a Minimal "Pairing started" at the top of the function (after `peer_addr` is known) and a Minimal "Pairing succeeded" near the end (where the responder finishes successfully — find the success path, e.g. after gossiping the new peer / before returning Ok). At the lockout site (line ~469, `emit("pairing-locked-out")`), add a Minimal event "Pairing listener locked out (too many failures)".

Keep messages short; do NOT include PIN/secret material except in the explicit Debug event from Step 1.

- [ ] **Step 4: Build**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors. `push_diagnostic` is now used.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/pairing/mod.rs
git commit -m "feat: pairing diagnostics capture points; move PIN dump to in-memory Debug event"
```

---

## Task 5: mTLS connect/drop capture in transport

**Files:**
- Modify: `src-tauri/src/transport.rs` (`start_listening` gains a connection-event callback)
- Modify: `src-tauri/src/app.rs` (wire the callback to `push_diagnostic`)

- [ ] **Step 1: Add a connection-event callback to `start_listening`**

In `src-tauri/src/transport.rs`, change `start_listening` to accept a third closure:

```rust
    pub fn start_listening<F, G, H>(&self, on_receive_message: F, on_receive_file: G, on_conn_event: H)
    where
        F: Fn(Vec<u8>, SocketAddr) + Send + Sync + 'static + Clone,
        G: Fn(quinn::RecvStream, SocketAddr) + Send + Sync + 'static + Clone,
        H: Fn(&str, SocketAddr, Option<String>) + Send + Sync + 'static + Clone,
    {
```

In the accept loop:
- After a successful handshake (`Ok(conn)` branch, right after `let remote_addr = conn.remote_address();`, ~line 223): `on_conn_event("connect", remote_addr, None);`
- In the handshake-failure branch (`Err(e)`, ~line 313): `on_conn_event("handshake_failed", remote_for_log, Some(e.to_string()));`
- For drop: clone `on_conn_event` into the spawned message-handler task and, when its `accept_bi` loop breaks (the `Err(_e) =>` arm, ~line 303-307), call `on_conn_event("drop", remote_addr, None);` before `break`. (Clone it into the closure capture like `on_receive_message` is cloned.)

Match the existing clone pattern: each spawned task does `let on_receive_message = on_receive_message.clone();` — add `let on_conn_event = on_conn_event.clone();` likewise where needed.

- [ ] **Step 2: Wire the callback in app.rs**

Find the `start_listening(` call in `src-tauri/src/app.rs`. It currently passes two closures (message + file handlers) that capture `state`/`app_handle`. Add a third closure that maps connection events to diagnostics:

```rust
            move |kind: &str, addr: std::net::SocketAddr, detail: Option<String>| {
                let (level, msg) = match kind {
                    "connect" => (crate::diagnostics::DiagLevel::Minimal, "mTLS connection established".to_string()),
                    "drop" => (crate::diagnostics::DiagLevel::Minimal, "mTLS connection dropped".to_string()),
                    "handshake_failed" => (
                        crate::diagnostics::DiagLevel::Detailed,
                        format!("mTLS handshake failed: {}", detail.unwrap_or_default()),
                    ),
                    _ => (crate::diagnostics::DiagLevel::Detailed, kind.to_string()),
                };
                crate::diagnostics::push_diagnostic(&conn_state, &conn_app, level, "mtls", Some(addr.to_string()), msg);
            },
```

You will need clones of `state` and `app_handle` captured by this closure (e.g. `let conn_state = state.clone(); let conn_app = app_handle.clone();` before the `start_listening` call), mirroring how the existing two closures capture their own clones. Match the exact cloning pattern already used for the message/file closures at that call site.

- [ ] **Step 3: Build**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/transport.rs src-tauri/src/app.rs
git commit -m "feat: mTLS connect/handshake/drop diagnostics via transport callback"
```

---

## Task 6: Frontend — types + the diagnostics panel

**Files:**
- Modify: `src/types.ts` (DiagLevel, DiagnosticEvent)
- Modify: `src/components/settings/DiagnosticsSettings.tsx` (the panel)

- [ ] **Step 1: Types**

In `src/types.ts`, add:

```ts
export type DiagLevel = "minimal" | "detailed" | "debug";

export interface DiagnosticEvent {
  ts_ms: number;
  level: DiagLevel;
  kind: string;
  peer: string | null;
  message: string;
}
```

- [ ] **Step 2: The panel in `DiagnosticsSettings.tsx`**

Add below the existing Verbose toggle. Implement:

```tsx
import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DiagLevel, DiagnosticEvent } from "../../types";
// ...existing imports (clsx, ShieldCheck, SectionHeader, Card, AppSettings)

const LEVEL_ORDER: Record<DiagLevel, number> = { minimal: 0, detailed: 1, debug: 2 };
const LEVELS: DiagLevel[] = ["minimal", "detailed", "debug"];
```

Inside the component, add state + effects:

```tsx
  const [events, setEvents] = useState<DiagnosticEvent[]>([]);
  const [level, setLevel] = useState<DiagLevel>("minimal");
  const [paused, setPaused] = useState(false);
  const [autoScroll, setAutoScroll] = useState(true);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    invoke<DiagnosticEvent[]>("get_diagnostic_events").then(setEvents).catch(() => {});
    const un = listen<DiagnosticEvent>("diagnostic-event", (e) => {
      if (pausedRef.current) return;
      setEvents((prev) => [...prev, e.payload].slice(-1000));
    });
    return () => { un.then((f) => f()); };
  }, []);

  // When unpausing, re-sync from the backend buffer so nothing is missed.
  useEffect(() => {
    if (!paused) invoke<DiagnosticEvent[]>("get_diagnostic_events").then(setEvents).catch(() => {});
  }, [paused]);

  const shown = events.filter((ev) => LEVEL_ORDER[ev.level] <= LEVEL_ORDER[level]);

  useEffect(() => {
    if (autoScroll && listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [shown.length, autoScroll]);

  const clearEvents = () => {
    invoke("clear_diagnostic_events").catch(() => {});
    setEvents([]);
  };
  const copyAll = () => {
    const text = shown
      .map((ev) => `${new Date(ev.ts_ms).toLocaleTimeString()} [${ev.level}] ${ev.kind}${ev.peer ? " " + ev.peer : ""} ${ev.message}`)
      .join("\n");
    navigator.clipboard.writeText(text).catch(() => {});
  };
```

Render the panel (a `<Card>` titled "Event Log"): a toolbar with the level `<select>` (dark-mode-styled — apply Tailwind classes like `bg-white text-zinc-900 dark:bg-zinc-800 dark:text-zinc-50 border ...` to BOTH the `<select>` and its `<option>`s so options are readable in dark mode), Pause / Auto-scroll toggle buttons, Copy all, and a Clear button; below it a scrollable monospace list (`ref={listRef}`) rendering `shown` rows as `time · level/kind · peer · message`. Use the existing toggle/button styles from the surrounding settings components for visual consistency.

Use these specifics:
- `<select value={level} onChange={(e) => setLevel(e.target.value as DiagLevel)}>` mapping `LEVELS` to `<option>`s with capitalized labels.
- Pause button toggles `paused`; Auto-scroll button toggles `autoScroll`; both show active state (emerald) when on.
- Empty state: when `shown.length === 0`, show a muted "No events." line.
- The list container: `className="max-h-60 overflow-y-auto rounded-lg border ... font-mono text-[11px]"`.

- [ ] **Step 3: Build**

Run: `npm run build`
Expected: builds, no type errors.

- [ ] **Step 4: Commit**

```bash
git add src/types.ts src/components/settings/DiagnosticsSettings.tsx
git commit -m "feat: diagnostics event-log panel (level filter, clear/copy/pause/autoscroll)"
```

---

## Task 7: Full build + test sweep

- [ ] **Step 1: Rust tests + strict build**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all pass (incl. the 2 diagnostics tests).
Run: `RUSTFLAGS="-D warnings" cargo build --manifest-path src-tauri/Cargo.toml`
Expected: clean, no warnings.

- [ ] **Step 2: Frontend build**

Run: `npm run build`
Expected: clean.

- [ ] **Step 3: Confirm no PIN reaches the file log**

Run: `grep -rn "PIN: {}\|Network PIN: {}\|pin.as_bytes" src-tauri/src --include=*.rs`
Expected: the only `pin.as_bytes()` hit is inside the `push_diagnostic` Debug call in `pairing/mod.rs` (in-memory, not `tracing`); no `tracing` macro formats a raw PIN value.

- [ ] **Step 4: Manual verification (record results)** — needs two devices on this build:
- Open Settings → Diagnostics. The Event Log panel renders; level defaults to Minimal.
- Pair successfully: Minimal shows "Pairing started" / "Pairing succeeded" and "mTLS connection established"; switch to Detailed to see step/reason detail; Debug reveals the PIN/AEAD line on a forced wrong-PIN attempt.
- The dropdown is readable in dark mode.
- Clear empties the panel; Copy all puts the filtered rows on the clipboard; Pause freezes the stream and resuming re-syncs; Auto-scroll follows the tail.
- Inspect `{temp}/ClusterCutLogs/clustercut.log*`: contains NO PIN value at any setting (including with "Verbose pairing logs" on).

- [ ] **Step 5: Final commit (only if fixups needed)**

```bash
git add -A
git commit -m "test: pin-safe diagnostics verification fixups"
```

---

## Self-Review Notes

- **Spec coverage:** file redaction (3 sites) → Tasks 3 (storage+lib) & 4 (pairing PIN dump→Debug event); in-memory channel + buffer + helper → Task 1; commands + event → Task 2 (+ emit in Task 1); capture points pairing → Task 4, mTLS → Task 5; keep Verbose toggle → untouched (only the PIN dump under it is moved); panel with level filter + Clear/Copy/Auto-scroll/Pause → Task 6; buffer cap 1000 memory-only → Task 1; levels Minimal/Detailed/Debug → Task 1 + filter in Task 6.
- **No placeholders:** new code (module, commands, callback, panel scaffolding) is given in full; pairing/transport capture points specify exact sites + the `push_diagnostic` call shape, with a note to match in-scope identifiers (these are additive calls at known emit/log sites, not unspecified work).
- **Type consistency:** `DiagnosticEvent {ts_ms,level,kind,peer,message}` and `DiagLevel` snake_case match across Rust (`diagnostics.rs`) and TS (`types.ts`); `push_diagnostic(state, app_handle, level, kind, peer, message)` signature used identically in pairing + the app.rs transport closure; commands `get_diagnostic_events`/`clear_diagnostic_events` named consistently.
