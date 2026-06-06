# Secret File Permissions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restrict the two secret files (`device_key.der`, `network_pin`) to owner-only (`0600`) on Unix, both on write and for pre-existing files at startup.

**Architecture:** A small `set_owner_only` helper in `storage.rs` (Unix `0600` via `PermissionsExt`; no-op on Windows where `%APPDATA%` is already per-user ACL-protected). It's called after writing the key and the PIN, and a `harden_secret_files` startup pass chmods those two files if they already exist (covers upgraders, since `device_key.der` is written only once and never rewritten).

**Tech Stack:** Rust (Tauri 2). No new dependencies — `std::os::unix::fs::PermissionsExt`.

**Reference spec:** `docs/superpowers/specs/2026-06-06-secret-file-permissions-design.md`

**Test command:** `cargo test --manifest-path src-tauri/Cargo.toml`
**Build command:** `cargo build --manifest-path src-tauri/Cargo.toml`

---

## File Structure

- `src-tauri/src/storage.rs` — the `set_owner_only` helper, calls in `save_device_cert` + `save_network_pin`, the `harden_secret_files` startup function, and a unit test module.
- `src-tauri/src/app.rs` — call `harden_secret_files` once during setup.

---

## Task 1: `set_owner_only` helper + unit test

**Files:**
- Modify: `src-tauri/src/storage.rs` (add helper near top after imports; add a `#[cfg(test)]` module at end of file)

- [ ] **Step 1: Write the failing test**

Add this test module at the very END of `src-tauri/src/storage.rs` (the file may already contain other `#[cfg(test)]` modules from prior work — that's fine, multiple test modules coexist):

```rust
#[cfg(all(test, unix))]
mod perms_tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn set_owner_only_sets_0600() {
        // Create a world-readable temp file, then harden it.
        let path = std::env::temp_dir().join(format!(
            "clustercut_perms_test_{}",
            std::process::id()
        ));
        std::fs::write(&path, b"secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        super::set_owner_only(&path);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode & 0o777, 0o600, "expected 0600, got {:o}", mode & 0o777);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml perms_tests`
Expected: FAIL to compile — `cannot find function set_owner_only in module ... / super`.

- [ ] **Step 3: Implement `set_owner_only`**

In `src-tauri/src/storage.rs`, add this private helper immediately after the imports block (after the `use tauri::{...};` line, before `pub fn load_network_name`):

```rust
use std::path::Path;

/// Restrict a file to owner-only access. On Unix sets mode 0600. On Windows
/// this is intentionally a no-op: `%APPDATA%\<app>` is already ACL-restricted to
/// the user, SYSTEM, and Administrators by default, so other standard users
/// cannot read it. Best-effort — logs on failure, never panics.
fn set_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            tracing::warn!("Failed to set 0600 on {}: {}", path.display(), e);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path; // Windows: AppData is already per-user ACL-protected.
    }
}
```

NOTE: `fs` is already imported (`use std::fs;`). The new `use std::path::Path;` is needed for the `&Path` parameter — if a `Path`/`PathBuf` import already exists at the top, don't duplicate it; otherwise add the line as shown.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml perms_tests`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage.rs
git commit -m "feat: add set_owner_only (0600 on unix) helper + test"
```

---

## Task 2: Harden on write + at startup

**Files:**
- Modify: `src-tauri/src/storage.rs` (calls in `save_device_cert` + `save_network_pin`; new `harden_secret_files`)
- Modify: `src-tauri/src/app.rs` (call `harden_secret_files` in setup)

- [ ] **Step 1: Harden the key on write in `save_device_cert`**

In `src-tauri/src/storage.rs`, in `save_device_cert`, the key write currently is:

```rust
    if let Err(e) = fs::write(&key_path, key_der) {
        tracing::error!("Failed to write device key: {}", e);
        return;
    }
    tracing::debug!("Saved device cert to disk.");
```

Change it to call `set_owner_only` after the successful key write:

```rust
    if let Err(e) = fs::write(&key_path, key_der) {
        tracing::error!("Failed to write device key: {}", e);
        return;
    }
    // The private key must not be world-readable (issue: secret file perms).
    set_owner_only(&key_path);
    tracing::debug!("Saved device cert to disk.");
```

(The cert file is public; leave it untouched.)

- [ ] **Step 2: Harden the PIN on write in `save_network_pin`**

In `src-tauri/src/storage.rs`, `save_network_pin` ends with:

```rust
    // Mirror the trim done on load_network_pin — keeps the on-disk file
    // canonical (no trailing whitespace from a pasted Settings input) so
    // even a build without the load-side trim would behave correctly.
    let _ = fs::write(path, pin.trim());
}
```

Change the final write to capture the path and harden it (the current code passes `path` by value into `fs::write`, so reorder to keep `path` usable):

```rust
    // Mirror the trim done on load_network_pin — keeps the on-disk file
    // canonical (no trailing whitespace from a pasted Settings input) so
    // even a build without the load-side trim would behave correctly.
    if fs::write(&path, pin.trim()).is_ok() {
        // The pairing PIN must not be world-readable (issue: secret file perms).
        set_owner_only(&path);
    }
}
```

- [ ] **Step 3: Add `harden_secret_files`**

In `src-tauri/src/storage.rs`, add this public function (place it right after `save_device_cert`):

```rust
/// Re-apply owner-only permissions to the on-disk secret files if they exist.
/// Run once at startup so installs created before this hardening landed get
/// fixed — `device_key.der` in particular is written only at first launch and
/// never rewritten, so the write-path hardening alone would never reach it.
pub fn harden_secret_files(app: &AppHandle) {
    let path_resolver = app.path();
    for name in ["device_key.der", "network_pin"] {
        if let Ok(path) = path_resolver.resolve(name, BaseDirectory::AppConfig) {
            if path.exists() {
                set_owner_only(&path);
            }
        }
    }
}
```

- [ ] **Step 4: Call it during setup**

In `src-tauri/src/app.rs`, find the `wipe_legacy_cluster_key(app_handle);` call (around line 542). Add the harden call immediately after it:

```rust
                wipe_legacy_cluster_key(app_handle);
                // Re-apply owner-only perms to secret files for installs created
                // before this hardening (issue: secret file perms).
                crate::storage::harden_secret_files(app_handle);
