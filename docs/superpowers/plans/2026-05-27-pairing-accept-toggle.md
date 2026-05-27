# Pairing-Accept Toggle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a user-controlled toggle that closes the SPAKE pairing listener on demand, plus a header-bar status icon that also reflects the existing brute-force lockout.

**Architecture:** Add a persisted boolean `pairing_accept_enabled` to `AppSettings`. Gate the existing pairing-listener accept loop on the new flag alongside the existing `pairing_locked_out` flag. Add two Tauri commands (`get_pairing_accept` / `set_pairing_accept`) and an event (`pairing-accept-changed`). The header button derives its tri-state visual from both `pairingAccepted` and the already-wired `pairingLockedOut` state.

**Tech Stack:** Rust + Tauri (backend), React + TypeScript + lucide-react (frontend). No new dependencies.

**Spec:** [docs/superpowers/specs/2026-05-27-pairing-accept-toggle-design.md](../specs/2026-05-27-pairing-accept-toggle-design.md)

---

## File Structure

**Modify:**
- `src-tauri/src/storage.rs` — add `pairing_accept_enabled: bool` to `AppSettings` + default.
- `src-tauri/src/lib.rs` — two new Tauri commands; gate the pairing accept loop on the new flag; register commands.
- `src/App.tsx` — add `disabled` prop to `IconButton`; add `pairingAccepted` state; add startup invoke + event listener; render new header button.

**No new files.** The feature is small and lives entirely inside existing modules.

---

## Task 1: Persist `pairing_accept_enabled` in `AppSettings`

