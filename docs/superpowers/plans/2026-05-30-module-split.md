# Split `lib.rs` and `App.tsx` into modules — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Break the 5510-line `src-tauri/src/lib.rs` and 3030-line `src/App.tsx` into focused, single-responsibility modules, and relocate two pieces of protocol logic from the TypeScript UI into the Rust backend — with zero behavior change except those two surgical relocations.

**Architecture:** Incremental, compile-gated extraction. Phase 1 splits the Rust backend leaf-first (pairing → helpers → handlers → commands → shortcuts → app builder). Phase 2 splits the React frontend (types → utils → ui → views → modals). Phase 3 does the two protocol relocations. Every task ends green on build+test and is its own commit.

**Tech Stack:** Rust (Tauri 2, quinn/QUIC, rustls mTLS, spake2), TypeScript + React (Vite), Just for build orchestration.

---

## How to work this plan (move-refactor conventions)

This is mostly a **code-move** refactor, not new-feature TDD. The "test" for a
mechanical move is that the **existing** suite still compiles and passes. So most
tasks follow this rhythm instead of red-green:

1. Create the new file; cut the named items out of the source file into it.
2. Add `use` imports the moved code needs; mark moved items `pub(crate)` where
   they're still called from elsewhere.
3. Wire the module (`mod` declaration + re-exports) and fix call sites.
4. **Gate:** run the build + test commands. Must be green.
5. Commit.

**Do not reproduce moved code by hand** — `git mv`-style cut/paste the real
bodies. The plan names *what* moves, *where it goes*, and *what to rewire*. When a
step says "move `fn foo`," it means the entire function body verbatim.

**Verification commands (memorize these):**

- Backend: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20`
  - Expected: `Finished` with no errors; test summary shows `0 failed`.
- Frontend typecheck: `npx tsc` (no emit in this project) — Expected: no output, exit 0.
- Frontend full build (run once at end of Phase 2): `npm run build` — Expected: `built in …`.

**Symbols shift line numbers as you extract.** Locate items by name with
`grep -n 'fn <name>' src-tauri/src/lib.rs`, not by the line numbers in this plan
(they were accurate at authoring time but will drift).

---

# Phase 1 — Backend split (`src-tauri/src/`)

## Task B1: Fold `crypto.rs` into a `pairing/` module

**Why first:** `crypto.rs` is purely SPAKE2/pairing crypto (verified: it has no
TLS code; transport.rs's TLS uses `rustls::crypto`, not our module). It is the
foundation the pairing orchestration sits on, and it carries the unit tests that
guard the riskiest moves.

**Files:**
- Create: `src-tauri/src/pairing/mod.rs`
- Create: `src-tauri/src/pairing/crypto.rs` (moved from `src-tauri/src/crypto.rs`)
- Delete: `src-tauri/src/crypto.rs`
- Modify: `src-tauri/src/lib.rs` (module decls + 2 call sites)

- [ ] **Step 1: Move the file**

```bash
cd src-tauri/src
mkdir -p pairing
git mv crypto.rs pairing/crypto.rs
```

- [ ] **Step 2: Create `pairing/mod.rs` that re-exports the crypto API**

```rust
// src-tauri/src/pairing/mod.rs
mod crypto;

