# Design: Split `lib.rs` and `App.tsx` into focused modules

**Date:** 2026-05-30
**Status:** Approved (design) — pending implementation plan
**Author:** Keith + Claude

## Problem

Two files have grown far past a maintainable size and mix many unrelated
responsibilities:

- `src-tauri/src/lib.rs` — **5510 lines / 263 KB**. Holds QUIC stream handlers,
  the ~900-line message router, the ~630-line SPAKE2 pairing flow, ~40 Tauri
  `#[command]` handlers, the ~870-line app builder, keyboard shortcuts, and
  assorted network helpers — all inline.
- `src/App.tsx` — **3030 lines / 124 KB**. Holds all type definitions, a UI
  component library, three large view components, six inline modals, the manual-
  sync UI, all backend event wiring, and protocol/encoding helpers that arguably
  belong in the backend.

Beyond size, there is a layering problem the auditor flagged: protocol /
wire-format knowledge (`MIN_COMPATIBLE_PROTOCOL`, payload decoding decisions)
lives in the React UI, when the UI should be concerned with rendering and the
Rust backend with protocol.

## Goals

1. Split both monster files into focused modules, each with one clear purpose,
   communicating through well-defined interfaces.
2. Relocate protocol/decision logic from the TypeScript UI to the Rust backend
   where it crosses the language boundary cleanly.
3. **Preserve behavior.** Every step is behavior-neutral except the two explicit
   protocol relocations, which are surgical and individually verifiable.

## Non-goals

- No feature work, no protocol version bump, no wire-format changes (the
  compatibility relocation reuses the existing `protocol_version` field).
- No renaming of `transport.rs` (it already *is* the QUIC transport; renaming to
  `quic_transport.rs` would churn imports for no behavioral gain). Decision:
  **keep `transport.rs`**.
- No unrelated refactors of the already-split modules (`state.rs`, `storage.rs`,
  `protocol.rs`, `discovery.rs`, `netmon.rs`, `tray.rs`, `dbus.rs`,
  `compression.rs`, `peer.rs`, `clipboard/`).

## Key insight: `crypto.rs` is two concerns

`crypto.rs` (391 lines) already mixes two independently-consumed concerns:

- **SPAKE2 + pairing-AEAD half** — `start_spake2`, `finish_spake2`,
  `pairing_transcript`, `derive_pair_subkeys`, `fresh_pair_nonce`,
  `pair_aead_encrypt`/`decrypt`, `INITIATOR_KC_PLAINTEXT`. Consumed **only** by
  the pairing flow in lib.rs.
- **TLS-verification half** — `verify_tls13_signature`, `verify_tls12_signature`,
  `WebPkiSupportedAlgorithms`, rustls provider glue. Consumed **only** by
  `transport.rs` for mTLS.

Therefore the SPAKE half folds into the new `pairing` module; the TLS half moves
to a new `tls.rs` next to the transport. The existing crypto unit tests move with
their respective halves and **must keep passing** — they are the guardrail for
the riskiest move in this refactor.

## Strategy

**Incremental, compile-gated, one small commit per extraction.** After every
backend extraction: `cargo build && cargo test`. After every frontend
extraction: `tsc --noEmit` / `vite build`. The entire value of a pure
restructure is *zero behavior change*; a green build+test after each step is how
we prove it. Each commit is independently revertible.

Rejected alternatives: big-bang reorganization (un-bisectable if the app
misbehaves); backend-then-frontend big-bang within each half (same risk,
smaller blast radius but still hard to localize).

## Target Rust layout (`src-tauri/src/`)

Existing modules are kept as-is. lib.rs is carved into:

| New module | Moved from lib.rs / crypto.rs | ~LOC |
|---|---|---|
| `pairing.rs` (or `pairing/` dir if it reads cleaner) | `start_pairing`, `handle_pairing_connection`, pairing `#[command]`s, failure/AEAD logging, **+ SPAKE half of crypto.rs and its tests** | ~800 |
| `tls.rs` | **TLS-verification half of crypto.rs** | ~200 |
| `handlers.rs` (or `handlers/` dir) | `handle_message` (~900), `handle_incoming_clipboard_blob_stream`, `handle_incoming_file_stream` | ~1250 |
| `commands/` (dir, by theme): `theme.rs`, `identity.rs`, `settings.rs`, `peers.rs`, `pairing.rs`, `clipboard.rs`, `system.rs` | the ~40 `#[command]` handlers | ~1100 |
| `app.rs` (or `app/setup.rs` + `app/listeners.rs`) | `pub fn run()` builder + spawned loops (discovery / heartbeat / GC / transport listener) | ~870 |
| `shortcuts.rs` | `register_shortcuts`, `handle_shortcut` | ~150 |
| `net_util.rs` | `probe_ip`, `gossip_peer`, `is_local_ip`, `is_in_local_subnet`, firewall helpers | ~250 |

After the split, `crypto.rs` ceases to exist (its two halves having moved), and
lib.rs shrinks to: module declarations, the `Args` struct, and minimal glue —
on the order of a few hundred lines.

Whether `pairing`, `handlers`, and `app` are single files or small directories
is an implementation detail to decide during extraction based on what reads
cleanly; the module *boundaries* above are the contract.