**Files:**
- Modify: [src-tauri/src/storage.rs:373-397](src-tauri/src/storage.rs#L373-L397) (struct definition)
- Modify: [src-tauri/src/storage.rs:399-419](src-tauri/src/storage.rs#L399-L419) (`Default` impl)

The flag is `true` by default so existing installs and fresh installs both start in the "accepting" state. `#[serde(default)]` lets older `settings.json` files without this field deserialise without migration — serde fills in `false` for a missing bool by default, **but we want `true`**, so the field needs its own default fn.

- [ ] **Step 1: Add the field to the struct**

Insert immediately after the existing `pairing_debug_logs` field at the end of the struct (around line 396):

```rust
    /// User-controlled pause for the SPAKE pairing listener. When `false`,
    /// inbound TCP pairing connections are dropped immediately at the accept
    /// loop, alongside the existing `pairing_locked_out` brute-force defence.
    /// Surfaced in the UI as a header-bar toggle (issue #16).
    #[serde(default = "default_pairing_accept_enabled")]
    pub pairing_accept_enabled: bool,
```

- [ ] **Step 2: Add the default function**

Insert just before the `impl Default for AppSettings` block (around line 398):

```rust
fn default_pairing_accept_enabled() -> bool {
    true
}
```

- [ ] **Step 3: Initialise the field in `Default::default()`**

Add inside the `Default` impl's struct literal (around line 414, after `compress_file_transfers: false,`):

```rust
            pairing_accept_enabled: true,
```

- [ ] **Step 4: Build and confirm it compiles**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: `Finished` with no errors. Warnings about unused symbols are acceptable at this stage — wired up in later tasks.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage.rs
git commit -m "Add pairing_accept_enabled to AppSettings (issue #16)"
```

---

## Task 2: Backend Tauri commands and event

**Files:**
- Modify: [src-tauri/src/lib.rs](src-tauri/src/lib.rs) — add two commands near the existing `is_pairing_locked_out` / `rearm_pairing` block at line 1717; register them in `generate_handler!` at line 3465.

- [ ] **Step 1: Add `get_pairing_accept` and `set_pairing_accept` commands**

Insert immediately after the `rearm_pairing` command (after the closing brace of the function on line 1733):

```rust
/// Read the user's "accept inbound pairing" flag. The pairing listener is
/// gated on this flag AND on `pairing_locked_out` — both must be clear for
/// inbound SPAKE to proceed. See issue #16.
#[tauri::command]
fn get_pairing_accept(state: tauri::State<'_, AppState>) -> bool {
    state.settings.lock().unwrap().pairing_accept_enabled
}

/// Write the user's "accept inbound pairing" flag, persist the change, and
/// emit `pairing-accept-changed` so any subscribed UI surface stays in sync.
/// Does NOT touch `pairing_locked_out` — abuse defence and user intent are
/// orthogonal. See issue #16.
#[tauri::command]
fn set_pairing_accept(
    enabled: bool,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    {
        let mut s = state.settings.lock().unwrap();
        s.pairing_accept_enabled = enabled;
    }
    let snapshot = state.settings.lock().unwrap().clone();
    crate::storage::save_settings(&app_handle, &snapshot);
    let _ = app_handle.emit("pairing-accept-changed", enabled);
    tracing::info!("Pairing accept set to {} by user.", enabled);
}
```

- [ ] **Step 2: Register both commands in `generate_handler!`**

Modify the handler list at line 3465 — after `is_pairing_locked_out,` and `rearm_pairing,` add:

```rust
            is_pairing_locked_out,
            rearm_pairing,
            get_pairing_accept,
            set_pairing_accept,
```

- [ ] **Step 3: Build**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: `Finished` with no errors.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "Add get_pairing_accept / set_pairing_accept commands (issue #16)"
```

---

## Task 3: Gate the pairing accept loop on the new flag

**Files:**
- Modify: [src-tauri/src/lib.rs:3226-3248](src-tauri/src/lib.rs#L3226-L3248) — the closure inside `start_pairing_listener`.

The existing closure already short-circuits on `state.is_pairing_locked_out()` with a `tracing::warn!` (adversarial event — worth warning about). For the manual-pause path we want `tracing::debug!` — a paired peer accidentally re-attempting pairing is expected, not noteworthy.

- [ ] **Step 1: Add the manual-pause check**

Modify the closure body in `start_pairing_listener` (around line 3226). Currently it looks like:

```rust
if let Err(e) = crate::transport::start_pairing_listener(port, move |stream, peer_addr| {
    let state = pairing_state.clone();
    let app = pairing_handle.clone();
    let t = pairing_transport.clone();
    if state.is_pairing_locked_out() {
        tracing::warn!(
            "Pairing TCP accept from {} refused: listener locked out (§H1).",
            peer_addr
        );
        drop(stream);
        return;
    }
    let permit = match state.pairing_slot.clone().try_acquire_owned() {
        // ...
```

Insert the new check **before** the existing lockout check (cheaper path, more common during normal operation):

```rust
if let Err(e) = crate::transport::start_pairing_listener(port, move |stream, peer_addr| {
    let state = pairing_state.clone();
    let app = pairing_handle.clone();
    let t = pairing_transport.clone();
    if !state.settings.lock().unwrap().pairing_accept_enabled {
        tracing::debug!(
            "Pairing TCP accept from {} dropped: pairing paused by user (issue #16).",
            peer_addr
        );
        drop(stream);
        return;
    }
    if state.is_pairing_locked_out() {
        tracing::warn!(
            "Pairing TCP accept from {} refused: listener locked out (§H1).",
            peer_addr
        );
        drop(stream);
        return;
    }
    let permit = match state.pairing_slot.clone().try_acquire_owned() {
        // ... (unchanged)
```

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: `Finished` with no errors.

- [ ] **Step 3: Manual verification (smoke test)**

Run: `npm run tauri dev` (in repo root). Launch a second instance on another machine and attempt to pair. With the toggle still defaulting to `true` and no UI yet, pairing should succeed as it does today (this task does not change behaviour for the default state).

If you have a Rust REPL or another way to flip the flag (e.g., editing the settings file by hand and restarting), set it to `false` and confirm a pairing attempt logs the new `pairing TCP accept from … dropped` line at debug level. Skip if not easy to wire up — Task 5 will give us the UI path.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "Gate pairing accept loop on pairing_accept_enabled (issue #16)"
```

---

## Task 4: Add `disabled` prop to `IconButton`

**Files:**
- Modify: [src/App.tsx:352-388](src/App.tsx#L352-L388)

`IconButton` doesn't currently support a disabled state. We need one for the "abuse-locked" state of the new header button, where clicks should be a no-op and the visual should hint at the non-interactive state.

- [ ] **Step 1: Add the prop**

Modify the function signature at line 352-366:

```tsx
function IconButton({
  label,
  onClick,
  children,
  variant = "ghost",
  active = false,
  danger = false,
  disabled = false,
}: {
  label: string;
  onClick?: () => void;
  children: React.ReactNode;
  variant?: "ghost" | "default";
  active?: boolean;
  danger?: boolean;
  disabled?: boolean;
}) {
```

- [ ] **Step 2: Wire it through to the `<button>`**

Modify the rendered button at line 368-379:

```tsx
  return (
    <button
      onClick={disabled ? undefined : onClick}
      disabled={disabled}
      className={clsx(
        "group relative flex h-10 w-10 items-center justify-center rounded-xl transition focus:outline-none focus:ring-2 focus:ring-emerald-500/40 no-drag",
        disabled && "cursor-not-allowed opacity-60",
        active
          ? "bg-white text-zinc-900 shadow-sm dark:bg-zinc-800 dark:text-zinc-50"
          : danger
            ? "text-red-500 hover:bg-red-50 dark:hover:bg-red-900/20"
            : "text-zinc-500 hover:bg-zinc-900/5 hover:text-zinc-900 dark:text-zinc-400 dark:hover:bg-white/5 dark:hover:text-zinc-50",
        variant === "default" && !active && !danger && "bg-zinc-100 dark:bg-white/5"
      )}
    >
```

- [ ] **Step 3: Typecheck**

Run: `npx tsc --noEmit 2>&1 | head -20`
Expected: no output (clean typecheck).

- [ ] **Step 4: Commit**

```bash
git add src/App.tsx
git commit -m "Add disabled prop to IconButton"
```

---

## Task 5: Wire the header button (frontend state + UI)

**Files:**
- Modify: [src/App.tsx](src/App.tsx) — add `pairingAccepted` state, initial fetch, event listener, header button.

- [ ] **Step 1: Add `pairingAccepted` state**

Add immediately after the existing `pairingLockedOut` declaration at line 478:

```tsx
  // Issue #16: user-controlled pause for inbound pairing. Persists across
  // restart via AppSettings.pairing_accept_enabled.
  const [pairingAccepted, setPairingAccepted] = useState(true);
```

- [ ] **Step 2: Add the initial fetch**

Add inside the same `useEffect` block as the existing `is_pairing_locked_out` fetch (around line 809-811):

```tsx
    invoke<boolean>("is_pairing_locked_out")
      .then(setPairingLockedOut)
      .catch(() => {});

    // Issue #16: initial fetch for the user-controlled pairing toggle.
    invoke<boolean>("get_pairing_accept")
      .then(setPairingAccepted)
      .catch(() => {});
```

- [ ] **Step 3: Subscribe to `pairing-accept-changed`**

Add inside the listener-setup `useEffect` near the existing `pairing-locked-out` / `pairing-rearmed` listeners (around line 870-874):

```tsx
    const unlistenPairingLocked = listen<void>("pairing-locked-out", () => {
      setPairingLockedOut(true);
    });
    const unlistenPairingRearmed = listen<void>("pairing-rearmed", () => {
      setPairingLockedOut(false);
    });
    // Issue #16: keep state in sync with any other surface that might toggle
    // the flag (tray menu in the future, etc.).
    const unlistenPairingAcceptChanged = listen<boolean>("pairing-accept-changed", (event) => {
      setPairingAccepted(event.payload);
    });
```

And in the cleanup block at the bottom of the same `useEffect` (near line 1029), add:

```tsx
      unlistenPairingLocked.then((f) => f());
      unlistenPairingRearmed.then((f) => f());
      unlistenPairingAcceptChanged.then((f) => f());
```

- [ ] **Step 4: Insert the header button**

In the header bar at line 1509 (between the vertical divider and the "Leave Cluster" `IconButton`):

```tsx
            <div className="mx-2 h-6 w-px bg-zinc-200 dark:bg-zinc-700" />

            {/* Issue #16: pairing-accept toggle. Tri-state visual:
                  green Unlock  = accepting
                  gray  Lock    = user-paused
                  rose  Lock    = abuse-locked-out (non-interactive; the
                                  red banner remains the rearm path) */}
            {(() => {
              const pairingState = pairingLockedOut
                ? "locked"
                : pairingAccepted
                  ? "accepting"
                  : "paused";
              const label =
                pairingState === "locked"
                  ? "Pairing locked — too many failed attempts"
                  : pairingState === "accepting"
                    ? "Pairing accepted"
                    : "Pairing paused";
              return (
                <IconButton
                  label={label}
                  disabled={pairingState === "locked"}
                  onClick={() => {
                    if (pairingState === "locked") return;
                    const next = !pairingAccepted;
                    setPairingAccepted(next);
                    invoke("set_pairing_accept", { enabled: next }).catch((err) => {
                      logToBackend("set_pairing_accept failed", err);
                      setPairingAccepted(!next);
                    });
                  }}
                >
                  {pairingState === "accepting" ? (
                    <Unlock className="h-5 w-5 text-emerald-500" />
                  ) : pairingState === "paused" ? (
                    <Lock className="h-5 w-5 text-zinc-400" />
                  ) : (
                    <Lock className="h-5 w-5 text-rose-500" />
                  )}
                </IconButton>
              );
            })()}

            <IconButton
              danger
              onClick={() => setLeaveOpen(true)}
              label="Leave Cluster"
            >
              <Unplug className="h-5 w-5" />
            </IconButton>
```

- [ ] **Step 5: Typecheck**

Run: `npx tsc --noEmit 2>&1 | head -20`
Expected: no output.

- [ ] **Step 6: Manual verification**

Run: `npm run tauri dev`.

1. **Fresh state:** header shows green `Unlock` icon left of `Leave Cluster`. Hover → "Pairing accepted".
2. **Pause:** click → icon turns gray `Lock`. Hover → "Pairing paused". From a second device, attempt to pair → fails (logs `Pairing TCP accept from … dropped: pairing paused by user` at debug level on the responder).
3. **Resume:** click again → icon turns green. Pairing from the second device now succeeds.
4. **Persistence:** pause, fully quit the app, relaunch → icon is still gray. Settings file at `~/.local/share/clustercut/settings.json` (Linux) contains `"pairing_accept_enabled": false`.
5. **Abuse-lockout interaction:** while accepting, trip the lockout by hammering 10 wrong-PIN pair attempts from a peer. Red banner appears AND header icon flips to rose `Lock`. Hover the header icon → "Pairing locked — too many failed attempts". Clicking the header icon does nothing. Click "Re-enable pairing" in the banner → header icon flips back to green (because the user flag was true throughout).
6. **Paused + locked:** click header to pause (icon gray). Then trip the abuse lockout — icon turns rose (lockout wins precedence). Click banner's "Re-enable pairing" → icon goes back to gray (user flag is still false).

- [ ] **Step 7: Commit**

```bash
git add src/App.tsx
git commit -m "Add header pairing-accept toggle with tri-state status (closes #16)"
```

---

## Task 6: Changelog entry

**Files:**
- Modify: [CHANGELOG.md](../../../CHANGELOG.md) — extend the existing `## [Unreleased]` section.

- [ ] **Step 1: Add an "Added" bullet under `## [Unreleased]`**

The section currently has only a "Fixed" subsection (issue #15). Add an "Added" subsection above it:

```markdown
## [Unreleased]

### Added
- Header-bar toggle to pause inbound pairing on demand. Green unlock = accepting, gray lock = paused. Setting persists across restarts. The same icon also turns rose when the existing brute-force lockout trips, so the header reflects the listener's actual state. Thanks to @mdunphy for the request (#16).

### Fixed
- ...
```

Keep it terse — single paragraph per [feedback_terse_changelog.md](../../../../.claude/projects/-home-keith-LocalCode-keithvassallomt-ClusterCut/memory/feedback_terse_changelog.md).

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "Changelog: pairing-accept toggle"
```

---

## Self-Review

**Spec coverage:**
- Persisted `pairing_accept_enabled` in `AppSettings`: Task 1. ✓
- Tauri commands `get_pairing_accept` / `set_pairing_accept`: Task 2. ✓
- `pairing-accept-changed` event: Task 2 (emit) + Task 5 (subscribe). ✓
- Listener gating on the new flag, debug-level log: Task 3. ✓
- Flags remain independent (manual pause does not touch lockout, banner does not touch user flag): enforced by command implementations in Task 2 (no cross-writes) and listener gating in Task 3 (both checks AND-gated). ✓
- Tri-state header icon (accepting / paused / locked): Task 5. ✓
- Non-interactive locked state with tooltip explaining: Task 4 (disabled prop) + Task 5 (usage). ✓
- Both icons already imported: verified — line 10 of App.tsx imports `Lock` and `Unlock`. ✓
- Test plan from spec is covered by Task 5 Step 6 manual verification. ✓

**Placeholder scan:** No TBD / TODO / "handle edge cases" / "similar to Task N" / unspecified types. ✓

**Type consistency:**
- `pairing_accept_enabled: bool` everywhere (struct field, default fn, command return type).
- Frontend `pairingAccepted: boolean`, command name `set_pairing_accept` / `get_pairing_accept`, event name `pairing-accept-changed`, all consistent across Tasks 2 and 5.
- IconButton `disabled?: boolean` prop matches usage in Task 5.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-27-pairing-accept-toggle.md`. Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using `executing-plans`, batch execution with checkpoints.

Which approach?