// Re-export the pairing-crypto surface so existing `crypto::X` call sites
// become `pairing::X` with no per-symbol churn.
pub(crate) use crypto::{
    derive_pair_subkeys, finish_spake2, fresh_pair_nonce, pair_aead_decrypt,
    pair_aead_encrypt, pairing_transcript, start_spake2, SpakeState,
    INITIATOR_KC_PLAINTEXT,
};
```

(If `cargo build` later reports an unused-import for any name here, remove that
name from the list — the set above is the full current public surface of
crypto.rs and may include one or two only used inside the pairing flow moved in
B2.)

- [ ] **Step 3: Swap `mod crypto;` for `mod pairing;` in lib.rs**

In `src-tauri/src/lib.rs`, find the line `mod crypto;` (near line 5) and replace
with `mod pairing;`.

- [ ] **Step 4: Repoint the two call sites in lib.rs**

```bash
grep -n 'crypto::' src-tauri/src/lib.rs
```

Replace each `crypto::` with `pairing::` in lib.rs. (At authoring time these were
in `start_pairing` ~line 1831 and `handle_pairing_connection` ~line 2242: e.g.
`crypto::start_spake2(...)` → `pairing::start_spake2(...)`,
`crypto::pairing_transcript(...)` → `pairing::pairing_transcript(...)`, etc.)

- [ ] **Step 5: Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -25`
Expected: `Finished`; the pairing crypto tests (`transcript_role_order_matters`,
`pair_aead_round_trip`, `wrong_pin_makes_aead_fail_closed`, etc.) PASS, `0 failed`.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor: fold crypto.rs into pairing module"
```

---

## Task B2: Move the pairing orchestration + commands into `pairing/mod.rs`

**Files:**
- Modify: `src-tauri/src/pairing/mod.rs` (receives moved code)
- Modify: `src-tauri/src/lib.rs` (loses ~630 lines + pairing commands; `invoke_handler` unchanged in spelling)

**Items to move from lib.rs into `pairing/mod.rs`** (cut entire bodies + their
`#[cfg(test)]` mods if any sit beside them):
- `async fn start_pairing(...)` (the `#[tauri::command]`, ~line 1831)
- `async fn handle_pairing_connection(...)` (~line 2242)
- The pairing command handlers: `is_pairing_locked_out`, `rearm_pairing`,
  `get_pairing_accept`, `set_pairing_accept`, and the pairing failure/AEAD
  helpers `log_pairing_failure`, `record_pairing_aead_failure` (grep them:
  `grep -n 'fn is_pairing_locked_out\|fn rearm_pairing\|fn get_pairing_accept\|fn set_pairing_accept\|fn log_pairing_failure\|fn record_pairing_aead_failure' src-tauri/src/lib.rs`).

- [ ] **Step 1: Cut the listed fns from lib.rs into `pairing/mod.rs`**

Paste them below the re-export block. Keep `#[tauri::command]` attributes intact.
Make any non-command helper `pub(crate)` if it's still referenced from lib.rs
(e.g. `handle_pairing_connection` is called by the TCP listener in `run()`):
change `async fn handle_pairing_connection` →
`pub(crate) async fn handle_pairing_connection`.

- [ ] **Step 2: Add imports at the top of `pairing/mod.rs`**

The moved code references shared types. Add (adjust to what the compiler asks for):

```rust
use crate::protocol::{Message, PairingMessage};
use crate::state::AppState;
use crate::transport::Transport;
use crate::storage; // if pairing persists known_peers / identity
use tauri::{AppHandle, Emitter, Manager, State};
```

The simplest reliable approach: run `cargo build`, then add each import the error
list names. The re-exported `crypto::*` names are already in scope via
`pairing::` — inside this module call them as `crypto::start_spake2` (private
submodule) **or** keep the `pairing::` form; either resolves.

- [ ] **Step 3: Fix the command registration in `run()`**

In lib.rs `run()`, the `tauri::generate_handler![...]` macro lists command names.
Because the moved commands now live in `pairing`, prefix them there:
`start_pairing` → `pairing::start_pairing`, `is_pairing_locked_out` →
`pairing::is_pairing_locked_out`, etc.

```bash
grep -n 'generate_handler!' src-tauri/src/lib.rs
```

Edit that macro invocation so each moved command is path-qualified.

- [ ] **Step 4: Repoint the TCP pairing listener call site**

In `run()`, the spawned TCP listener calls `handle_pairing_connection(...)`.
Change it to `pairing::handle_pairing_connection(...)`.

- [ ] **Step 5: Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -25 && cargo test 2>&1 | tail -25`
Expected: `Finished`, `0 failed`.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor: move pairing flow and commands into pairing module"
```

---

## Task B3: Extract network helpers into `net_util.rs`

**Files:**
- Create: `src-tauri/src/net_util.rs`
- Modify: `src-tauri/src/lib.rs`

**Items to move** (grep each, cut whole body):
- `fn parse_protocol_version(s: &str) -> Option<(u32,u32,u32)>` (~1132)
- `fn gossip_peer(...)` (~1182)
- `async fn probe_ip(...)` (~1435)
- `fn is_local_ip(...)` (~5479)
- `fn is_in_local_subnet(...)` (~5494)
- the Windows firewall helper(s) (`grep -n 'firewall' src-tauri/src/lib.rs`)

- [ ] **Step 1: Create `net_util.rs` and move the fns**

