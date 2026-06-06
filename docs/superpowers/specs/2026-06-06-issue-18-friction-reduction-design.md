# Issue #18 — Reduce friction for restricted-network / non-admin use

**Issue:** https://github.com/keithvassallomt/ClusterCut/issues/18
**Branch:** `issue-18`
**Date:** 2026-06-06

## Background

A user running ClusterCut as a non-admin on Windows behind a restrictive
inbound firewall asked for three changes that reduce setup friction in
locked-down environments:

1. A switch to disable configuring the Windows firewall at startup
   (default enabled, persisted in `settings.json`).
2. A switch to disable mDNS advertising (default enabled, persisted in
   `settings.json`).
3. "Add Remote" currently always runs Pair+Connect; it should be able to
   just Connect when the peer is already paired/pinned.

## Decisions

- **Feature 3 UX:** Auto — "Add Remote" tries Connect and falls back to
  Pair+Connect. Single button, no extra UI.
- **mDNS scope:** Advertising only. Browsing/discovery stays on, so the
  device can still find and connect to others while invisible itself.
- **Apply timing:** Apply immediately where feasible; otherwise persist
  and apply on next launch.
- **Firewall OFF semantics:** OFF only *stops adding* the rule. Any rule
  already created by a previous run is left in place (harmless; removing
  it is out of scope).
- **Connect-vs-pair key:** The decision is keyed on the typed IP matching
  the last-known IP of a trusted, fingerprinted known peer.

## Design

### Backward-compatibility note (applies to features 1 & 2)

Both new settings are `bool` with a **default of `true`**. A bare
`#[serde(default)]` on a `bool` deserializes a missing field to `false`,
which would silently disable the feature on upgrade. Both fields therefore
use `#[serde(default = "default_true")]` with a shared helper:

```rust
fn default_true() -> bool { true }
```

This mirrors the existing `default_pairing_accept_enabled` pattern in
`storage.rs`. Both fields are also added to `AppSettings::default()`.

### Feature 1 — Toggle: configure Windows firewall at startup

- **Setting:** `configure_firewall: bool` in `AppSettings`
  (`src-tauri/src/storage.rs`), `#[serde(default = "default_true")]`,
  default `true`.
- **Startup:** the Windows-only `configure_windows_firewall()` spawn at
  `src-tauri/src/app.rs:513` runs only when `configure_firewall` is true.
- **Apply-where-feasible:** in the `save_settings` command
  (`src-tauri/src/commands/settings.rs`), on a Windows OFF→ON transition,
  spawn `configure_windows_firewall()` immediately (same UAC-elevation
  path as startup). OFF persists with no runtime action — existing rules
  are not removed.

### Feature 2 — Toggle: mDNS advertising

- **Setting:** `mdns_advertising: bool` in `AppSettings`,
  `#[serde(default = "default_true")]`, default `true`.
- **Startup:** at `src-tauri/src/app.rs:673`, always create the
  `Discovery` daemon and call `browse()` (discovery must keep working);
  only call `register()` when `mdns_advertising` is true.
- **New method:** `Discovery::unregister(&mut self)` in
  `src-tauri/src/discovery.rs` — unregisters `registered_service` (if any)
  via `daemon.unregister(...)` and clears the stored fullname, keeping the
  daemon and active browse alive. (Distinct from `Drop`, which tears the
  whole daemon down.)
- **Apply-live:** in `save_settings`, compare old vs new `mdns_advertising`:
  - ON→OFF: call `discovery.unregister()`.
  - OFF→ON: call `discovery.register(device_id, network_name, port)`.
  - Inputs come from `AppState`: `local_device_id`, `network_name`, the
    `Discovery` handle (`state.discovery`), and the listening port derived
    from `state.transport.local_addr()` (fallback `4654`, matching
    `get_listening_port`).

### Feature 3 — Add Remote: try Connect, fall back to Pair

- **CIDR input:** unchanged — `add_manual_peer` scans the range and
  connects to reachable peers using pinned fingerprints. Not a first-pair
  entry point.
- **Single IP input:** new Tauri command in
  `src-tauri/src/commands/peers.rs`:

  ```rust
  #[tauri::command]
  async fn add_remote_peer(ip, state, transport, app_handle)
      -> Result<AddRemoteOutcome, String>
  ```

  where `AddRemoteOutcome` is a serde enum serialized for the frontend:

  ```rust
  enum AddRemoteOutcome { Connected, NeedsPairing }
  ```

  Logic:
  1. Parse the typed address (IP or IP:port; default port 4654), same
     parsing as `add_manual_peer`'s single-IP branch.
  2. Scan `known_peers` for an entry that is `is_trusted` **and** has
     `fingerprint.is_some()` **and** whose `ip` equals the parsed IP.
  3. **Found** → run `probe_ip(addr, port, ...)` to (re)establish the
     mTLS connection via the pinned cert, then return `Connected`.
  4. **Not found** → return `NeedsPairing` (no network action).

- **Frontend** (`src/App.tsx`, `submitManualPeer`): the single-IP branch
  (currently calling `startManualPairFlow` directly) instead `invoke`s
  `add_remote_peer`:
  - `Connected` → close the Add Remote modal, clear input (success).
  - `NeedsPairing` → call `startManualPairFlow(input)` to open the
    existing PIN modal. The SPAKE2 pairing flow is unchanged.
  - On error → surface the error as today.

### Cross-cutting / frontend settings

- Add `default_true()` helper to `storage.rs`.
- Add `configure_firewall` and `mdns_advertising` to the frontend
  `AppSettings` type (`src/types.ts`).
- Add two toggles to the Settings view, defaulting on. The firewall
  toggle is **Windows-only** — it is hidden on macOS and Linux, where
  `configure_windows_firewall()` does not exist (`#[cfg(target_os =
  "windows")]`) and there is nothing for it to control. The mDNS
  advertising toggle is shown on all platforms.
- `save_settings` already preserves backend-only fields
  (`flatpak_autostart`); the two new fields are frontend-managed and flow
  through normally.

## Out of scope

- Removing previously-created firewall rules when the toggle is turned off.
- Disabling mDNS browsing/discovery (only advertising is toggled).
- Any change to the SPAKE2 pairing protocol itself.

## Testing

- **Rust unit tests** (`storage.rs`): a `settings.json` missing both new
  fields deserializes them to `true`; round-trip serialize/deserialize
  preserves explicit `false`.
- **`add_remote_peer` decision logic:** unit-test the trusted+fingerprinted
  IP-match predicate returns `NeedsPairing` for unknown/untrusted IPs and
  `Connected` for a matching trusted peer (probe stubbed/guarded as
  feasible given existing test patterns).
- **Manual verification:** firewall toggle gates startup config and applies
  on OFF→ON; mDNS toggle makes the device disappear/reappear in another
  instance's discovery list live; Add Remote with an already-paired IP
  connects without a PIN prompt, and with a new IP opens the PIN modal.