```

NOTE: confirm `app_handle` is in scope here and is `&AppHandle` (it is — `wipe_legacy_cluster_key(app_handle)` on the preceding line takes the same). `harden_secret_files` takes `&AppHandle`.

- [ ] **Step 5: Verify it builds and tests pass**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors and no warnings (`set_owner_only` and `harden_secret_files` are now both used).
Run: `cargo test --manifest-path src-tauri/Cargo.toml perms_tests`
Expected: PASS (1 test).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/storage.rs src-tauri/src/app.rs
git commit -m "feat: 0600 perms on device_key.der + network_pin (write + startup)"
```

---

## Task 3: Full build + test sweep

- [ ] **Step 1: Full Rust test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all tests pass (the existing suite plus the new `set_owner_only_sets_0600`).

- [ ] **Step 2: Strict build**

Run: `RUSTFLAGS="-D warnings" cargo build --manifest-path src-tauri/Cargo.toml`
Expected: clean, no warnings.

- [ ] **Step 3: Manual verification (record results)**

- Fresh install: delete `~/.config/app.clustercut.clustercut/`, launch the app, quit, then `ls -l ~/.config/app.clustercut.clustercut/`. Confirm `device_key.der` and `network_pin` are `-rw-------` (0600), and that non-secret files (e.g. `device_cert.der`, `settings.json`) are unaffected.
- Upgrade path: `chmod 644 ~/.config/app.clustercut.clustercut/device_key.der ~/.config/app.clustercut.clustercut/network_pin`, relaunch the app, quit, `ls -l` again. Confirm both are back to `-rw-------` (startup re-hardening works).

---

## Self-Review Notes

- **Spec coverage:** helper with Unix 0600 / Windows no-op → Task 1; harden on write (key + PIN) → Task 2 Steps 1-2; startup harden of existing files → Task 2 Steps 3-4; unit test → Task 1; scope limited to the two secrets (cert/ids/settings untouched) → only those two write sites changed. Windows no-op documented in the helper.
- **Placeholder scan:** none — all code is concrete.
- **Type consistency:** `set_owner_only(&Path)` defined in Task 1, called in Task 2 (`&key_path`, `&path` are `PathBuf`, which deref-coerces to `&Path`); `harden_secret_files(&AppHandle)` defined and called consistently.