Mark each `pub(crate)` (all are called from lib.rs and/or handlers). Add imports
the compiler requests (`std::net::{IpAddr, SocketAddr}`, `crate::state::AppState`,
`crate::peer::Peer`, etc.).

- [ ] **Step 2: Wire and repoint**

Add `mod net_util;` to lib.rs. Update call sites: bare `parse_protocol_version(…)`
→ `net_util::parse_protocol_version(…)`, same for `gossip_peer`, `probe_ip`,
`is_local_ip`, `is_in_local_subnet`, firewall helper.

```bash
grep -n 'parse_protocol_version\|gossip_peer\|probe_ip\|is_local_ip\|is_in_local_subnet' src-tauri/src/lib.rs
```

- [ ] **Step 3: Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -15`
Expected: `Finished`, `0 failed`.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor: extract network helpers into net_util"
```

---

## Task B4: Extract message + stream handlers into `handlers.rs`

**Files:**
- Create: `src-tauri/src/handlers.rs`
- Modify: `src-tauri/src/lib.rs`

**Items to move:**
- `async fn handle_message(msg, addr, listener_state, listener_handle, transport_inside)` (~4236, ~900 lines)
- `async fn handle_incoming_clipboard_blob_stream(...)` (~3881)
- `async fn handle_incoming_file_stream(...)` (~4040)

- [ ] **Step 1: Create `handlers.rs`, move all three fns**

Mark all three `pub(crate)` (called from the transport listener in `run()`). Cut
verbatim — `handle_message` is large; do not retype it.

- [ ] **Step 2: Add imports**

Build once, add what the compiler names. Expect:

```rust
use crate::protocol::{Message, /* payload structs used in the match arms */};
use crate::state::AppState;
use crate::transport::Transport;
use crate::{net_util, pairing, compression, storage};
use tauri::{AppHandle, Emitter, Manager};
use std::net::SocketAddr;
```

If `handle_message` calls any private lib.rs helper not yet moved, mark that
helper `pub(crate)` in lib.rs and call it as `crate::<name>`.

- [ ] **Step 3: Wire and repoint**

Add `mod handlers;`. In `run()`, the transport listener calls `handle_message(…)`
and the stream dispatchers call the two `handle_incoming_*` fns — prefix each with
`handlers::`.

```bash
grep -n 'handle_message\|handle_incoming_clipboard_blob_stream\|handle_incoming_file_stream' src-tauri/src/lib.rs
```

- [ ] **Step 4: Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -30 && cargo test 2>&1 | tail -15`
Expected: `Finished`, `0 failed`.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor: extract message and stream handlers into handlers module"
```

---

## Task B5: Extract the remaining commands into a `commands/` module

**Files:**
- Create: `src-tauri/src/commands/mod.rs`
- Create: `src-tauri/src/commands/{theme,identity,settings,peers,clipboard,system}.rs`
- Modify: `src-tauri/src/lib.rs`

**Grouping** (the ~30 non-pairing `#[tauri::command]`s; pairing ones already moved
in B2). Move each into its themed file, keeping `#[tauri::command]` intact:

- `commands/theme.rs`: `get_theme_override`, `get_current_theme`,
  `configure_autostart`, `get_autostart_state`, `show_native_notification`
- `commands/identity.rs`: `get_device_id`, `get_network_name`, `get_network_pin`,
  `get_hostname`, `set_network_identity`, `regenerate_network_identity`
- `commands/settings.rs`: `get_settings`, `save_settings`
- `commands/peers.rs`: `get_peers`, `get_known_peers`, `get_legacy_peers`,
  `dismiss_legacy_peer_banner`, `expects_remote_manual_peers`, `get_listening_port`,
  `get_local_ip`, `add_manual_peer`, `leave_network`, `delete_peer`, `retry_connection`
- `commands/clipboard.rs`: `send_clipboard`, `set_local_clipboard`,
  `set_local_clipboard_files`, `delete_history_item`, `confirm_pending_clipboard`,
  `promote_pending_rich`, `request_file`
- `commands/system.rs`: `log_frontend`, `exit_app`, `check_gnome_extension_status`,
  `get_launch_args`, and `perform_factory_reset` if it's a command

(Verify the full command set: `grep -n '#\[tauri::command\]' src-tauri/src/lib.rs`
— every one not already in `pairing` must land in exactly one file above.)

- [ ] **Step 1: Create the six files; move each command into its themed file**

Add a `commands/mod.rs`:

