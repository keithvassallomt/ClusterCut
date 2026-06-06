# Ephemeral Auto-Mode Pairing PIN Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In Auto mode, the pairing PIN is generated in memory and never written to disk (new PIN per launch, existing on-disk PIN deleted); Provisioned mode keeps persisting it.

**Architecture:** PIN persistence becomes a single mode-aware decision via `establish_network_pin(app, mode)` in storage.rs — provisioned reads/persists from disk; auto deletes the file and returns an ephemeral `generate_pin()`. Startup, the Provisioned→Auto regenerate path, and factory reset all route through it.

**Tech Stack:** Rust (Tauri 2). No new dependencies.

**Reference spec:** `docs/superpowers/specs/2026-06-06-ephemeral-auto-pin-design.md`

**Test command:** `cargo test --manifest-path src-tauri/Cargo.toml`
**Build command:** `cargo build --manifest-path src-tauri/Cargo.toml`

---

## File Structure

- `src-tauri/src/storage.rs` — `generate_pin`, `pin_should_persist`, `establish_network_pin`; refactor `load_network_pin` to use `generate_pin`; replace `regenerate_identity` with `regenerate_network_name`; unit tests.
- `src-tauri/src/app.rs` — startup PIN block uses `establish_network_pin`.
- `src-tauri/src/commands/identity.rs` — `regenerate_network_identity` uses `regenerate_network_name` + `establish_network_pin(app, "auto")`.
- `src-tauri/src/lib.rs` — `perform_factory_reset` uses `establish_network_pin(app, "auto")`.

---

## Task 1: PIN helpers + tests (storage.rs)

