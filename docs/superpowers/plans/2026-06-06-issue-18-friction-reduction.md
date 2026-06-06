# Issue #18 Friction Reduction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two default-on settings toggles (Windows firewall config, mDNS advertising) and make "Add Remote" connect directly to already-paired peers instead of always re-pairing.

**Architecture:** Two new `bool` fields on the Rust `AppSettings` struct (default-true via a serde helper), gated at startup and applied live in the `save_settings` command. A new `add_remote_peer` Tauri command decides connect-vs-pair by matching the typed IP against trusted, fingerprinted known peers; the frontend opens the existing PIN modal only on the `NeedsPairing` outcome.

**Tech Stack:** Rust (Tauri 2, serde, mdns_sd), TypeScript/React frontend.

**Reference spec:** `docs/superpowers/specs/2026-06-06-issue-18-friction-reduction-design.md`

**Test command (Rust):** `cargo test --manifest-path src-tauri/Cargo.toml`
**Typecheck command (frontend):** `npm run build` (runs `tsc` + vite) — or `npx tsc --noEmit -p tsconfig.json` from repo root.

---

## File Structure

- `src-tauri/src/storage.rs` — add `configure_firewall` + `mdns_advertising` fields, `default_true()` helper, defaults, and unit tests.
- `src-tauri/src/discovery.rs` — add `Discovery::unregister()`.
- `src-tauri/src/app.rs` — gate startup firewall config and mDNS `register()` on the new settings.
- `src-tauri/src/commands/settings.rs` — apply both toggles live in `save_settings`.
- `src-tauri/src/commands/peers.rs` — add `peer_already_paired()` helper, `AddRemoteOutcome` enum, `add_remote_peer` command, and unit tests.
- `src-tauri/src/app.rs` (invoke_handler) — register `add_remote_peer`.
- `src/types.ts` — add the two fields to the `AppSettings` interface.
- `src/components/SettingsView.tsx` — add the two toggles (firewall toggle Windows-only).
- `src/App.tsx` — rewrite the single-IP branch of `submitManualPeer` to call `add_remote_peer`.

---

## Task 1: Add the two settings fields with default-true behavior

**Files:**
- Modify: `src-tauri/src/storage.rs:385-441` (struct, helper, Default)
- Test: `src-tauri/src/storage.rs` (new `#[cfg(test)]` module at end of file)

- [ ] **Step 1: Write the failing tests**

Add this module at the very end of `src-tauri/src/storage.rs`:

```rust
#[cfg(test)]
mod settings_tests {
    use super::AppSettings;

    #[test]
    fn missing_new_fields_default_to_true() {
        // A settings.json written before issue #18 has neither field.
        // They must deserialize to `true`, not bool's serde default of false.
        let json = r#"{
            "custom_device_name": null,
            "cluster_mode": "auto",
            "auto_send": true,
            "auto_receive": true,
            "notifications": {"device_join": true, "device_leave": true, "data_sent": false, "data_received": false},
            "shortcut_send": null,
            "shortcut_receive": null,
            "enable_file_transfer": true,
            "max_auto_download_size": 52428800,
            "notify_large_files": true
        }"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert!(s.configure_firewall);
        assert!(s.mdns_advertising);
    }

    #[test]
    fn explicit_false_round_trips() {
        let mut s = AppSettings::default();
        s.configure_firewall = false;
        s.mdns_advertising = false;
        let json = serde_json::to_string(&s).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert!(!back.configure_firewall);
        assert!(!back.mdns_advertising);
    }

    #[test]
    fn defaults_are_true() {
        let s = AppSettings::default();
        assert!(s.configure_firewall);
        assert!(s.mdns_advertising);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml settings_tests`
Expected: FAIL to compile — `no field configure_firewall on type AppSettings`.

- [ ] **Step 3: Add the fields, helper, and defaults**

In `src-tauri/src/storage.rs`, add the two fields to the `AppSettings` struct, immediately after the `pairing_accept_enabled` field (around line 414, before the closing `}`):

```rust
    /// Issue #18: when off, the Windows firewall rule is NOT auto-created at
    /// startup. Default-on for backward compatibility. Windows-only effect.
    #[serde(default = "default_true")]
    pub configure_firewall: bool,
    /// Issue #18: when off, the device does not advertise itself over mDNS
    /// (browsing/discovery of others stays active). Default-on.
    #[serde(default = "default_true")]
    pub mdns_advertising: bool,
```

Add the helper next to `default_pairing_accept_enabled` (around line 417):

```rust
fn default_true() -> bool {
    true
}
```