```rust
pub mod clipboard;
pub mod identity;
pub mod peers;
pub mod settings;
pub mod system;
pub mod theme;
```

- [ ] **Step 2: Add imports per file**

Each file needs `use crate::state::AppState;`, `use tauri::{State, AppHandle, ...}`
and whatever it touches (`crate::storage`, `crate::transport::Transport`,
`crate::net_util`, `crate::pairing`, `crate::handlers`, `crate::protocol::*`).
Build iteratively, adding what the compiler names.

- [ ] **Step 3: Wire and repoint registration**

Add `mod commands;` to lib.rs. In the `generate_handler![...]` macro, path-qualify
every moved command: `get_peers` → `commands::peers::get_peers`,
`send_clipboard` → `commands::clipboard::send_clipboard`, etc. Also repoint any
internal call sites in `run()`/handlers that call these fns directly.

- [ ] **Step 4: Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -30 && cargo test 2>&1 | tail -15`
Expected: `Finished`, `0 failed`.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor: extract Tauri commands into commands module"
```

---

## Task B6: Extract keyboard shortcuts into `shortcuts.rs`

**Files:**
- Create: `src-tauri/src/shortcuts.rs`
- Modify: `src-tauri/src/lib.rs`

**Items to move:** `fn register_shortcuts(app_handle)` (~5244),
`fn handle_shortcut(app_handle, shortcut, event)` (~5286).

- [ ] **Step 1: Move both fns; mark `register_shortcuts` `pub(crate)`**

`handle_shortcut` can stay private to the module (called only by
`register_shortcuts`). Add imports: `tauri::{AppHandle, Manager, Emitter}`,
`tauri_plugin_global_shortcut::{Shortcut, ShortcutEvent, ...}`,
`crate::{commands, handlers, state::AppState, transport, net_util}` as the
compiler requires (the shortcut handler inlines send/receive logic).

- [ ] **Step 2: Wire and repoint**

Add `mod shortcuts;`. In `run()`, change `register_shortcuts(…)` →
`shortcuts::register_shortcuts(…)`.

- [ ] **Step 3: Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -15`
Expected: `Finished`, `0 failed`.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor: extract keyboard shortcuts into shortcuts module"
```

---

## Task B7: Extract the app builder into `app.rs`

**Files:**
- Create: `src-tauri/src/app.rs`
- Modify: `src-tauri/src/lib.rs` (becomes module decls + `Args` + a thin `run()` shim)

**Items to move:** the body of `pub fn run()` (~2977, ~870 lines) — the Rustls
init, logging setup, plugin registration, Tauri builder, state setup, spawned
loops (discovery, transport listener, heartbeat, GC), `generate_handler!`, and the
window/run event handlers.

- [ ] **Step 1: Move `run()`'s body into `app.rs`**