## Target frontend layout (`src/`)

| New file | Moved from App.tsx |
|---|---|
| `types.ts` | all interfaces: `Peer`, `Ttype`, `NearbyNetwork`, `ClipboardBlobPreview`, `ClipboardFormatPreview`, `HistoryItem`, `NotificationSettings`, `AppSettings` |
| `lib/protocol.ts` | `blobFromPayload` (slimmed — see relocation #2), `formatsFromPayload`, `parseProtocolVersion`*, `shortRichLabel` |
| `lib/format.ts` | `timeAgo`, `formatBytes` |
| `components/ui.tsx` | `Badge`, `SectionHeader`, `Card`, `Button`, `IconButton`, `Field`, `Modal` |
| `components/DevicesView.tsx` | the devices view (~225 lines) |
| `components/HistoryView.tsx` | the history view (~235 lines) |
| `components/SettingsView.tsx` | the settings view (~550 lines; may split its cards further if it stays unwieldy) |
| `components/ManualSync.tsx` | `CopyMini`, `ManualSyncFAB`, `ManualSyncModal` |
| `components/Banners.tsx` | legacy-peer + pairing-lockout banners |
| `components/modals/` | the inline modals: Join, Leave, Add-Remote, Incompatible, GnomeExtension, PortWarning, ConnectionFailure |

\* `parseProtocolVersion` is deleted entirely if relocation #1 makes it unused on
the frontend; otherwise it stays in `lib/protocol.ts`.

App.tsx retains top-level state, the backend event-listener wiring (optionally
extracted into a `useBackendEvents` hook), and the top-level layout — dropping
from ~3030 to roughly ~1000 lines.

## Protocol relocation (UI → Rust)

The guiding rule: **decisions and wire-format knowledge → Rust; rendering and
browser APIs → TypeScript.** Code that crosses the boundary is *reimplemented in
Rust*, not relocated verbatim.

### Relocation 1 — protocol compatibility (fully moves to Rust)

- **Today (TS):** `MIN_COMPATIBLE_PROTOCOL = [0,3,3]`, `parseProtocolVersion`,
  `isPeerProtocolCompatible` compute, per peer, whether its advertised
  `protocol_version` is acceptable.
- **After:** Rust owns `MIN_COMPATIBLE_PROTOCOL` and the comparison (it already
  has `parse_protocol_version` in lib.rs). When building each `Peer` for the
  frontend, Rust sets a new `compatible: bool` field. The frontend reads the
  flag and renders the amber warning / incompatibility modal off it.
- **Deleted from TS:** the constant, `parseProtocolVersion`,
  `isPeerProtocolCompatible`.
- **Verification:** a peer on an old protocol version still shows the warning and
  still blocks send; a current peer does not.

### Relocation 2 — `blobFromPayload` (policy moves, DOM work stays)

- **Today (TS):** `blobFromPayload` inspects the payload — if `fetch_id` is
  present it returns a descriptor-only preview (large image fetched separately);
  otherwise it base64-decodes inline bytes, builds a `Blob`, and calls
  `URL.createObjectURL`.
- **After:** the **classification** (descriptor vs inline) becomes a backend-set
  field on the payload struct, set in Rust where the payload is constructed. The
  frontend decoder trusts that field instead of re-deriving it.
- **Stays in TS (cannot move):** base64 → `Uint8Array` → `Blob` →
  `URL.createObjectURL`, plus object-URL revocation. Object URLs are a browser
  API; Rust cannot create them, and the bytes cross the Tauri bridge as base64
  regardless.
- **Verification:** inline images still render thumbnails; large descriptor
  images still show the descriptor preview and fetch on confirm; object URLs are
  still revoked (no leak).

## Verification plan

- **Per step:** `cargo build && cargo test` (backend); `tsc --noEmit` and/or
  `vite build` (frontend). No step lands red.
- **Test guardrails that must stay green:** the SPAKE2/pairing tests (moving with
  the SPAKE half), `protocol.rs` tests, `clipboard/common.rs` and
  `clipboard/rich.rs` tests.
- **No integration tests exist for the app event loop**, so the refactor ends
  with a manual smoke checklist run against a real two-device setup:
  1. Pair two devices via PIN (initiator + responder).
  2. Send/receive plain text both directions.
  3. Send/receive an inline image (thumbnail renders).
  4. Send/receive a large image (descriptor → confirm → fetch).
  5. Send/receive rich text (HTML/RTF) on Linux.
  6. File transfer with progress.
  7. Leave network and re-pair.
  8. Old-protocol peer shows the incompatibility warning (relocation #1).

## Risks & mitigations

- **Riskiest move = SPAKE half of crypto.rs into `pairing`.** Mitigated by moving
  its unit tests with it and gating on `cargo test`.
- **Large `handle_message` move** could subtly drop a match arm. Mitigated by
  extracting it whole first (verbatim), compiling, then optionally sub-splitting.
- **Two protocol relocations change the bridge surface.** Done as their own
  commits, after the mechanical split is green, each with its own smoke check.
- **In-flight work:** user confirmed pairing is working and to proceed now; the
  rich-text-paste work should be landed or stashed before starting to avoid
  overlapping edits in the clipboard path.