Add both fields to the `Default::default()` impl, after `pairing_accept_enabled: true,` (around line 438):

```rust
            configure_firewall: true,
            mdns_advertising: true,
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml settings_tests`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage.rs
git commit -m "feat: add configure_firewall + mdns_advertising settings (default on)"
```

---

## Task 2: Add `Discovery::unregister()` for live mDNS toggling

**Files:**
- Modify: `src-tauri/src/discovery.rs:101` (add method after `register`, before `browse`)

This wraps an `mdns_sd` daemon call that can't be meaningfully unit-tested in
isolation, so verification is a compile check.

- [ ] **Step 1: Add the method**

In `src-tauri/src/discovery.rs`, add this method inside `impl Discovery`, right after the `register` method's closing brace (line 101):

```rust
    /// Stop advertising this device without tearing down the daemon or any
    /// active browse. Used to apply the mDNS-advertising toggle live. Safe to
    /// call when nothing is registered.
    pub fn unregister(&mut self) {
        if let Some(fullname) = self.registered_service.take() {
            tracing::info!("Unregistering service (advertising disabled): {}", fullname);
            if let Err(e) = self.daemon.unregister(&fullname) {
                tracing::error!("Failed to unregister service: {}", e);
            }
        }
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds (a `method never used` warning is acceptable until Task 4 wires it in).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/discovery.rs
git commit -m "feat: add Discovery::unregister for live mDNS advertising toggle"
```

---

## Task 3: Gate firewall config and mDNS register at startup

**Files:**
- Modify: `src-tauri/src/app.rs:510-516` (firewall spawn)
- Modify: `src-tauri/src/app.rs:672-678` (mDNS register)

The settings are loaded into `state.settings` earlier in setup (app.rs:606-608),
so they're available before both call sites.

- [ ] **Step 1: Gate the firewall spawn**

Replace the Windows firewall block at `src-tauri/src/app.rs:510-516`:

```rust
            #[cfg(target_os = "windows")]
            {
                // Ensure firewall rule exists; checks first and only prompts UAC if needed.
                std::thread::spawn(|| {
                    crate::net_util::configure_windows_firewall();
                });
            }
```

with:

```rust
            #[cfg(target_os = "windows")]
            {
                // Issue #18: skip auto-config when the user disabled it. Settings
                // are loaded into state below (app.rs ~606), but this block runs
                // earlier in setup, so read straight from disk here.
                if crate::storage::load_settings(app_handle).configure_firewall {
                    // Ensure firewall rule exists; checks first and only prompts UAC if needed.
                    std::thread::spawn(|| {
                        crate::net_util::configure_windows_firewall();
                    });
                }
            }
```

- [ ] **Step 2: Gate the mDNS register**

Replace the discovery registration at `src-tauri/src/app.rs:672-678`:

```rust
                // 4. Register Discovery
                let mut discovery = Discovery::new().expect("Failed to initialize discovery");
                discovery
                    .register(&device_id, &network_name, port)
                    .expect("Failed to register service");
                let receiver = discovery.browse().expect("Failed to browse");
                *state.discovery.lock().unwrap() = Some(discovery);
```

with:

```rust
                // 4. Register Discovery. Browsing always runs so we can still
                // discover peers; advertising (register) is gated on the
                // mdns_advertising setting (issue #18).
                let mut discovery = Discovery::new().expect("Failed to initialize discovery");
                if state.settings.lock().unwrap().mdns_advertising {
                    discovery
                        .register(&device_id, &network_name, port)
                        .expect("Failed to register service");
                } else {
                    tracing::info!("mDNS advertising disabled by settings; browsing only.");
                }
                let receiver = discovery.browse().expect("Failed to browse");
                *state.discovery.lock().unwrap() = Some(discovery);
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/app.rs
git commit -m "feat: gate startup firewall config and mDNS advertising on settings"
```

---

## Task 4: Apply both toggles live in `save_settings`

**Files:**
- Modify: `src-tauri/src/commands/settings.rs:12-44`

`save_settings` is a sync Tauri command with `State` + `AppHandle`. We capture
the previous settings before overwriting, then act on transitions. All inputs
(`discovery`, `local_device_id`, `network_name`, `transport`) live on `AppState`.

- [ ] **Step 1: Rewrite the command**

Replace the body of `save_settings` in `src-tauri/src/commands/settings.rs` (lines 12-44) with:

```rust
#[tauri::command]
pub(crate) fn save_settings(
    mut settings: AppSettings,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    // Capture the previous settings so we can detect toggle transitions.
    let prev = state.settings.lock().unwrap().clone();

    // Preserve backend-only fields that the frontend doesn't manage.
    settings.flatpak_autostart = prev.flatpak_autostart;
    *state.settings.lock().unwrap() = settings.clone();
    tracing::info!(
        "Saving Settings: auto_send={}, auto_receive={}, configure_firewall={}, mdns_advertising={}",
        settings.auto_send, settings.auto_receive, settings.configure_firewall, settings.mdns_advertising
    );
    crate::storage::save_settings(&app_handle, &settings);
    let _ = app_handle.emit("settings-changed", settings.clone());

    // --- Issue #18: apply mDNS advertising toggle live ---
    if settings.mdns_advertising != prev.mdns_advertising {
        let mut disc_lock = state.discovery.lock().unwrap();
        if let Some(disc) = disc_lock.as_mut() {
            if settings.mdns_advertising {
                let device_id = state.local_device_id.lock().unwrap().clone();
                let network_name = state.network_name.lock().unwrap().clone();
                let port = state
                    .transport
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|t| t.local_addr().ok())
                    .map(|a| a.port())
                    .unwrap_or(4654);
                if let Err(e) = disc.register(&device_id, &network_name, port) {
                    tracing::error!("Failed to re-register mDNS service: {}", e);
                }
            } else {
                disc.unregister();
            }
        }
    }

    // --- Issue #18: apply firewall toggle live (Windows OFF->ON only) ---
    #[cfg(target_os = "windows")]
    {
        if settings.configure_firewall && !prev.configure_firewall {
            std::thread::spawn(|| {
                crate::net_util::configure_windows_firewall();
            });
        }
    }

    #[cfg(desktop)]
    crate::tray::update_tray_menu(&app_handle);

    // Update Shortcuts
    crate::shortcuts::register_shortcuts(&app_handle);
}
```

Note: `Transport::local_addr()` returns `Result<SocketAddr, _>` (see
`commands/peers.rs:62`), hence `.ok()` then `.map(|a| a.port())`.

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors. The `unregister` "never used" warning from Task 2 is now gone.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/commands/settings.rs
git commit -m "feat: apply firewall + mDNS toggles live in save_settings"
```