Create `pub(crate) fn run()` in `app.rs` with the full body. Keep the `Args`
struct in lib.rs (it's CLI-parsing glue) unless it's only used by `run()`, in
which case move it too and re-export.

- [ ] **Step 2: lib.rs keeps a one-line shim**

```rust
// src-tauri/src/lib.rs — run() now delegates
pub fn run() {
    app::run();
}
```

Add `mod app;`. Ensure every module the builder references is declared in lib.rs
(`mod` lines) — `app.rs` reaches them via `crate::`.

- [ ] **Step 3: Add imports to `app.rs`**

This is the widest import set: `crate::{commands, handlers, pairing, shortcuts,
net_util, discovery, netmon, state, storage, transport, tray, dbus, clipboard}`,
plus all the Tauri/quinn/tokio symbols the builder uses. Build iteratively.

- [ ] **Step 4: Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -30 && cargo test 2>&1 | tail -15`
Expected: `Finished`, `0 failed`.

- [ ] **Step 5: Confirm lib.rs shrank**

Run: `wc -l src-tauri/src/lib.rs` — Expected: a few hundred lines (down from 5510).

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor: extract app builder into app module; lib.rs is now glue"
```

---

# Phase 2 — Frontend split (`src/`)

All frontend tasks gate on `npx tsc` (typecheck) after the move. Run
`npm run build` once at the end of the phase.

## Task F1: Extract type definitions into `types.ts`

**Files:**
- Create: `src/types.ts`
- Modify: `src/App.tsx`

**Items to move** (the interfaces, ~lines 30–250): `Peer`, `Ttype`,
`NearbyNetwork`, `ClipboardBlobPreview`, `ClipboardFormatPreview`, `HistoryItem`,
`NotificationSettings`, `AppSettings`.

- [ ] **Step 1: Move interfaces into `src/types.ts`, each `export`ed**

```bash
grep -nE '^(export )?(interface|type) ' src/App.tsx
```

- [ ] **Step 2: Import them back into App.tsx**

Add at the top of App.tsx:

```ts
import type {
  Peer, Ttype, NearbyNetwork, ClipboardBlobPreview, ClipboardFormatPreview,
  HistoryItem, NotificationSettings, AppSettings,
} from "./types";
```

- [ ] **Step 3: Typecheck**

Run: `npx tsc`
Expected: no output, exit 0.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor: extract frontend type definitions into types.ts"
```

---

## Task F2: Extract formatting helpers into `lib/format.ts`

**Files:**
- Create: `src/lib/format.ts`
- Modify: `src/App.tsx`

**Items to move:** `timeAgo`, `formatBytes` (~lines 199–226). Export both.

- [ ] **Step 1: Move + export; import back into App.tsx**

```ts
import { timeAgo, formatBytes } from "./lib/format";
```

- [ ] **Step 2: Typecheck** — Run: `npx tsc` — Expected: exit 0.
- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "refactor: extract formatting helpers into lib/format.ts"
```

---

## Task F3: Extract protocol/encoding helpers into `lib/protocol.ts`

**Files:**
- Create: `src/lib/protocol.ts`
- Modify: `src/App.tsx`

**Items to move:** `MIN_COMPATIBLE_PROTOCOL`, `parseProtocolVersion`,
`isPeerProtocolCompatible`, `blobFromPayload`, `formatsFromPayload`,
`shortRichLabel`. Export each. (These move as-is now; Phase 3 changes their
internals/consumers.)

- [ ] **Step 1: Move + export; import back into App.tsx and any view that uses them**

```ts
import {
  MIN_COMPATIBLE_PROTOCOL, parseProtocolVersion, isPeerProtocolCompatible,
  blobFromPayload, formatsFromPayload, shortRichLabel,
} from "./lib/protocol";
```

`blobFromPayload`/`formatsFromPayload` import the `ClipboardBlobPreview` /
`ClipboardFormatPreview` types from `../types`.

- [ ] **Step 2: Typecheck** — Run: `npx tsc` — Expected: exit 0.
- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "refactor: extract protocol/encoding helpers into lib/protocol.ts"
```

---

## Task F4: Extract the UI component library into `components/ui.tsx`

**Files:**
- Create: `src/components/ui.tsx`
- Modify: `src/App.tsx`

**Items to move** (~lines 255–455): `Badge`, `SectionHeader`, `Card`, `Button`,
`IconButton`, `Field`, `Modal`. Export each. Move any small style helpers they
depend on alongside them.

- [ ] **Step 1: Move + export; import back**

```ts
import { Badge, SectionHeader, Card, Button, IconButton, Field, Modal } from "./components/ui";
```

- [ ] **Step 2: Typecheck** — Run: `npx tsc` — Expected: exit 0.
- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "refactor: extract UI component library into components/ui.tsx"
```

---

## Task F5: Extract `DevicesView` into its own file

**Files:**
- Create: `src/components/DevicesView.tsx`
- Modify: `src/App.tsx`

`DevicesView` (~1823–2047) reads peers/nearby-networks and several callbacks from
the App scope. Convert those into an explicit `Props` interface.

- [ ] **Step 1: Define `DevicesViewProps`**

Identify every prop the component currently closes over (peers, nearby networks,
expanded-network state + setter, pairing/join handlers, etc.). Declare:

```ts
// in DevicesView.tsx
export interface DevicesViewProps {
  // one field per value/callback the component uses from App
}
export function DevicesView(props: DevicesViewProps) { /* moved JSX */ }
```

- [ ] **Step 2: Move the component; import types/ui/helpers it needs**

From `../types`, `./ui`, `../lib/format`, `../lib/protocol` as used.

- [ ] **Step 3: Render it from App.tsx with explicit props**

Replace the inline `<DevicesView/>` usage with the prop-passing call.

- [ ] **Step 4: Typecheck** — Run: `npx tsc` — Expected: exit 0.
- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor: extract DevicesView component"
```

---

## Task F6: Extract `HistoryView` into its own file

**Files:**
- Create: `src/components/HistoryView.tsx`
- Modify: `src/App.tsx`

`HistoryView` (~2049–2285) owns local `progress` / `downloadedFiles` state — keep
that state inside the component; only the history items + action callbacks become
props.

- [ ] **Step 1: Define `HistoryViewProps`** (history items, send/receive/delete
  callbacks, `request_file` trigger, isAutoSend, etc.).
- [ ] **Step 2: Move the component; keep its `useState` for progress/downloads;
  import `../types`, `./ui`, `../lib/format`, `../lib/protocol`.**
- [ ] **Step 3: Render from App.tsx with explicit props.**
- [ ] **Step 4: Typecheck** — Run: `npx tsc` — Expected: exit 0.
- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor: extract HistoryView component"
```

---

## Task F7: Extract `SettingsView` into its own file

**Files:**
- Create: `src/components/SettingsView.tsx`
- Modify: `src/App.tsx`

`SettingsView` (~2287–2840, ~550 lines) owns local settings/dirty-check state and
the autosave effect — keep those inside. Props are the loaded settings, the save
callback, identity provisioning callbacks, and autostart toggle.

- [ ] **Step 1: Define `SettingsViewProps`.**
- [ ] **Step 2: Move the component (incl. its autosave `useEffect` and local
  state); import `../types`, `./ui`, plus `ShortcutRecorder` from
  `./ShortcutRecorder`.**
- [ ] **Step 3: Render from App.tsx with explicit props.**
- [ ] **Step 4: Typecheck** — Run: `npx tsc` — Expected: exit 0.
- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor: extract SettingsView component"
```

---

## Task F8: Extract ManualSync, Banners, and the modals

**Files:**
- Create: `src/components/ManualSync.tsx` (`CopyMini`, `ManualSyncFAB`, `ManualSyncModal`)
- Create: `src/components/Banners.tsx` (legacy-peer + pairing-lockout banners)
- Create: `src/components/modals/index.tsx` (Join, Leave, AddRemote, Incompatible, GnomeExtension, PortWarning, ConnectionFailure)
- Modify: `src/App.tsx`

- [ ] **Step 1: Move ManualSync trio into `ManualSync.tsx` with a `Props` interface; render from App.**
- [ ] **Step 2: Move the two banner blocks into `Banners.tsx` (props: the flags + dismiss/rearm callbacks); render from App.**
- [ ] **Step 3: Move each inline modal into `modals/index.tsx` as a named export with explicit props; render from App.**
- [ ] **Step 4: Typecheck** — Run: `npx tsc` — Expected: exit 0.
- [ ] **Step 5: Full build (first time this phase)**

Run: `npm run build`
Expected: `tsc` clean, then `vite … built in …`, no errors.

- [ ] **Step 6: Confirm App.tsx shrank** — Run: `wc -l src/App.tsx` — Expected:
  ~1000 lines (down from 3030).
- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "refactor: extract ManualSync, Banners, and modals into components"
```

---

# Phase 3 — Protocol relocation (UI → Rust)

## Task R1: Move protocol-compatibility decision to Rust

**Goal:** Rust computes per-peer compatibility and ships it as a boolean; the
frontend stops owning `MIN_COMPATIBLE_PROTOCOL` and the version compare.

**Files:**
- Modify: `src-tauri/src/peer.rs` (add field) and wherever the frontend `Peer`
  payload is built/emitted (search: `protocol_version`)
- Modify: `src-tauri/src/net_util.rs` (already holds `parse_protocol_version`)
- Modify: `src/types.ts`, `src/lib/protocol.ts`, and peer-rendering consumers

- [ ] **Step 1 (backend): Add the constant + compatibility fn in Rust**

In `net_util.rs`:

```rust
/// Minimum wire-protocol version this build interoperates with.
pub(crate) const MIN_COMPATIBLE_PROTOCOL: (u32, u32, u32) = (0, 3, 3);

pub(crate) fn is_protocol_compatible(version: &str) -> bool {
    match parse_protocol_version(version) {
        Some(v) => v >= MIN_COMPATIBLE_PROTOCOL,
        None => false,
    }
}
```

(Confirm the real minimum from the deleted TS constant — it was `[0,3,3]`.)

- [ ] **Step 2 (backend): Add `compatible: bool` to the peer payload emitted to the UI**

Find the struct serialized to the frontend (the `Peer` the UI receives — grep
`protocol_version` in `peer.rs`/`state.rs`/`commands/peers.rs`/`handlers.rs`). Add
a `compatible: bool` field and populate it via `net_util::is_protocol_compatible`
at every construction/emit site (`get_peers`, `peer-update` event payload).

- [ ] **Step 3 (backend): Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -15`
Expected: `Finished`, `0 failed`.

- [ ] **Step 4 (frontend): Consume the flag, delete the TS logic**

- In `src/types.ts`, add `compatible: boolean;` to the `Peer` interface.
- In `src/lib/protocol.ts`, delete `MIN_COMPATIBLE_PROTOCOL`,
  `parseProtocolVersion`, `isPeerProtocolCompatible`.
- At every call site of `isPeerProtocolCompatible(peer)` (peer cards,
  incompatibility modal, pre-send check), replace with `peer.compatible`.

```bash
grep -rn 'isPeerProtocolCompatible\|MIN_COMPATIBLE_PROTOCOL\|parseProtocolVersion' src/
```

Expected after edits: no matches.

- [ ] **Step 5 (frontend): Typecheck + build**

Run: `npx tsc && npm run build`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor: move protocol-compatibility decision from UI to Rust"
```

---

## Task R2: Move `blobFromPayload` classification to Rust (DOM work stays in TS)

**Goal:** The descriptor-vs-inline decision becomes a backend-set field; the
frontend decoder trusts it instead of inspecting `fetch_id`. Base64→Blob→object
URL stays in TypeScript.

**Files:**
- Modify: the Rust clipboard payload struct (grep `fetch_id`) + its build sites
- Modify: `src/types.ts`, `src/lib/protocol.ts`

- [ ] **Step 1 (backend): Add an explicit classification field**

Find the clipboard payload struct sent to the UI (`grep -rn 'fetch_id' src-tauri/src`).
Add a field that names the kind, e.g.:

```rust
// "inline" = bytes travel in the payload; "descriptor" = fetch separately by id
pub kind: ClipboardBlobKind, // or a bool `is_descriptor: bool`
```

Set it where the payload is constructed: descriptor when a `fetch_id` is present
(large image via file-transfer ALPN), inline otherwise. Keep `fetch_id` itself —
the frontend still needs it to trigger the fetch.

- [ ] **Step 2 (backend): Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -15`
Expected: `Finished`, `0 failed`.

- [ ] **Step 3 (frontend): Trust the field in `blobFromPayload`**

In `src/types.ts`, add the new field to the blob-preview/payload type. In
`src/lib/protocol.ts`, change `blobFromPayload` so the inline-vs-descriptor branch
keys off the backend field instead of re-deriving from `fetch_id`. The base64
decode, `Blob` construction, `URL.createObjectURL`, and URL-revocation logic stay
exactly as they are.

- [ ] **Step 4 (frontend): Typecheck + build**

Run: `npx tsc && npm run build`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor: move clipboard-blob classification from UI to Rust"
```

---

# Final verification — manual smoke test (no app-loop integration tests exist)

Run the full build once more, then exercise the app across two real devices.

- [ ] `cd src-tauri && cargo test` — all green.
- [ ] `npm run build` — clean.
- [ ] Launch on two machines (`just run-flatpak` or the dev runner).
- [ ] Pair two devices via PIN (initiator + responder) — succeeds.
- [ ] Send/receive plain text, both directions.
- [ ] Send/receive an inline image — thumbnail renders.
- [ ] Send/receive a large image — descriptor preview → confirm → fetch completes.
- [ ] Send/receive rich text (HTML/RTF) on Linux.
- [ ] File transfer shows progress and lands the file.
- [ ] Leave network, then re-pair — works.
- [ ] A peer on an older protocol version shows the incompatibility warning and
  blocks send (validates R1).

---

## Notes for the executor

- **One commit per task**, exactly as written — keeps each behavior-neutral move
  independently revertible.
- If a `cargo build` after a move surfaces a private helper in lib.rs that the
  moved code needs, mark that helper `pub(crate)` in lib.rs and call it via
  `crate::<name>` — do not duplicate it.
- Do not "improve" logic while moving it. Behavior change is out of scope for
  Phase 1–2; only R1 and R2 change behavior, and only as specified.
- If any task's diff turns out larger than expected, still keep it one commit —
  the gate (green build+test) is what guarantees correctness, not diff size.
