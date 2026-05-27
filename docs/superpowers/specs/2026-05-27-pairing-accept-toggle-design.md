# Pairing-Accept Toggle (Issue #16)

## Background

A ClusterCut device runs a SPAKE2 pairing TCP listener for the entire process lifetime so new peers can join via PIN. Once a user's cluster is fully set up, that listener is open with no remaining purpose — every paired peer thereafter connects over QUIC/mTLS, discovered either via mDNS or via the "Add Remote" flow. Reporter @mdunphy asked for a way to close the SPAKE listener on demand.

There is already a brute-force defence (`pairing_locked_out`) that trips after `PAIRING_FAILURE_LOCKOUT_THRESHOLD` AEAD failures and forces the user to re-arm via a red banner. That mechanism stays as-is; this design adds an independent **user-controlled** pause.

## UX

A new `IconButton` is added to the header bar in [src/App.tsx](src/App.tsx), positioned immediately to the left of the existing "Leave Cluster" button (i.e. between the vertical divider at [App.tsx:1509](src/App.tsx#L1509) and the danger button at [App.tsx:1511-1517](src/App.tsx#L1511-L1517)).

The icon reflects the **effective** pairing-acceptance state — i.e. whether an inbound SPAKE connection would currently succeed. Three distinct states:

| Condition | Icon | Tint | Tooltip | Click |
|---|---|---|---|---|
| Accepting (user flag true AND not abuse-locked) | `Unlock` | `text-emerald-500` | `Pairing accepted` | Pause (set user flag false) |
| User-paused (user flag false AND not abuse-locked) | `Lock` | `text-zinc-400` | `Pairing paused` | Resume (set user flag true) |
| Abuse-locked (`pairing_locked_out = true`, regardless of user flag) | `Lock` | `text-rose-500` | `Pairing locked — too many failed attempts` | Non-interactive (button disabled); the existing red banner's "Re-enable pairing" stays the sole rearm path |

A single click in the two interactive states toggles. No confirmation modal — single-click reversible.

Why the button goes non-interactive in the abuse-locked state instead of also rearming: the abuse-lockout banner already provides a prominent, intentionally-deliberate rearm affordance. Adding a second rearm entry point risks an accidental click clearing a defence the user has not yet acknowledged. The icon's job here is **status**, not control; the banner remains the control surface.

Both icons are already imported in [App.tsx:10](src/App.tsx#L10).

## Backend

### State

Extend `AppSettings` with:

```rust
pub pairing_accept_enabled: bool,  // default: true
```

The flag is persisted in the existing settings file so the user's intent survives restart.

### Tauri commands

- `get_pairing_accept() -> bool` — reads `state.settings.pairing_accept_enabled`.
- `set_pairing_accept(enabled: bool)` — writes the flag, persists settings, emits a `pairing-accept-changed` event with the new value.

Both registered in the existing `tauri::generate_handler!` block in `lib.rs`.

### Listener gating

`handle_pairing_connection` in [src-tauri/src/lib.rs](src-tauri/src/lib.rs) already short-circuits on `state.is_pairing_locked_out()`. Add an equivalent check at the same point:

```rust
if !state.settings.lock().unwrap().pairing_accept_enabled {
    // Drop the connection immediately. No log spam — this is expected.
    return;
}
```

Two flags now gate inbound pairing. **Both must be clear** for SPAKE to proceed. They are orthogonal:

- `pairing_locked_out` (existing): adversarial defence. Cleared only via the red "Re-enable pairing" banner.
- `pairing_accept_enabled` (new): user intent. Toggled only via the new header button.

Toggling one never touches the other.

### Event

`pairing-accept-changed` carries the new `bool` value. The frontend subscribes so any future surface (tray menu, future Settings-tab mirror) stays in sync with the header button. No existing surface listens today, but emitting the event keeps the door open without retrofitting later.

## Frontend

### State

```ts
const [pairingAccepted, setPairingAccepted] = useState(true);
// `pairingLockedOut` already exists at App.tsx:478 and is wired up to
// the `pairing-locked-out` / `pairing-rearmed` events.
```

Initial fetch alongside the other startup invokes in the same `useEffect` as [App.tsx:809-811](src/App.tsx#L809-L811):

```ts
invoke<boolean>("get_pairing_accept").then(setPairingAccepted).catch(() => {});
```

Subscribe to `pairing-accept-changed` in the existing listener-setup `useEffect` (where `pairing-locked-out` and `pairing-rearmed` are already wired up around [App.tsx:870-874](src/App.tsx#L870-L874)).

The button's render is a function of **both** `pairingAccepted` and `pairingLockedOut`:

```ts
const pairingState =
  pairingLockedOut ? "locked"
  : pairingAccepted ? "accepting"
  : "paused";
```

### Header button

```tsx
<IconButton
  label={
    pairingState === "locked"    ? "Pairing locked — too many failed attempts"
    : pairingState === "accepting" ? "Pairing accepted"
    :                                 "Pairing paused"
  }
  disabled={pairingState === "locked"}
  onClick={() => {
    if (pairingState === "locked") return;
    const next = !pairingAccepted;
    setPairingAccepted(next);                              // optimistic
    invoke("set_pairing_accept", { enabled: next }).catch(err => {
      logToBackend("set_pairing_accept failed", err);
      setPairingAccepted(!next);                           // rollback on error
    });
  }}
>
  {pairingState === "accepting"
    ? <Unlock className="h-5 w-5 text-emerald-500" />
    : pairingState === "paused"
    ? <Lock className="h-5 w-5 text-zinc-400" />
    : <Lock className="h-5 w-5 text-rose-500" />}
</IconButton>
```

Inserted between the divider and the "Leave Cluster" `IconButton`. If `IconButton` does not yet support a `disabled` prop, add one (small, contained change in the same component).

## Out of scope

- Tray-menu mirror of the toggle (deferred; emits the event so it's a drop-in addition later).
- Auto-disable after N successful pairings (no user signal for this).
- Mirror toggle in the Settings tab (the header button is the spec).

## Test plan

- New install → header shows green `Unlock`. Verify `get_pairing_accept` returns `true`.
- Click → icon flips to gray `Lock`. Inbound SPAKE from a peer fails (peer should see a generic connect-then-close, no PIN prompt advances past T0).
- Click again → icon flips back to green. Inbound SPAKE succeeds.
- Restart app while paused → header still shows gray `Lock`. Inbound SPAKE still refused.
- Trip the brute-force lockout (10 failed AEAD attempts) while accepting → red banner appears AND the header icon switches to rose-tinted `Lock` with the "Pairing locked — too many failed attempts" tooltip. The header button becomes non-interactive in this state.
- Clear the abuse lockout via the banner's "Re-enable pairing" → header icon reverts to its previous user-flag-driven colour (green if user flag is true, gray if user flag is false).
- Toggle the header button to paused, then trip the abuse lockout → icon turns rose (lockout wins the precedence). After banner rearm → icon goes back to gray because user flag is still false.

## Implementation notes

- Order of the `match` / `if` guards in `handle_pairing_connection`: place the new manual-pause check *before* the `pairing_locked_out` check, since manual pause is the cheaper / more common path. No semantic difference — both close the socket immediately.
- The pairing connection handler should not log a warning for refused-while-paused connections — that's expected behaviour and would create noise if a paired peer (mistakenly) tries to re-pair. `tracing::debug!` is appropriate.
- Persisting `pairing_accept_enabled` reuses the existing settings-save path. No migration required: missing field defaults to `true` via serde's `#[serde(default)]`.