---

## Task 5: Add `add_remote_peer` connect-or-pair command

**Files:**
- Modify: `src-tauri/src/commands/peers.rs` (helper, enum, command, tests)
- Modify: `src-tauri/src/app.rs:1117` (register command in invoke_handler)

- [ ] **Step 1: Write the failing test for the decision helper**

Add this `#[cfg(test)]` module at the end of `src-tauri/src/commands/peers.rs`:

```rust
#[cfg(test)]
mod add_remote_tests {
    use super::peer_already_paired;
    use crate::peer::Peer;
    use std::collections::HashMap;
    use std::net::IpAddr;

    fn peer(ip: &str, is_trusted: bool, fingerprint: Option<Vec<u8>>) -> Peer {
        Peer {
            id: format!("clustercut-{}", ip),
            ip: ip.parse().unwrap(),
            port: 4654,
            hostname: "test".to_string(),
            last_seen: 0,
            is_trusted,
            is_manual: false,
            network_name: None,
            signature: None,
            fingerprint,
            protocol_version: None,
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn matches_trusted_fingerprinted_peer() {
        let mut peers = HashMap::new();
        let p = peer("10.8.0.5", true, Some(vec![1, 2, 3]));
        peers.insert(p.id.clone(), p);
        assert!(peer_already_paired(&peers, ip("10.8.0.5")));
    }

    #[test]
    fn untrusted_peer_is_not_paired() {
        let mut peers = HashMap::new();
        let p = peer("10.8.0.5", false, Some(vec![1, 2, 3]));
        peers.insert(p.id.clone(), p);
        assert!(!peer_already_paired(&peers, ip("10.8.0.5")));
    }

    #[test]
    fn trusted_without_fingerprint_is_not_paired() {
        // Legacy pre-mTLS entry — must re-pair.
        let mut peers = HashMap::new();
        let p = peer("10.8.0.5", true, None);
        peers.insert(p.id.clone(), p);
        assert!(!peer_already_paired(&peers, ip("10.8.0.5")));
    }

    #[test]
    fn unknown_ip_is_not_paired() {
        let mut peers = HashMap::new();
        let p = peer("10.8.0.5", true, Some(vec![1, 2, 3]));
        peers.insert(p.id.clone(), p);
        assert!(!peer_already_paired(&peers, ip("10.8.0.99")));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml add_remote_tests`
Expected: FAIL to compile — `cannot find function peer_already_paired`.

- [ ] **Step 3: Add the helper, enum, and command**

