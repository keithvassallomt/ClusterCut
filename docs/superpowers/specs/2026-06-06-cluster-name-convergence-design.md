# Cluster Name Convergence — Design

**Source:** Email from @mdunphy ("Issue 2"), branch `dunphy-mail`
**Date:** 2026-06-06

## Problem

In a running 2-peer cluster, switching Settings from Provisioned → Auto on one
device gives that device a new cluster name while the other peer keeps the old
name. The divergence survives a restart on both ends. Clipboard sync keeps
working, so it isn't a connectivity bug, but the cluster's "true name" becomes
ambiguous.

## Root cause (verified)

- **`cluster_id`** (a UUID) is the authoritative cluster identity. It drives
  membership, routing, and gossip-loop suppression, and is never regenerated on
  a mode switch — which is why clipboard keeps working
  ([state.rs:39], [app.rs:547]).
- **`network_name`** is a per-device, local-only display label
  ([state.rs:52], [storage.rs] `load_network_name`/`save_network_name`). The UI
  literally shows it as "My Cluster" ([DevicesView.tsx:70], sourced from each
  device's own `get_network_name` at [App.tsx:387]).
- The name is synchronized exactly **once, one-way**: at pairing the joiner
  adopts the responder's `network_name` from the `ClusterInfo` exchange and
  saves it as its own ([pairing/mod.rs:358-367]). After that there is **no
  rename-propagation mechanism**.
- On Provisioned → Auto, the device deletes its own `network_name`, generates a
  new one, and re-registers mDNS ([identity.rs] `regenerate_network_identity`,
  [storage.rs] `regenerate_identity`). Nothing updates the other peer, so the
  two labels diverge permanently.

## Decisions

1. **Model:** the cluster name becomes **one shared, cluster-wide property**
   (replicated across a leaderless set of peers), not a per-device label.
2. **Conflict resolution:** Lamport-style **version counter, newest wins**;
   ties broken by `device_id`.
3. **Mode switch (Provisioned → Auto in an active cluster):** **warn/confirm**
   before renaming. Confirm → regenerate + propagate; Cancel → revert the
   toggle, keep the name. Solo device → regenerate silently.
4. **Upgrade:** **auto-converge**. Pre-existing diverged peers are both
   effectively `version 0`; equal-version ties resolve by `device_id`, so the
   cluster heals to one of the existing names on first reconnect with no user
   action.
5. **PIN is unaffected** — it remains per-device. Only the name becomes shared.

## Design

### Data model — versioned name register

The cluster name becomes a small versioned register stored per device:

- `network_name: String` — the name (unchanged on disk format/location).
- `network_name_version: u64` — Lamport-style counter.
- `network_name_origin: String` — `device_id` of whoever set the current name
  (the tie-breaker).

**Storage:** persist `network_name_version` and `network_name_origin` as **two
new sibling files** next to the existing `network_name` file. The existing
`network_name` file is left exactly as-is (raw text), so there is **no migration
and no risk** to existing installs. **Backward-compat on load:** missing version
file → `0`; missing origin file → the local `device_id`.

**State:** `AppState` gains `network_name_version: Arc<Mutex<u64>>` and
`network_name_origin: Arc<Mutex<String>>`.

### Convergence rule

Define a total order on `(version, origin)`. Incoming `(name, version, origin)`
**wins** iff:

```
incoming.version > local.version
  || (incoming.version == local.version && incoming.origin > local.origin)
```

(`origin` compared as a string.) On a win: adopt all three fields, persist,
re-register mDNS with the new name, emit `network-update`, and **re-gossip** the
winning register to other connected peers. Re-gossip is **deduplicated** by
`(version, origin)` — if we already hold exactly that register, do not re-send,
preventing gossip storms/loops. If the local register is strictly newer than an
incoming one, reply to the sender with the local register so it converges up.

This applies to equal `version == 0` registers too (upgrade auto-converge): two
unversioned peers deterministically converge to the higher-`device_id` name.

### Local rename

A single internal helper, e.g. `apply_local_rename(name)`:

1. `version = local.version + 1` (the local version always tracks the max seen,
   because every accepted incoming register overwrites it, so `+1` beats
   everything currently known).
2. `origin = local device_id`.
3. Persist name + version + origin.
4. Re-register mDNS with the new name.
5. Propagate (see below).

Used by **both** rename entry points:
- Provisioned mode `set_network_identity` (user types a name) — name path runs
  `apply_local_rename`; **PIN handling is unchanged**.
- Auto `regenerate` — generates a random name, then `apply_local_rename`. (PIN
  regeneration unchanged.)

### Propagation

New authenticated, post-pairing message:

```
Message::ClusterName { name: String, version: u64, origin: String }
```

Sent only over the established QUIC/mTLS channel to trusted peers.

- **Push on rename:** after `apply_local_rename`, send `ClusterName` to all
  connected trusted peers. Each recipient runs the convergence rule and
  re-gossips on a win.
- **Anti-entropy on (re)connect:** whenever a peer connects or is rediscovered
  (startup reconnection probe, mDNS `ServiceResolved`), send our current
  `ClusterName` to it. This converges peers that were offline during a rename,
  and lets a peer that renamed while alone propagate on return — no rename event
  required.
- **Pairing seed:** extend the `ClusterInfo` struct ([protocol.rs:287-293]) to
  carry `network_name_version` and `network_name_origin` alongside
  `network_name`, so a new joiner adopts the full register, not just the string
  ([pairing/mod.rs:358-367] updated accordingly).
- **Safe degradation / no floor bump:** `ClusterName` is sent **only** to peers
  advertising a new-enough `proto` version (gate on the advertised mDNS/peer
  `proto`). Older peers are never sent the new message, so there is **no
  protocol-floor bump and no pairing break**; they simply keep today's
  local-name behavior and won't converge. The `CLUSTERCUT_PROTOCOL_VERSION` is
  bumped to mark availability of `ClusterName`, used purely for this gating, not
  to exclude older peers from pairing.

### Mode-switch UX (the reported trigger)

Frontend ([SettingsView.tsx] autosave, currently the
`provisioned → auto` branch at ~166-175):

- **Active cluster** (≥1 trusted peer, derivable from the existing trusted-peers
  list): on Provisioned → Auto, show a confirm dialog:
  *"This will rename the cluster for all devices to a new auto-generated name."*
  - **Confirm** → invoke `regenerate` (now version-bumps + propagates) →
    all peers converge to the new name.
  - **Cancel** → revert the mode toggle back to Provisioned; no rename, no
    state change.
- **Solo** (no trusted peers) → switch to Auto and regenerate silently (still
  version-bumped locally so the register is well-formed for future joiners).
- **Auto → Provisioned** is unchanged: it just lets the user type a name; the
  rename happens through the existing `set_network_identity` path when a valid
  name is entered (which now version-bumps + propagates).

## Out of scope

- `cluster_id` semantics, membership, trust, or PIN behavior (PIN stays
  per-device).
- Disbanding/reforming the cluster on mode switch.
- Convergence with peers older than this feature (graceful degradation only).
- Periodic re-announce beyond connect-time + push (add later only if a gap
  shows up in practice — YAGNI).

## Testing

**Rust unit tests (pure convergence logic):**
- `incoming.version > local` wins; `<` loses; equal-version resolves by
  `origin` (both directions); identical register is a no-op (dedup).
- `apply_local_rename` sets `version = prev + 1` and `origin = local id`.
- Upgrade case: two `version 0` registers converge deterministically by
  `device_id`.

**Backward-compat:**
- Loading a `network_name` file with no version/origin yields `version 0`,
  `origin = local device_id`.

**Manual / integration verification:**
- Two-peer cluster: rename on A (Provisioned set-name) → B converges live.
- Rename A while B offline → B converges on reconnect (anti-entropy).
- Provisioned → Auto on A with B present → confirm dialog; confirm renames both,
  cancel keeps the name and reverts the toggle.
- New device pairing adopts the current name + version + origin.

## File anchors (for the plan)

- `src-tauri/src/state.rs` — add version/origin to `AppState`.
- `src-tauri/src/storage.rs` — load/save version + origin; backward-compat
  defaults; `regenerate_identity` name path.
- `src-tauri/src/commands/identity.rs` — `set_network_identity`,
  `regenerate_network_identity` route through `apply_local_rename`.
- `src-tauri/src/protocol.rs` — `Message::ClusterName`; extend `ClusterInfo`.
- `src-tauri/src/pairing/mod.rs` — adopt full register at join; send seed.
- `src-tauri/src/handlers.rs` — handle inbound `ClusterName`; build extended
  `ClusterInfo`.
- `src-tauri/src/net_util.rs` / `app.rs` — anti-entropy send on probe/mDNS
  rediscovery.
- `src-tauri/src/discovery.rs` — `CLUSTERCUT_PROTOCOL_VERSION` bump + gating.
- `src/components/SettingsView.tsx`, `src/App.tsx` — confirm dialog + active-
  cluster check on mode switch.
