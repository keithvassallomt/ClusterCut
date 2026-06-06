# Secret File Permissions — Design

**Source:** Email from @mdunphy ("Issue 3"), branch `dunphy-mail`
**Date:** 2026-06-06

## Problem

The two secret-bearing files ClusterCut writes to disk are created with bare
`fs::write`, so under the default umask they land world-readable (`0644`) on
Linux/macOS. Any local user can read the device's mTLS private key or the
pairing PIN.

The secrets:
- `device_key.der` — the mTLS private key ([storage.rs:284], written in
  `save_device_cert`).
- `network_pin` — the SPAKE2 pairing secret ([storage.rs:376], written in
  `save_network_pin`).

The codebase currently has **no permission-setting code at all**.

## Decisions

1. **Scope:** only the two real secrets above. The cert (`device_cert.der`),
   identifiers (`device_id`, `cluster_id`, `network_name*`), `known_peers.json`,
   and `settings.json` are not secret material and are left unchanged. The
   config directory is not chmod'd.
2. **Existing files:** harden both on every write **and** once at startup, so
   pre-existing installs are fixed (critical: `device_key.der` is written only
   once at first launch and never rewritten, so a write-only fix would never
   reach an upgrader's key).
3. **Windows:** set `0600` on Unix only. On Windows do nothing extra —
   `%APPDATA%\<app>` is already ACL-restricted to the user, SYSTEM, and
   Administrators by default, so other standard users cannot read it. No Win32
   ACL code (avoids fragile, hard-to-test `SetNamedSecurityInfo` work for
   marginal gain). A code comment documents this.

No new dependencies: `std::os::unix::fs::PermissionsExt` covers Unix.

## Design

### Helper: `set_owner_only`

A private helper in `src-tauri/src/storage.rs`:

```rust
/// Restrict a file to owner-only access. On Unix sets mode 0600. On Windows
/// this is intentionally a no-op: %APPDATA%\<app> is already ACL-restricted to
/// the user, SYSTEM, and Administrators by default, so other standard users
/// cannot read it. Best-effort — logs on failure, never panics.
fn set_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!("Failed to set 0600 on {}: {}", path.display(), e);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path; // Windows: AppData is already per-user ACL-protected.
    }
}
```

### Apply on write

- In `save_device_cert` ([storage.rs:284]): after `fs::write(&key_path, key_der)`
  succeeds, call `set_owner_only(&key_path)`. The cert file is public; leave it.
- In `save_network_pin` ([storage.rs:376]): after `fs::write(path, pin.trim())`,
  call `set_owner_only(&path)`.

### Harden existing files at startup

A `pub fn harden_secret_files(app: &AppHandle)` in `storage.rs` that resolves
`device_key.der` and `network_pin` under `BaseDirectory::AppConfig` and, for
each that exists, calls `set_owner_only`. Called once during setup in
`src-tauri/src/app.rs` (alongside the existing `wipe_legacy_cluster_key` /
device-cert load near the start of state setup). Idempotent and cheap.

This covers upgraders; the write-path calls cover fresh writes and mid-session
PIN regeneration.

## Testing

- **Unit test (`#[cfg(unix)]`)** of `set_owner_only`: write a temp file (under
  `std::env::temp_dir()`, unique name), call the helper, assert
  `metadata.permissions().mode() & 0o777 == 0o600`. The test lives in
  `storage.rs`'s own `#[cfg(test)] mod`, so it can call the private
  `set_owner_only` directly via `super::` — no visibility change needed.
- The `save_*` functions need a Tauri `AppHandle`, so they are not unit-tested;
  the pure helper carries the coverage.
- **Manual (Linux):** delete `~/.config/app.clustercut.clustercut/`, launch the
  app, then `ls -l` the config dir — `device_key.der` and `network_pin` are
  `-rw-------`. Then `chmod 644` them, relaunch, confirm startup re-hardens them
  back to `0600`.

## Out of scope

- Non-secret files and the config directory mode.
- Windows ACL manipulation.
- Encrypting secrets at rest (separate concern).

## File anchors (for the plan)

- `src-tauri/src/storage.rs` — `set_owner_only` helper; calls in
  `save_device_cert` and `save_network_pin`; `harden_secret_files`; unit test.
- `src-tauri/src/app.rs` — call `harden_secret_files` during setup.