In `src-tauri/src/commands/peers.rs`, add the imports needed at the top (the file already imports `Peer`, `AppState`, `net_util`, `Transport`, `State`). Add `std::collections::HashMap` and `std::net::IpAddr` usage inline (fully-qualified below to avoid touching the import block).

Add the helper function (place it just above the `add_manual_peer` command, around line 75):

```rust
/// True if `ip` belongs to a peer we've already paired with — i.e. a trusted
/// entry that carries a pinned cert fingerprint. Legacy trusted-but-
/// unfingerprinted entries and untrusted manual placeholders return false, so
/// "Add Remote" falls back to the pairing flow for them (issue #18).
pub(crate) fn peer_already_paired(
    peers: &std::collections::HashMap<String, Peer>,
    ip: std::net::IpAddr,
) -> bool {
    peers
        .values()
        .any(|p| p.is_trusted && p.fingerprint.is_some() && p.ip == ip)
}

/// Outcome of an "Add Remote" attempt for a single IP. `Connected` means we
/// recognised an already-paired peer at that address and (re)established the
/// connection; `NeedsPairing` means the frontend should open the PIN modal.
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AddRemoteOutcome {
    Connected,
    NeedsPairing,
}

/// Issue #18: "Add Remote" for a single IP. If we've already paired with the
/// peer at this address, connect directly (no PIN). Otherwise tell the frontend
/// to run the pairing flow. CIDR input still goes through `add_manual_peer`.
#[tauri::command]
pub(crate) async fn add_remote_peer(
    ip: String,
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<AddRemoteOutcome, String> {
    // Parse as IP or IP:PORT (default 4654), matching add_manual_peer's single-IP branch.
    let (addr, port) = if let Ok(sock) = ip.parse::<std::net::SocketAddr>() {
        (sock.ip(), sock.port())
    } else if let Ok(ip_addr) = ip.parse::<std::net::IpAddr>() {
        (ip_addr, 4654)
    } else {
        return Err("Invalid Format. Use IP or IP:PORT.".to_string());
    };

    let already_paired = {
        let peers = state.known_peers.lock().unwrap();
        peer_already_paired(&peers, addr)
    };

    if already_paired {
        net_util::probe_ip(addr, port, (*state).clone(), (*transport).clone(), app_handle).await;
        Ok(AddRemoteOutcome::Connected)
    } else {
        Ok(AddRemoteOutcome::NeedsPairing)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml add_remote_tests`
Expected: PASS (4 tests).

- [ ] **Step 5: Register the command in the invoke_handler**

In `src-tauri/src/app.rs`, add a line after `crate::commands::peers::add_manual_peer,` (line 1117):

```rust
            crate::commands::peers::add_remote_peer,
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/commands/peers.rs src-tauri/src/app.rs
git commit -m "feat: add add_remote_peer command (connect if paired, else pair)"
```

---

## Task 6: Add the two toggles to the frontend settings

**Files:**
- Modify: `src/types.ts:77-91` (interface)
- Modify: `src/components/SettingsView.tsx` (new toggles)

- [ ] **Step 1: Extend the AppSettings interface**

In `src/types.ts`, add two fields to the `AppSettings` interface after `pairing_debug_logs: boolean;` (line 90):

```ts
  configure_firewall: boolean;
  mdns_advertising: boolean;
```

- [ ] **Step 2: Add a Windows-detection constant in SettingsView**

In `src/components/SettingsView.tsx`, add this module-level constant just below the imports (after line 11, before `export function SettingsView`):

```ts
// Issue #18: the firewall toggle only has an effect on Windows, where
// configure_windows_firewall() exists. Match ShortcutRecorder's userAgent check.
const isWindows = navigator.userAgent.toLowerCase().includes("win");
```

- [ ] **Step 3: Add the toggles UI**

In `src/components/SettingsView.tsx`, insert a new Card immediately AFTER the closing `</Card>` of the "File Transfer" section (the `</Card>` at line 451, right before the `{/* Notifications */}` comment at line 453):

