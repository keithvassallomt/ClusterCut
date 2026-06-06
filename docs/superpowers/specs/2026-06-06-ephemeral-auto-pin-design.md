# Ephemeral Pairing PIN in Auto Mode — Design

**Source:** Email from @mdunphy ("Issue 4"), branch `dunphy-mail`
**Date:** 2026-06-06

## Problem

`network_pin` is the SPAKE2 pairing PIN for **this device**. Since v0.3 it is
per-device — pairing never imports the other side's PIN (the old broadcast/shared
PIN was removed). The PIN is used only for:
- display in the UI ("My Cluster PIN"), and
- the pairing **responder** validating an incoming pairing attempt (SPAKE2
  password).

It is **not** used by auto-reconnect or any background flow — those run on
pinned mTLS certs. So nothing needs the PIN to survive a restart; persisting it
only buys a *memorable* PIN, which matters only in Provisioned mode.

Today the PIN is always persisted to disk (`network_pin` file) regardless of
mode. In Auto mode that is an unnecessary stored secret — a means for the PIN to
leak — for zero benefit.

## Decisions

1. **Auto mode: ephemeral PIN.** Generate the PIN in memory at startup, never
   write it to disk. A new PIN is produced on every launch (acceptable —
   pairing is interactive with both devices open).
2. **Auto mode: delete any existing on-disk PIN** on startup, so current users'
   previously-stored PIN is wiped (achieves the security win for upgraders).
3. **Provisioned mode: unchanged.** The PIN is still persisted to disk (with the
   `0600` permissions from the issue-3 work), so a user-set memorable PIN
   survives restarts.
4. PIN persistence becomes the single mode-aware decision, routed through one
   helper so every path (startup, regenerate, factory reset) behaves
   consistently.

## Design

### Helpers (in `src-tauri/src/storage.rs`)

- `fn generate_pin() -> String` — the existing 6-char lowercase-alphanumeric
  random generation, extracted out of `load_network_pin` (no disk I/O). Both
  `load_network_pin` (when generating a missing PIN) and the ephemeral path call
  it, keeping one generator.

- `pub fn establish_network_pin(app: &AppHandle, mode: &str) -> String` — the
  single mode-aware decision point:
  - `mode == "provisioned"` → `load_network_pin(app)` (reads disk; generates and
    saves if missing — unchanged behavior).
  - otherwise (auto) → delete the `network_pin` file if it exists, then return
    `generate_pin()` **without saving**. Ephemeral.

- `pub(crate) fn pin_should_persist(mode: &str) -> bool` — returns `mode ==
  "provisioned"`. A trivially unit-testable expression of the rule;
  `establish_network_pin` uses it.

`load_network_pin` keeps its current behavior (used by the provisioned branch),
refactored only to call `generate_pin()` internally.

### Routing every PIN-establishment path through the helper

- **Startup** (`src-tauri/src/app.rs`, the `// 3c. Load Network PIN` block):
  read `cluster_mode` from the already-loaded `state.settings`, then
  `state.network_pin = establish_network_pin(app, &mode)`. In auto this yields a
  fresh ephemeral PIN and deletes any stale on-disk file; in provisioned it
  reads the persisted PIN. (Settings are loaded into `state.settings` before the
  PIN block, so the mode is available.)

- **Provisioned→Auto switch / `regenerate_network_identity`**
  (`src-tauri/src/commands/identity.rs`): after the switch the mode is auto, so
  the PIN goes through `establish_network_pin(app, "auto")` → ephemeral, file
  deleted. The **name** continues to be regenerated, persisted, version-bumped,
  and propagated via the existing `apply_local_rename` (issue-2 work) — unchanged.

- **Factory reset / leave network** (`perform_factory_reset` in
  `src-tauri/src/lib.rs`): it already resets `cluster_mode` to `"auto"` and
  deletes the `network_pin` file via `reset_network_state`. Replace its
  `load_network_pin` call with `establish_network_pin(app, "auto")` so the
  post-reset PIN is ephemeral (idempotent with the file deletion).

### Unchanged

- `set_network_identity` (provisioned, user-entered PIN) still persists via
  `save_network_pin`.
- `get_network_pin`, the pairing responder (reads `state.network_pin`), and the
  frontend display (re-fetches via `get_network_pin` on startup and on
  `network-update`) — all read runtime state and assume nothing about
  cross-restart stability.

## Testing

- **Unit (`storage.rs`):**
  - `generate_pin()` returns a 6-character string, all chars in
    `[a-z0-9]`.
  - `pin_should_persist("provisioned")` is `true`; `pin_should_persist("auto")`
    and `pin_should_persist("anything-else")` are `false`.
- `establish_network_pin` needs a Tauri `AppHandle` (file I/O), so it is covered
  by manual verification, not a unit test.
- **Manual (Linux):**
  - Auto mode (default): launch, note the PIN, quit, confirm
    `~/.config/app.clustercut.clustercut/network_pin` does **not** exist.
    Relaunch, confirm the PIN is different. Pre-seed a `network_pin` file, launch
    in auto, confirm the file is deleted.
  - Provisioned mode: set a memorable PIN in Settings, confirm `network_pin`
    exists (mode `0600`) and the same PIN survives a restart.
  - Pairing still works in auto mode: read the displayed PIN on device A, pair
    from device B using it.

## Out of scope

- Renaming `network_pin` → a clearer name (e.g. `local_pairing_pin`). The
  misnomer is real, but a rename touches the on-disk filename and many call
  sites for no functional gain; it can be a separate cleanup.
- Any change to Provisioned-mode persistence or to the pairing protocol.

## File anchors (for the plan)

- `src-tauri/src/storage.rs` — `generate_pin`, `establish_network_pin`,
  `pin_should_persist`; refactor `load_network_pin` to use `generate_pin`; unit
  tests.
- `src-tauri/src/app.rs` — startup PIN block uses `establish_network_pin`.
- `src-tauri/src/commands/identity.rs` — `regenerate_network_identity` PIN path
  uses `establish_network_pin(app, "auto")`.
- `src-tauri/src/lib.rs` — `perform_factory_reset` uses
  `establish_network_pin(app, "auto")`.