**Files:**
- Modify: `src-tauri/src/storage.rs` (extract `generate_pin`, add `pin_should_persist` + `establish_network_pin`, add a `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing tests**

Add this test module at the END of `src-tauri/src/storage.rs` (other `#[cfg(test)]` modules already exist there — add a new one):

```rust
#[cfg(test)]
mod pin_tests {
    use super::{generate_pin, pin_should_persist};

    #[test]
    fn generate_pin_is_six_lowercase_alnum() {
        let pin = generate_pin();
        assert_eq!(pin.len(), 6, "pin was {:?}", pin);
        assert!(
            pin.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "unexpected chars in {:?}",
            pin
        );
    }

    #[test]
    fn only_provisioned_persists() {
        assert!(pin_should_persist("provisioned"));
        assert!(!pin_should_persist("auto"));
        assert!(!pin_should_persist("something-else"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml pin_tests`
Expected: FAIL to compile — `cannot find function generate_pin` / `pin_should_persist`.

- [ ] **Step 3: Extract `generate_pin` and refactor `load_network_pin`**

In `src-tauri/src/storage.rs`, add this private helper immediately ABOVE `load_network_pin`:

```rust
/// Generate a fresh 6-character lowercase-alphanumeric pairing PIN. No disk I/O.
fn generate_pin() -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..6)
        .map(|_| {
            let idx = rand::thread_rng().gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}
```

Then in `load_network_pin`, replace the inline generation block:

```rust
    // Generate new PIN
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let pin: String = (0..6)
        .map(|_| {
            let idx = rand::thread_rng().gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect();

    tracing::info!("Generated New Network PIN: {}", pin);
    save_network_pin(app, &pin);
    pin
}
```

with:

```rust
    // Generate a new PIN and persist it (provisioned-mode path / lazy default).
    let pin = generate_pin();
    tracing::info!("Generated New Network PIN: {}", pin);
    save_network_pin(app, &pin);
    pin
}
```

- [ ] **Step 4: Add `pin_should_persist` and `establish_network_pin`**

In `src-tauri/src/storage.rs`, add these immediately AFTER `save_network_pin`:

```rust
/// Whether this device's pairing PIN should be persisted to disk. Only
/// provisioned mode keeps a stable, user-set PIN across restarts; auto mode is
/// ephemeral (issue 4).
pub(crate) fn pin_should_persist(mode: &str) -> bool {
    mode == "provisioned"
}

/// Establish this device's pairing PIN for the given cluster mode.
///
/// Provisioned mode persists the PIN (a user-set, memorable value must survive
/// restarts), so it reads (and lazily generates + saves) from disk. Auto mode
/// keeps the PIN ephemeral: any on-disk `network_pin` file is deleted and a
/// fresh PIN is generated in memory, never written to disk. The PIN is only
/// needed live during interactive pairing, so a per-launch value is sufficient
/// and avoids storing the secret. See issue 4.
pub fn establish_network_pin(app: &AppHandle, mode: &str) -> String {
    if pin_should_persist(mode) {
        return load_network_pin(app);
    }
    // Auto mode: delete any stored PIN, go ephemeral.
    if let Ok(path) = app.path().resolve("network_pin", BaseDirectory::AppConfig) {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    generate_pin()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml pin_tests`
Expected: PASS (2 tests).

- [ ] **Step 6: Verify the crate builds**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds. A `dead_code` warning for `establish_network_pin` is acceptable here (callers land in Task 2); `generate_pin` and `pin_should_persist` are already used (by `load_network_pin` and the test / `establish_network_pin`). Do NOT add `#[allow(dead_code)]`.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/storage.rs
git commit -m "feat: mode-aware PIN helpers (generate_pin, establish_network_pin) + tests"
```

---

## Task 2: Route startup, regenerate, and factory reset through `establish_network_pin`

**Files:**
- Modify: `src-tauri/src/storage.rs` (replace `regenerate_identity` with `regenerate_network_name`)
- Modify: `src-tauri/src/app.rs` (startup PIN block)
- Modify: `src-tauri/src/commands/identity.rs` (`regenerate_network_identity`)
- Modify: `src-tauri/src/lib.rs` (`perform_factory_reset`)

- [ ] **Step 1: Replace `regenerate_identity` with `regenerate_network_name`**

In `src-tauri/src/storage.rs`, find `regenerate_identity` (it deletes the name and PIN files, then reloads both). Replace the ENTIRE function:

```rust
pub fn regenerate_identity(app: &AppHandle) -> (String, String) {
    let path_resolver = app.path();
    // 1. Delete existing Name/PIN files
    if let Ok(path) = path_resolver.resolve("network_name", BaseDirectory::AppConfig) {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    if let Ok(path) = path_resolver.resolve("network_pin", BaseDirectory::AppConfig) {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }

    // 2. Load (which generates new ones if missing)
    let new_name = load_network_name(app);
    let new_pin = load_network_pin(app);

    (new_name, new_pin)
}
```

with:

```rust
/// Regenerate just the cluster NAME: delete the on-disk name file and return a
/// fresh generated name (`load_network_name` regenerates and persists it). The
/// PIN is handled separately by `establish_network_pin` so its persistence
/// follows the cluster mode (ephemeral in auto — issue 4).
pub fn regenerate_network_name(app: &AppHandle) -> String {
    let path_resolver = app.path();
    if let Ok(path) = path_resolver.resolve("network_name", BaseDirectory::AppConfig) {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    load_network_name(app)
}
```

- [ ] **Step 2: Startup PIN block in app.rs**

In `src-tauri/src/app.rs`, find the `// 3c. Load Network PIN` block:

```rust
                // 3c. Load Network PIN
                let network_pin = load_network_pin(app_handle);
                *state.network_pin.lock().unwrap() = network_pin.clone();
                tracing::info!("Network PIN: {}", network_pin);
```

Replace it with:

```rust
                // 3c. Establish Network PIN — mode-aware: persisted in
                // provisioned, ephemeral (in-memory, file deleted) in auto.
                // Issue 4. Settings are already loaded into state above, so the
                // mode is available here.
                let cluster_mode = state.settings.lock().unwrap().cluster_mode.clone();
                let network_pin = crate::storage::establish_network_pin(app_handle, &cluster_mode);
                *state.network_pin.lock().unwrap() = network_pin.clone();
                tracing::info!("Network PIN established (mode: {})", cluster_mode);
```

CRITICAL: confirm `state.settings` is already populated at this point. There is an earlier `*state.settings.lock().unwrap() = load_settings(app_handle);` (the first settings load) BEFORE this `3c` block — read the surrounding code to verify. If for any reason settings are NOT yet loaded before this block, read the mode directly from disk instead: `let cluster_mode = crate::storage::load_settings(app_handle).cluster_mode;`. Use whichever is correct given the actual ordering; both yield the right mode.

NOTE: the old log printed the PIN value (`tracing::info!("Network PIN: {}", network_pin)`). The replacement intentionally does NOT log the PIN value (it's a secret); it logs the mode instead.

NOTE: `load_network_pin` may now be unused as a direct import in app.rs. If app.rs has a `use crate::storage::{... load_network_pin ...}` import and this was its only use, remove `load_network_pin` from that import to avoid an unused-import warning. Check and adjust.

- [ ] **Step 3: `regenerate_network_identity` in identity.rs**

In `src-tauri/src/commands/identity.rs`, replace the ENTIRE `regenerate_network_identity` command:

```rust
#[tauri::command]
pub(crate) fn regenerate_network_identity(
    state: State<'_, AppState>,
    transport: State<'_, crate::transport::Transport>,
    app_handle: tauri::AppHandle,
) {
    // Regenerate name + PIN files; PIN stays per-device.
    let (name, pin) = crate::storage::regenerate_identity(&app_handle);
    *state.network_pin.lock().unwrap() = pin;

    // The regenerated name is a cluster rename: bump the register + propagate.
    apply_local_rename(&name, &state, &transport, &app_handle);
}
```

with:

```rust
#[tauri::command]
pub(crate) fn regenerate_network_identity(
    state: State<'_, AppState>,
    transport: State<'_, crate::transport::Transport>,
    app_handle: tauri::AppHandle,
) {
    // This command runs when switching to Auto mode. Generate a fresh random
    // name (persisted + version-bumped + propagated by apply_local_rename) and
    // an EPHEMERAL PIN — auto mode never stores the PIN on disk (issue 4).
    let name = crate::storage::regenerate_network_name(&app_handle);
    let pin = crate::storage::establish_network_pin(&app_handle, "auto");
    *state.network_pin.lock().unwrap() = pin;

    apply_local_rename(&name, &state, &transport, &app_handle);
}
```

- [ ] **Step 4: `perform_factory_reset` in lib.rs**

In `src-tauri/src/lib.rs`, the factory reset computes a new name + PIN (around lines 784-785):

```rust
        let new_name_val = load_network_name(app_handle);
        let new_pin_val = load_network_pin(app_handle);
```

Replace the PIN line so the post-reset PIN is ephemeral (factory reset always resets the cluster to auto mode, set a few lines below):

```rust
        let new_name_val = load_network_name(app_handle);
        // Factory reset returns the cluster to auto mode (set below), so the PIN
        // is ephemeral — not persisted (issue 4).
        let new_pin_val = crate::storage::establish_network_pin(app_handle, "auto");
```

Then fix the import on the `use crate::storage::{...}` line that currently includes `load_network_pin` (around line 25: `load_network_name, load_network_pin,`). `load_network_pin` is no longer referenced in lib.rs — replace it in the import with `establish_network_pin` (keep `load_network_name`):

Change `load_network_name, load_network_pin,` to `establish_network_pin, load_network_name,` and update the call above to use the unqualified `establish_network_pin(app_handle, "auto")` if you prefer the imported form, OR keep the fully-qualified `crate::storage::establish_network_pin(...)` and simply REMOVE `load_network_pin` from the import. Pick one and make it consistent (no unused import, no unresolved name). Verify `load_network_pin` has no other use in lib.rs before removing it from the import (it does not — factory reset was its only caller).

- [ ] **Step 5: Verify build + tests**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with NO errors and NO warnings (`establish_network_pin` now has real callers; `regenerate_identity` is gone; no unused imports).
Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/storage.rs src-tauri/src/app.rs src-tauri/src/commands/identity.rs src-tauri/src/lib.rs
git commit -m "feat: ephemeral pairing PIN in auto mode (startup, regenerate, factory reset)"
```

---

## Task 3: Full build + test sweep

- [ ] **Step 1: Full Rust test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all tests pass (existing suite + `pin_tests`).

- [ ] **Step 2: Strict build**

Run: `RUSTFLAGS="-D warnings" cargo build --manifest-path src-tauri/Cargo.toml`
Expected: clean, no warnings.

- [ ] **Step 3: Manual verification (record results)**

- **Auto mode (default):** delete `~/.config/app.clustercut.clustercut/`, launch, note the displayed "My Cluster PIN", quit. Confirm `~/.config/app.clustercut.clustercut/network_pin` does **not** exist. Relaunch; confirm the PIN is **different**.
- **Auto upgrade path:** create a dummy PIN file (`echo abc123 > ~/.config/app.clustercut.clustercut/network_pin`), launch in auto mode, quit, confirm the file was **deleted**.
- **Provisioned mode:** in Settings switch to Provisioned, set a memorable PIN, save. Confirm `network_pin` exists (perms `-rw-------`) and the **same** PIN survives a restart.
- **Pairing in auto:** with two devices on this build, read device A's PIN and pair from device B — confirm it still works.
- **Factory reset:** trigger factory reset / leave network; confirm it returns to auto mode and no `network_pin` file is written.

- [ ] **Step 4: Final commit (only if manual-test fixups were needed)**

```bash
git add -A
git commit -m "test: ephemeral auto-pin verification fixups"
```

---

## Self-Review Notes

- **Spec coverage:** auto ephemeral + delete-on-startup → `establish_network_pin` auto branch (Task 1) wired at startup (Task 2 Step 2); provisioned persists → auto branch skipped, `load_network_pin` retained (Task 1); single chokepoint → `establish_network_pin` used by startup, regenerate, factory reset (Task 2 Steps 2-4); per-launch regeneration → startup calls it every launch; name handling unchanged → `regenerate_network_name` + `apply_local_rename` (Task 2 Steps 1, 3); `set_network_identity` untouched (not modified). Tests → Task 1.
- **Placeholder scan:** none — all code concrete.
- **Type consistency:** `establish_network_pin(&AppHandle, &str) -> String`, `pin_should_persist(&str) -> bool`, `generate_pin() -> String`, `regenerate_network_name(&AppHandle) -> String` — names/signatures used identically across tasks. `regenerate_identity` is fully removed and all its callers updated (only `regenerate_network_identity`).
- **Secret-in-logs:** the startup PIN log no longer prints the PIN value (logs the mode instead).