```tsx
      {/* Network — issue #18 */}
      <Card className="p-4">
        <SectionHeader
          icon={<Wifi className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Network"
          subtitle="Discovery and connectivity."
        />
        <div className="mt-4 px-1 space-y-4">
          <div className="flex items-center justify-between">
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">mDNS Advertising</div>
              <div className="text-xs text-zinc-500">Let other devices discover this one automatically. Turn off to stay hidden and connect only via Add Remote.</div>
            </div>
            <button
              onClick={() => setSettings({ ...settings, mdns_advertising: !settings.mdns_advertising })}
              className={clsx("relative h-6 w-11 shrink-0 rounded-full transition-colors", settings.mdns_advertising ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
            >
              <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.mdns_advertising ? "translate-x-6" : "translate-x-1")} />
            </button>
          </div>

          {isWindows && (
            <div className="flex items-center justify-between">
              <div>
                <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Configure Windows Firewall</div>
                <div className="text-xs text-zinc-500">Add the inbound/outbound rule on startup (may prompt for admin). Turn off if your firewall is managed externally.</div>
              </div>
              <button
                onClick={() => setSettings({ ...settings, configure_firewall: !settings.configure_firewall })}
                className={clsx("relative h-6 w-11 shrink-0 rounded-full transition-colors", settings.configure_firewall ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
              >
                <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.configure_firewall ? "translate-x-6" : "translate-x-1")} />
              </button>
            </div>
          )}
        </div>
      </Card>
```

(`Wifi` is already imported from `lucide-react` at line 6.)

- [ ] **Step 4: Verify it typechecks**

Run: `npm run build`
Expected: build succeeds (tsc reports no errors). The new fields are present on `AppSettings`, so `settings.mdns_advertising` / `settings.configure_firewall` resolve.

- [ ] **Step 5: Commit**

```bash
git add src/types.ts src/components/SettingsView.tsx
git commit -m "feat: add Network settings toggles (mDNS advertising, Windows firewall)"
```

---

## Task 7: Wire "Add Remote" to connect-or-pair

**Files:**
- Modify: `src/App.tsx:733-757` (`submitManualPeer`)

- [ ] **Step 1: Rewrite the single-IP branch**

In `src/App.tsx`, replace the `else` branch of `submitManualPeer` (the single-IP case at lines 752-756):

```tsx
    } else {
      setAddManualOpen(false);
      setManualIp("");
      startManualPairFlow(input);
    }
```

with:

```tsx
    } else {
      // Single IP: try a direct connect to an already-paired peer first
      // (issue #18). Only fall back to the PIN/pairing modal if we don't
      // recognise this address.
      setManualBusy(true);
      try {
        const outcome = await invoke<"connected" | "needs_pairing">("add_remote_peer", { ip: input });
        setAddManualOpen(false);
        setManualIp("");
        if (outcome === "needs_pairing") {
          startManualPairFlow(input);
        }
      } catch (e) {
        alert("Failed: " + e);
      } finally {
        setManualBusy(false);
      }
    }
```

Note: the serde enum serializes to the snake_case strings `"connected"` /
`"needs_pairing"` (from `#[serde(rename_all = "snake_case")]` in Task 5).

- [ ] **Step 2: Verify it typechecks**

Run: `npm run build`
Expected: build succeeds.

- [ ] **Step 3: Commit**

```bash
git add src/App.tsx
git commit -m "feat: Add Remote connects directly to already-paired peers"
```

---

## Task 8: Full build + test sweep

- [ ] **Step 1: Run the full Rust test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all tests pass (including the new `settings_tests` and `add_remote_tests`).

- [ ] **Step 2: Run the full frontend build**

Run: `npm run build`
Expected: succeeds with no type errors.

- [ ] **Step 3: Manual verification checklist (record results)**

- mDNS toggle: with two instances running, turning OFF "mDNS Advertising" on instance A makes it disappear from instance B's discovery list within a few seconds; turning it back ON makes it reappear — no restart.
- Firewall toggle (Windows): OFF then restart → no UAC prompt / no rule added; OFF→ON in Settings → triggers the firewall config (UAC) immediately.
- Add Remote: typing the IP of an already-paired peer connects with no PIN prompt; typing a brand-new IP opens the PIN modal as before.

- [ ] **Step 4: Final commit (if any manual-test fixups were needed)**

```bash
git add -A
git commit -m "test: issue #18 verification fixups"
```

---

## Self-Review Notes

- **Spec coverage:** Feature 1 (firewall toggle) → Tasks 1,3,4,6. Feature 2 (mDNS toggle) → Tasks 1,2,3,4,6. Feature 3 (connect-or-pair) → Tasks 5,7. Backward-compat default-true → Task 1. Windows-only firewall display → Task 6. Out-of-scope items (no rule removal, browsing stays on, no SPAKE2 change) are respected.
- **Type consistency:** `peer_already_paired` and `AddRemoteOutcome` are defined in Task 5 and consumed in Tasks 5/7; the frontend string union `"connected" | "needs_pairing"` matches the `snake_case` serde rename. `configure_firewall` / `mdns_advertising` names are identical across storage.rs, types.ts, SettingsView.tsx, and App.tsx (via the command).
- **No placeholders:** every code step contains complete code.
