# Peer-Visibility Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the peer list self-healing: network changes (VPN up, Wi-Fi swap), suspend/resume, and missed probes recover automatically, and cluster members that joined while a device was offline become reachable without re-pairing.

**Architecture:** A new `presence` module centralizes peer-liveness logic: a shared burst re-probe (`PeerDiscovery`-based, replacing the presence-inert `Ping` recovery), a 30s anti-entropy loop that re-probes absent known peers and syncs cluster membership via the existing `ClusterInfoRequest`/`ClusterInfo` messages, and pause/refresh helpers that fix the resume prune race. Handlers gain presence refresh on Ping/Pong and a merge branch for unsolicited `ClusterInfo`.

**Tech Stack:** Rust (Tauri 2 backend), tokio via `tauri::async_runtime`, quinn QUIC transport, mdns-sd. No frontend changes.

## Global Constraints

- **No version bumps of any kind**: app version, GNOME extension version, and `CLUSTERCUT_PROTOCOL_VERSION` (stays `"0.3.4"`) are untouched. Fix 2b uses only wire messages that have existed since 0.3.1 (`ClusterInfoRequest`, `ClusterInfo`) — no new `Message` variants, no serde changes.
- **No new crates.** Everything uses existing deps (tokio, serde, tauri).
- **Lock order**: `known_peers` before `peers` (see comment at `app.rs:1223`). Never hold `peers` while acquiring `pending_removals` — drop first.
- **CHANGELOG entries are terse**: 1–3 sentences per bullet.
- **UI behavior unchanged**: offline peers still disappear from the list (user decision — no greyed-out state).
- Repo tests are in-file `#[cfg(test)]` modules; run with `cargo test` from `src-tauri/`.
- All work on branch `fix/peer-visibility-recovery` off `main`.

### Symptom → fix map (context for reviewers)

| Reported symptom | Root cause | Fix (task) |
|---|---|---|
| #1 VPN up → no auto-recovery | netmon recovery sends `Message::Ping`, which neither side records (`handlers.rs:1477`, `:1608`) | Tasks 3, 4 |
| #2 Retry finds some peers; restart finds all | single-shot 2s probes + nothing ever re-probes a missed peer (`net_util.rs:190`, heartbeat only targets runtime peers `app.rs:1157`) | Tasks 3, 8 |
| #2b New member joins while offline → mutual mTLS reject forever | pairing-time gossip goes only to peers online at that instant (`net_util.rs:54-58`), never repeats | Tasks 7, 8 |
| #3 Peers vanish/reappear | one 2s ping decides removal (`app.rs:286-315`); app-level removal invisible to mdns-sd cache so no re-resolve | Tasks 6, 8 |
| #4 Slow/empty list after resume | prune loop wipes stale-`last_seen` peers ~10s after resume, before Wi-Fi is back (`app.rs:1217-1258` has no grace check) | Task 5 |
| #5 (accepted enhancement) | no reconnect kick on window focus | Task 9 |

---

### Task 1: `presence` module — pure liveness helpers

**Files:**
- Create: `src-tauri/src/presence.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod presence;` next to the other module declarations, e.g. after `mod peer;`)
- Test: in-file `#[cfg(test)]` in `src-tauri/src/presence.rs`

**Interfaces:**
- Consumes: `AppState` (`state.rs`), `Peer` (`peer.rs`).
- Produces (later tasks call these exact signatures):
  - `pub(crate) fn presence_paused(state: &AppState) -> bool`
  - `pub(crate) fn refresh_peer_liveness(state: &AppState)`
  - `pub(crate) fn touch_peer_by_addr(state: &AppState, addr: std::net::SocketAddr) -> bool`
  - `pub(crate) fn peers_needing_probe(known: &HashMap<String, Peer>, runtime: &HashMap<String, Peer>) -> Vec<Peer>`

- [ ] **Step 1: Create branch**

```bash
cd /home/keith/LocalCode/keithvassallomt/ClusterCut
git checkout -b fix/peer-visibility-recovery
```

- [ ] **Step 2: Write the failing tests**

Create `src-tauri/src/presence.rs` with module doc, `use` lines, and ONLY the test module first:

```rust
//! Peer-presence convergence: shared re-probe used by startup/retry/netmon
//! recovery, the anti-entropy loop (absent-peer re-probe + membership sync),
//! and helpers that pause presence bookkeeping around suspend/outages.
//!
//! Background: `Message::Ping`/`Pong` are deliberately presence-inert, the 5s
//! heartbeat only targets peers already in the runtime map, and mdns-sd's
//! long-lived browse won't re-emit `ServiceResolved` for records it still
//! caches. So any peer that drops out of the runtime map needs an active
//! `PeerDiscovery` re-probe to come back — that's this module's job.

use crate::peer::Peer;
use crate::state::AppState;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str, ip: &str, port: u16) -> Peer {
        Peer {
            id: id.to_string(),
            ip: ip.parse().unwrap(),
            port,
            hostname: format!("host-{id}"),
            last_seen: 0,
            is_trusted: true,
            is_manual: false,
            network_name: Some("TestNet".to_string()),
            signature: None,
            fingerprint: Some(vec![1, 2, 3]),
            protocol_version: Some("0.3.4".to_string()),
        }
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    // ── presence_paused ────────────────────────────────────────────────

    #[test]
    fn presence_not_paused_by_default() {
        let state = AppState::new();
        assert!(!presence_paused(&state));
    }

    #[test]
    fn presence_paused_when_network_down() {
        let state = AppState::new();
        state.network_available.store(false, Ordering::Relaxed);
        assert!(presence_paused(&state));
    }

    #[test]
    fn presence_paused_when_suspended() {
        let state = AppState::new();
        state.network_suspended.store(true, Ordering::Relaxed);
        assert!(presence_paused(&state));
    }

    #[test]
    fn presence_paused_during_resume_grace() {
        let state = AppState::new();
        *state.resume_grace_until.lock().unwrap() =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
        assert!(presence_paused(&state));
    }

    #[test]
    fn presence_unpaused_after_grace_expires() {
        let state = AppState::new();
        *state.resume_grace_until.lock().unwrap() =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert!(!presence_paused(&state));
    }

    // ── refresh_peer_liveness ──────────────────────────────────────────

    #[test]
    fn refresh_bumps_all_runtime_last_seen() {
        let state = AppState::new();
        state.add_peer(peer("clustercut-a", "192.168.1.10", 4654)); // last_seen: 0
        state.add_peer(peer("clustercut-b", "192.168.1.11", 4654));
        refresh_peer_liveness(&state);
        let peers = state.peers.lock().unwrap();
        for p in peers.values() {
            assert!(now_secs().saturating_sub(p.last_seen) < 5);
        }
    }

    // ── touch_peer_by_addr ─────────────────────────────────────────────

    #[test]
    fn touch_bumps_matching_peer_and_cancels_removal() {
        let state = AppState::new();
        state.add_peer(peer("clustercut-a", "192.168.1.10", 4654));
        state
            .pending_removals
            .lock()
            .unwrap()
            .insert("clustercut-a".to_string(), 42);

        let addr: std::net::SocketAddr = "192.168.1.10:4654".parse().unwrap();
        assert!(touch_peer_by_addr(&state, addr));

        let peers = state.peers.lock().unwrap();
        assert!(now_secs().saturating_sub(peers["clustercut-a"].last_seen) < 5);
        drop(peers);
        assert!(state.pending_removals.lock().unwrap().is_empty());
    }

    #[test]
    fn touch_misses_unknown_addr() {
        let state = AppState::new();
        state.add_peer(peer("clustercut-a", "192.168.1.10", 4654));
        let addr: std::net::SocketAddr = "192.168.1.99:4654".parse().unwrap();
        assert!(!touch_peer_by_addr(&state, addr));
    }

    // ── peers_needing_probe ────────────────────────────────────────────

    #[test]
    fn absent_known_peers_are_selected() {
        let mut known = HashMap::new();
        known.insert("clustercut-a".to_string(), peer("clustercut-a", "192.168.1.10", 4654));
        known.insert("clustercut-b".to_string(), peer("clustercut-b", "192.168.1.11", 4654));
        let mut runtime = HashMap::new();
        runtime.insert("clustercut-a".to_string(), peer("clustercut-a", "192.168.1.10", 4654));

        let need = peers_needing_probe(&known, &runtime);
        assert_eq!(need.len(), 1);
        assert_eq!(need[0].id, "clustercut-b");
    }

    #[test]
    fn known_peer_skipped_when_its_ip_is_already_online() {
        // A `manual-<ip>` placeholder and the real peer can share an IP; if
        // any runtime peer already answers at that IP, don't double-probe it.
        let mut known = HashMap::new();
        known.insert("manual-192.168.1.10".to_string(), {
            let mut p = peer("manual-192.168.1.10", "192.168.1.10", 4654);
            p.fingerprint = None;
            p.is_manual = true;
            p
        });
        let mut runtime = HashMap::new();
        runtime.insert("clustercut-a".to_string(), peer("clustercut-a", "192.168.1.10", 4654));

        assert!(peers_needing_probe(&known, &runtime).is_empty());
    }

    #[test]
    fn all_selected_when_runtime_empty() {
        let mut known = HashMap::new();
        known.insert("clustercut-a".to_string(), peer("clustercut-a", "192.168.1.10", 4654));
        known.insert("clustercut-b".to_string(), peer("clustercut-b", "192.168.1.11", 4654));
        let runtime = HashMap::new();
        assert_eq!(peers_needing_probe(&known, &runtime).len(), 2);
    }
}
```

Add `mod presence;` to `src-tauri/src/lib.rs` next to the other `mod` declarations.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd src-tauri && cargo test presence:: 2>&1 | tail -20`
Expected: COMPILE ERROR — `presence_paused`, `refresh_peer_liveness`, `touch_peer_by_addr`, `peers_needing_probe` not found.

- [ ] **Step 4: Write the implementations**

Insert above the `#[cfg(test)]` module in `presence.rs`:

```rust
/// True while presence bookkeeping should pause: the network is down or
/// suspended, or we are inside the post-resume grace window. While paused,
/// `last_seen` cannot be refreshed (peers are unreachable), so acting on it
/// — pruning, anti-entropy probing — would produce false negatives.
pub(crate) fn presence_paused(state: &AppState) -> bool {
    if !state.network_available.load(Ordering::Relaxed) {
        return true;
    }
    if state.network_suspended.load(Ordering::Relaxed) {
        return true;
    }
    if let Some(end) = *state.resume_grace_until.lock().unwrap() {
        if std::time::Instant::now() < end {
            return true;
        }
    }
    false
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Stamp every runtime peer's `last_seen` to now. Called on resume: time
/// spent asleep must not count as peer absence, or the 300s prune wipes the
/// whole list ~10s after wake — before Wi-Fi is even re-associated.
pub(crate) fn refresh_peer_liveness(state: &AppState) {
    let now = now_unix_secs();
    let mut peers = state.peers.lock().unwrap();
    for p in peers.values_mut() {
        p.last_seen = now;
    }
}

/// Refresh `last_seen` (and cancel any pending debounced removal) for the
/// runtime peer at `addr`. Returns false if no runtime peer matches. The
/// transport multiplexes client+server on one UDP socket, so a sender's
/// source port IS its listening port — matching ip+port is exact.
pub(crate) fn touch_peer_by_addr(state: &AppState, addr: std::net::SocketAddr) -> bool {
    let now = now_unix_secs();
    let touched_id = {
        let mut peers = state.peers.lock().unwrap();
        match peers
            .values_mut()
            .find(|p| p.ip == addr.ip() && p.port == addr.port())
        {
            Some(p) => {
                p.last_seen = now;
                Some(p.id.clone())
            }
            None => None,
        }
    };
    match touched_id {
        Some(id) => {
            state.pending_removals.lock().unwrap().remove(&id);
            true
        }
        None => false,
    }
}

/// Known peers worth an active probe: not in the runtime map by id, and not
/// reachable at an IP some runtime peer already answers on (a `manual-<ip>`
/// placeholder and its real peer share an IP).
pub(crate) fn peers_needing_probe(
    known: &HashMap<String, Peer>,
    runtime: &HashMap<String, Peer>,
) -> Vec<Peer> {
    let online_ips: std::collections::HashSet<std::net::IpAddr> =
        runtime.values().map(|p| p.ip).collect();
    known
        .values()
        .filter(|p| !runtime.contains_key(&p.id))
        .filter(|p| !online_ips.contains(&p.ip))
        .cloned()
        .collect()
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd src-tauri && cargo test presence:: 2>&1 | tail -10`
Expected: `test result: ok. 10 passed`

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/presence.rs src-tauri/src/lib.rs
git commit -m "feat(presence): liveness helpers (pause window, resume refresh, addr touch, probe targeting)"
```

---

### Task 2: `probe_ip` returns success and gains a notify switch

**Files:**
- Modify: `src-tauri/src/net_util.rs:136-257` (`probe_ip`)
- Modify: `src-tauri/src/commands/peers.rs:123` and `:177` (add `, true` arg)
- Modify: `src-tauri/src/app.rs:803` (add `, true` arg — replaced entirely in Task 3, but must compile now)

**Interfaces:**
- Produces: `pub(crate) async fn probe_ip(ip: std::net::IpAddr, port: u16, state: AppState, transport: Transport, app_handle: tauri::AppHandle, notify: bool) -> bool` — `true` iff the QUIC send succeeded. `notify: false` suppresses all probe notifications (needed by the 30s anti-entropy loop, which would otherwise toast "Connection Failed" for every offline peer forever).

- [ ] **Step 1: Change the signature and returns**

In `net_util.rs`, change the signature:

```rust
pub(crate) async fn probe_ip(
    ip: std::net::IpAddr,
    port: u16,
    state: AppState,
    transport: Transport,
    app_handle: tauri::AppHandle,
    notify: bool,
) -> bool {
```

Inside the function body make exactly these changes:
1. Every `if state.should_notify() {` that wraps a `send_notification` call becomes `if notify && state.should_notify() {` (there are four: success at :195, already-exists "Connection Verified" at :239, send-error at :246, timeout at :252).
2. The `Ok(Ok(()))` arm ends with `true` as its value; the `Ok(Err(e))` and `Err(_)` arms end with `false`. Make the `match` the function's tail expression:

```rust
            match tokio::time::timeout(std::time::Duration::from_millis(2000), send_future).await {
                Ok(Ok(())) => {
                    // ... existing success body unchanged (placeholder insert, emit, notifications) ...
                    true
                },
                Ok(Err(e)) => {
                    tracing::warn!("Probe to {} FAILED (Send Error): {}", addr, e);
                    if notify && state.should_notify() {
                        crate::send_notification(&app_handle, "Connection Failed", &format!("Failed to send packet to {}: {}", ip, e), true, None, "devices", crate::NotificationPayload::None);
                    }
                    false
                },
                Err(_) => {
                    tracing::warn!("Probe to {} FAILED (Timeout)", addr);
                    if notify && state.should_notify() {
                        crate::send_notification(&app_handle, "Connection Failed", &format!("Connection to {} timed out. Check firewall/VPN.", ip), true, None, "devices", crate::NotificationPayload::None);
                    }
                    false
                }
            }
}
```

3. Update the three existing callers to pass `true` (user-visible behavior unchanged):
   - `commands/peers.rs:123` (`add_manual_peer`): `net_util::probe_ip(addr, port, (*state).clone(), (*transport).clone(), app_handle, true).await;`
   - `commands/peers.rs:177` (`add_remote_peer`): same `, true`.
   - `app.rs:803` (startup probe): `crate::net_util::probe_ip(peer.ip, peer.port, s, t, a, true).await;`
   - `commands/peers.rs:375` (`retry_connection`): same `, true` (this whole body is replaced in Task 3; keep it compiling now).

- [ ] **Step 2: Build and run the full suite**

Run: `cd src-tauri && cargo test 2>&1 | tail -5`
Expected: all existing tests pass (nothing asserts on probe_ip's return yet — this task is behavior-preserving).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/net_util.rs src-tauri/src/commands/peers.rs src-tauri/src/app.rs
git commit -m "refactor(net): probe_ip reports success and can run silently"
```

---

### Task 3: shared burst re-probe; wire into startup, retry, and netmon recovery

**Files:**
- Modify: `src-tauri/src/presence.rs` (add `reprobe_known_peers`)
- Modify: `src-tauri/src/app.rs:788-807` (startup probe uses it)
- Modify: `src-tauri/src/commands/peers.rs:350-386` (`retry_connection` uses it)
- Modify: `src-tauri/src/netmon.rs:112-136` (replace inert `Ping` re-probe)

**Interfaces:**
- Consumes: `probe_ip(..., notify) -> bool` (Task 2), `peers_needing_probe` (Task 1).
- Produces: `pub(crate) fn reprobe_known_peers(state: AppState, transport: Transport, app_handle: tauri::AppHandle, notify: bool, attempts: u32)` — fire-and-forget; spawns one task per absent known peer, each making up to `attempts` probes spaced 2s, 4s. Later tasks (8, 9) call this.

- [ ] **Step 1: Implement `reprobe_known_peers`** (no pure unit to TDD here — it composes tested pieces around spawned I/O; the compile + behavior-preserving suite is the check)

Add to `presence.rs` (above the tests):

```rust
use crate::transport::Transport;

/// Burst re-probe of every known peer that is absent from the runtime map.
///
/// Sends the `PeerDiscovery` probe (`net_util::probe_ip`) — NOT `Ping` —
/// because only `PeerDiscovery` makes the far side record us and heartbeat
/// back, which is what actually repopulates both peer lists. Each absent
/// peer gets up to `attempts` tries (2s, then 4s apart): right after a VPN
/// or resume the first packets often race route/ARP/firewall setup.
///
/// `notify: true` surfaces the final attempt's outcome as notifications
/// (user-initiated retry); earlier attempts are always silent.
pub(crate) fn reprobe_known_peers(
    state: AppState,
    transport: Transport,
    app_handle: tauri::AppHandle,
    notify: bool,
    attempts: u32,
) {
    let targets = {
        // Lock order: known_peers before peers (matches prune/reset).
        let kp = state.known_peers.lock().unwrap();
        let rt = state.peers.lock().unwrap();
        peers_needing_probe(&kp, &rt)
    };
    if targets.is_empty() {
        return;
    }
    tracing::info!(
        "[Presence] Re-probing {} absent known peer(s) ({} attempt(s) each)",
        targets.len(),
        attempts.max(1)
    );
    for peer in targets {
        let s = state.clone();
        let t = transport.clone();
        let a = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let attempts = attempts.max(1);
            for attempt in 0..attempts {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                    // The peer may have surfaced meanwhile (e.g. its own
                    // heartbeat reached us) — stop burning probes on it.
                    let surfaced = s.peers.lock().unwrap().values().any(|p| p.ip == peer.ip);
                    if surfaced {
                        return;
                    }
                }
                let last = attempt + 1 == attempts;
                if crate::net_util::probe_ip(peer.ip, peer.port, s.clone(), t.clone(), a.clone(), notify && last).await {
                    return;
                }
            }
        });
    }
}
```

- [ ] **Step 2: Wire the startup probe** (`app.rs`)

Replace the block from `// Clone to vector for iteration (drop lock)` (app.rs:788) through the end of the `if !peers_to_probe.is_empty() { ... }` block (app.rs:806) with:

```rust
                     drop(known_peers);

                     crate::presence::reprobe_known_peers(
                         state_owned.clone(),
                         transport_clone.clone(),
                         app_handle_clone.clone(),
                         true,
                         3,
                     );
```

(The `is_manual` auto-correct pass above it stays untouched.)

- [ ] **Step 3: Wire `retry_connection`** (`commands/peers.rs:350-386`)

Replace the whole function body with:

```rust
#[tauri::command]
pub(crate) async fn retry_connection(
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    tracing::info!("Retry Connection: probing absent known peers...");
    crate::presence::reprobe_known_peers(
        (*state).clone(),
        (*transport).clone(),
        app_handle,
        true,
        3,
    );
    Ok(())
}
```

- [ ] **Step 4: Fix netmon recovery** (`netmon.rs:112-136`)

Replace the entire `// Re-probe all known peers` block inside `start_recovery_tasks` with:

```rust
        // Re-probe absent known peers with PeerDiscovery bursts. The old code
        // sent bare `Message::Ping`s here — but Ping/Pong are presence-inert
        // on both sides, so recovery "succeeded" without ever repopulating
        // the peer list (the VPN-reconnect bug).
        {
            let transport_opt = state.transport.lock().unwrap().clone();
            if let Some(transport) = transport_opt {
                crate::presence::reprobe_known_peers(
                    state.clone(),
                    transport,
                    handle.clone(),
                    false,
                    3,
                );
            }
        }
```

Also in `start_recovery_tasks`, rename `let _handle = app_handle.clone();` (netmon.rs:90) to `let handle = app_handle.clone();` — it's used now. Remove the now-unused `use crate::peer::Peer;` and `use crate::protocol::Message;` imports at the top of `netmon.rs` if nothing else in the file uses them (`Message` is no longer used after this change; check `Peer` too).

- [ ] **Step 5: Build and test**

Run: `cd src-tauri && cargo test 2>&1 | tail -5`
Expected: all pass, no warnings about unused imports in netmon.rs.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/presence.rs src-tauri/src/app.rs src-tauri/src/commands/peers.rs src-tauri/src/netmon.rs
git commit -m "fix(presence): recovery/startup/retry share a PeerDiscovery burst re-probe

netmon recovery previously pinged peers with a presence-inert message, so
VPN/network-change recovery never repopulated the peer list; startup and
retry probed each peer exactly once with a 2s timeout."
```

---

### Task 4: Ping/Pong refresh presence

**Files:**
- Modify: `src-tauri/src/handlers.rs:1477-1482` (`Message::Ping` arm) and `:1608-1626` (`Message::Pong` arm)

**Interfaces:**
- Consumes: `presence::touch_peer_by_addr` (Task 1).

- [ ] **Step 1: Update both arms**

`Message::Ping` arm becomes:

```rust
        Message::Ping => {
            tracing::debug!("Received Ping from {}. Sending Pong.", addr);
            // An authenticated Ping proves the sender is alive — refresh its
            // runtime entry so debounced removals/pruning don't fire on a
            // peer that is actively probing us.
            crate::presence::touch_peer_by_addr(&listener_state, addr);
            if let Ok(pong_data) = serde_json::to_vec(&Message::Pong) {
                let _ = transport_inside.send_message(addr, &pong_data).await;
            }
        }
```

`Message::Pong` arm: insert one line after the `tracing::debug!` line:

```rust
        Message::Pong => {
             tracing::debug!("Received Pong from {}. Connection Verified.", addr);
             crate::presence::touch_peer_by_addr(&listener_state, addr);
             // ... existing deferred-join notification block unchanged ...
```

- [ ] **Step 2: Build and test**

Run: `cd src-tauri && cargo test 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/handlers.rs
git commit -m "fix(presence): Ping/Pong refresh the sender's runtime liveness"
```

---

### Task 5: resume prune race — grace-aware pruning + liveness reset on resume

**Files:**
- Modify: `src-tauri/src/netmon.rs:36-69` (`resume_recovery`)
- Modify: `src-tauri/src/app.rs:1217-1258` (prune loop)

**Interfaces:**
- Consumes: `presence::refresh_peer_liveness`, `presence::presence_paused` (Task 1).

- [ ] **Step 1: Reset liveness in `resume_recovery`**

In `netmon.rs`, inside `resume_recovery`, insert after the grace-period block (after the `tracing::info!("[Netmon] Grace period set...")` line):

```rust
    // Sleep/outage time must not count as peer absence: the prune loop
    // compares wall-clock last_seen, so after a >5min suspend every peer
    // looks stale and gets wiped ~10s after wake — usually before Wi-Fi is
    // even up. Stamp them now; heartbeats/re-probes re-verify from here.
    crate::presence::refresh_peer_liveness(state);
```

- [ ] **Step 2: Pause the prune loop during outages/grace**

In `app.rs`, in the prune task, insert directly after `tokio::time::sleep(std::time::Duration::from_secs(10)).await;` (app.rs:1219):

```rust
                    // last_seen can't refresh while we're offline/suspended or
                    // in the post-resume grace window — pruning then would
                    // remove peers for our outage, not theirs.
                    if crate::presence::presence_paused(&prune_state) {
                        continue;
                    }
```

- [ ] **Step 3: Build and test**

Run: `cd src-tauri && cargo test 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/netmon.rs src-tauri/src/app.rs
git commit -m "fix(presence): resume no longer races the prune loop

Reset runtime last_seen on resume and skip prune passes while the network
is down/suspended or within the resume grace window."
```

---

### Task 6: removal debounce probes 3× before removing

**Files:**
- Modify: `src-tauri/src/app.rs:291-315` (probe section of `removal_debounce_task`)

- [ ] **Step 1: Replace the single-attempt probe**

Replace the `let mut is_alive = false; if let Some(addr) = peer_addr { ... }` block (app.rs:291-315) with:

```rust
    let mut is_alive = false;

    if let Some(addr) = peer_addr {
        tracing::info!("[Discovery] Debounce expired for {}. Probing...", peer_id);
        let transport_opt = { state.transport.lock().unwrap().clone() };

        if let Some(transport) = transport_opt {
            if let Ok(ping_data) = serde_json::to_vec(&Message::Ping) {
                // One 2s attempt was deciding removal — a single lost
                // handshake under Wi-Fi power-save removed a live peer.
                const PROBE_ATTEMPTS: u32 = 3;
                for attempt in 1..=PROBE_ATTEMPTS {
                    let send_fut = async {
                        match transport.send_message(addr, &ping_data).await {
                            Ok(_) => true,
                            Err(e) => {
                                tracing::warn!("[Discovery] Active probe to {} failed (attempt {}/{}): {}", addr, attempt, PROBE_ATTEMPTS, e);
                                false
                            }
                        }
                    };
                    match tokio::time::timeout(std::time::Duration::from_secs(2), send_fut).await {
                        Ok(true) => {
                            is_alive = true;
                            break;
                        }
                        Ok(false) => {}
                        Err(_) => {
                            tracing::warn!("[Discovery] Active probe to {} timed out (attempt {}/{}).", addr, attempt, PROBE_ATTEMPTS);
                        }
                    }
                    if attempt < PROBE_ATTEMPTS {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            }
        }
    }
```

- [ ] **Step 2: Build and test**

Run: `cd src-tauri && cargo test 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/app.rs
git commit -m "fix(discovery): removal probe retries 3x before dropping a peer"
```

---

### Task 7: membership merge from unsolicited `ClusterInfo` (fix 2b, receive side)

**Files:**
- Modify: `src-tauri/src/presence.rs` (add `merge_cluster_membership` + tests)
- Modify: `src-tauri/src/handlers.rs:1525-1539` (`Message::ClusterInfo` arm)

**Interfaces:**
- Consumes: `crate::protocol::ClusterInfo` (fields: `cluster_id: String`, `known_peers: Vec<Peer>`, plus name/register fields this fix must NOT touch).
- Produces: `pub(crate) fn merge_cluster_membership(state: &AppState, info: &crate::protocol::ClusterInfo) -> Vec<Peer>` — inserts only *new, fingerprinted, non-self, non-placeholder* members into `known_peers`; returns what it imported. Caller persists/probes.

**Security note for the implementer:** an unsolicited `ClusterInfo` can only arrive over an mTLS connection whose client cert matched a pinned fingerprint (`state.knows_fingerprint`, wired in `app.rs:1015-1020`) — i.e. from a paired cluster member. Importing its membership (with fingerprints) is the same transitive-trust model the existing `PeerDiscovery` gossip handler already applies (`handlers.rs:1099-1104`). The cluster-id guard below adds cross-cluster hygiene on top.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `presence.rs`:

```rust
    // ── merge_cluster_membership ───────────────────────────────────────

    fn cluster_info(peers: Vec<Peer>) -> crate::protocol::ClusterInfo {
        crate::protocol::ClusterInfo {
            cluster_id: "cluster-uuid-1".to_string(),
            known_peers: peers,
            network_name: "TestNet".to_string(),
            network_name_version: 0,
            network_name_origin: String::new(),
            cluster_mode: "auto".to_string(),
        }
    }

    #[test]
    fn merge_imports_new_fingerprinted_member() {
        let state = AppState::new();
        *state.local_device_id.lock().unwrap() = "clustercut-me".to_string();
        let info = cluster_info(vec![peer("clustercut-new", "192.168.1.20", 4654)]);

        let imported = merge_cluster_membership(&state, &info);

        assert_eq!(imported.len(), 1);
        let kp = state.known_peers.lock().unwrap();
        let entry = &kp["clustercut-new"];
        assert!(entry.is_trusted);
        assert!(!entry.is_manual);
        assert_eq!(entry.fingerprint, Some(vec![1, 2, 3]));
        // Membership import is bookkeeping, not presence: runtime untouched.
        drop(kp);
        assert!(state.peers.lock().unwrap().is_empty());
    }

    #[test]
    fn merge_skips_self_existing_placeholder_and_unfingerprinted() {
        let state = AppState::new();
        *state.local_device_id.lock().unwrap() = "clustercut-me".to_string();
        state
            .known_peers
            .lock()
            .unwrap()
            .insert("clustercut-known".to_string(), peer("clustercut-known", "192.168.1.30", 4654));

        let me = peer("clustercut-me", "10.8.0.5", 4654);
        let existing = peer("clustercut-known", "192.168.1.99", 4654); // different IP than ours
        let placeholder = {
            let mut p = peer("manual-192.168.1.40", "192.168.1.40", 4654);
            p.is_manual = true;
            p
        };
        let unfingerprinted = {
            let mut p = peer("clustercut-legacy", "192.168.1.50", 4654);
            p.fingerprint = None;
            p
        };
        let info = cluster_info(vec![me, existing, placeholder, unfingerprinted]);

        let imported = merge_cluster_membership(&state, &info);

        assert!(imported.is_empty());
        let kp = state.known_peers.lock().unwrap();
        assert_eq!(kp.len(), 1);
        // Existing entry not clobbered by the sender's differing view.
        assert_eq!(kp["clustercut-known"].ip.to_string(), "192.168.1.30");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test presence:: 2>&1 | tail -10`
Expected: COMPILE ERROR — `merge_cluster_membership` not found.

- [ ] **Step 3: Implement**

Add to `presence.rs` (above tests):

```rust
/// Merge cluster membership from an authenticated `ClusterInfo` snapshot.
///
/// Closes the "joined while I was away" hole: pairing-time gossip goes only
/// to peers online at that instant and never repeats, so a member that
/// paired in while we were offline is mutually unreachable forever (neither
/// side has the other's fingerprint pinned) until a manual re-pair.
///
/// Conservative by design — inserts only entries that are new to us,
/// fingerprinted, not ourselves, and not `manual-<ip>` placeholders (those
/// are the sender's local reachability hints, not members). Existing entries
/// are never overwritten: direct contact refreshes those. Runtime presence
/// is untouched; the caller probes imports to surface them. Cluster
/// name/id/PIN adoption is pairing-only and does NOT happen here.
///
/// Returns the imported peers. Caller persists `known_peers` if non-empty.
pub(crate) fn merge_cluster_membership(
    state: &AppState,
    info: &crate::protocol::ClusterInfo,
) -> Vec<Peer> {
    let local_id = state.local_device_id.lock().unwrap().clone();
    let mut kp = state.known_peers.lock().unwrap();
    let mut imported = Vec::new();
    for peer in &info.known_peers {
        if peer.id == local_id {
            continue;
        }
        if peer.id.starts_with("manual-") {
            continue;
        }
        if peer.fingerprint.is_none() {
            // Unusable under strict mTLS, and importing it would trip the
            // "needs re-pair" banner for a device we never actually paired.
            continue;
        }
        if kp.contains_key(&peer.id) {
            continue;
        }
        let mut p = peer.clone();
        p.is_trusted = true; // same transitive trust as PeerDiscovery gossip
        p.is_manual = false;
        kp.insert(p.id.clone(), p.clone());
        imported.push(p);
    }
    imported
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test presence:: 2>&1 | tail -10`
Expected: `test result: ok. 12 passed`

- [ ] **Step 5: Wire the handler**

In `handlers.rs`, replace the `None => { ... }` arm of the `Message::ClusterInfo(info)` waiter match (handlers.rs:1535-1537) with:

```rust
                None => {
                    // Unsolicited ClusterInfo = reply to an anti-entropy
                    // membership-sync request (the sender passed mTLS, so it
                    // is a paired member). Merge members we're missing.
                    let local_cluster = listener_state.cluster_id.lock().unwrap().clone();
                    if local_cluster.is_empty() || info.cluster_id != local_cluster {
                        tracing::warn!(
                            "Ignoring ClusterInfo from {} for foreign/unset cluster ({})",
                            addr,
                            info.cluster_id
                        );
                        return;
                    }
                    let imported = crate::presence::merge_cluster_membership(&listener_state, &info);
                    if imported.is_empty() {
                        return;
                    }
                    {
                        let kp = listener_state.known_peers.lock().unwrap();
                        storage::save_known_peers(listener_handle.app_handle(), &kp);
                    }
                    for peer in imported {
                        tracing::info!(
                            "[Presence] Membership sync: learned {} ({}) from {}",
                            peer.hostname, peer.id, addr
                        );
                        let s = listener_state.clone();
                        let t = transport_inside.clone();
                        let a = listener_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = crate::net_util::probe_ip(peer.ip, peer.port, s, t, a, false).await;
                        });
                    }
                }
```

(`use tauri::Manager;` is already in scope in handlers.rs for `app_handle()`; verify, and check `storage::save_known_peers` is already imported — it's used at handlers.rs:1124.)

- [ ] **Step 6: Build and full test**

Run: `cd src-tauri && cargo test 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/presence.rs src-tauri/src/handlers.rs
git commit -m "feat(presence): merge membership from unsolicited ClusterInfo

Members that pair in while a device is offline are now importable later:
guarded by cluster-id match, insert-only-new, fingerprinted entries only."
```

---

### Task 8: anti-entropy loop (periodic re-probe + membership sync request)

**Files:**
- Modify: `src-tauri/src/presence.rs` (add `spawn_anti_entropy_loop`)
- Modify: `src-tauri/src/app.rs` (spawn it in setup, right after the pruning task block that ends at app.rs:1258)

**Interfaces:**
- Consumes: `reprobe_known_peers` (Task 3), `presence_paused` (Task 1), `Message::ClusterInfoRequest` (existing), `state.pending_cluster_info` (existing).
- Produces: `pub(crate) fn spawn_anti_entropy_loop(app_handle: tauri::AppHandle)`.

- [ ] **Step 1: Implement the loop**

Add to `presence.rs`:

```rust
/// Anti-entropy cadence. 30s bounds how long a missed peer stays invisible
/// (the old behavior was "forever, until app restart"). Traffic cost per
/// tick: one 2s QUIC dial per absent peer + one tiny ClusterInfoRequest.
const ANTI_ENTROPY_PERIOD_SECS: u64 = 30;

/// Periodic self-healing for the peer list. Every tick (while presence is
/// not paused): re-probe absent known peers (single silent attempt — the
/// tick period is the retry cadence), then ask one online trusted member
/// for its known_peers so membership converges (see
/// `merge_cluster_membership` for why).
pub(crate) fn spawn_anti_entropy_loop(app_handle: tauri::AppHandle) {
    use tauri::Manager;
    let state: AppState = (*app_handle.state::<AppState>()).clone();
    tauri::async_runtime::spawn(async move {
        let mut tick: u64 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(ANTI_ENTROPY_PERIOD_SECS)).await;
            tick = tick.wrapping_add(1);
            if presence_paused(&state) {
                continue;
            }
            let transport_opt = state.transport.lock().unwrap().clone();
            let Some(transport) = transport_opt else { continue };

            reprobe_known_peers(state.clone(), transport.clone(), app_handle.clone(), false, 1);

            // Membership sync — skip while a pairing is waiting on
            // ClusterInfo, so we can't swallow its T7 reply.
            if state.pending_cluster_info.lock().unwrap().is_some() {
                continue;
            }
            let mut online: Vec<Peer> = state
                .peers
                .lock()
                .unwrap()
                .values()
                .filter(|p| p.is_trusted && !p.id.starts_with("manual-"))
                .cloned()
                .collect();
            if online.is_empty() {
                continue;
            }
            online.sort_by(|a, b| a.id.cmp(&b.id));
            let target = &online[(tick as usize) % online.len()];
            let addr = std::net::SocketAddr::new(target.ip, target.port);
            if let Ok(bytes) = serde_json::to_vec(&crate::protocol::Message::ClusterInfoRequest) {
                let t = transport.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        t.send_message(addr, &bytes),
                    )
                    .await;
                });
            }
        }
    });
}
```

- [ ] **Step 2: Spawn it in setup**

In `app.rs`, directly after the pruning task's closing `});` (app.rs:1258), add:

```rust
            // Background Task: Anti-Entropy (self-healing peer list +
            // membership sync — see presence.rs)
            crate::presence::spawn_anti_entropy_loop(app.handle().clone());
```

- [ ] **Step 3: Build and full test**

Run: `cd src-tauri && cargo test 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/presence.rs src-tauri/src/app.rs
git commit -m "feat(presence): 30s anti-entropy loop re-probes absent peers and syncs membership"
```

---

### Task 9: focus-triggered silent re-probe (fix 5)

**Files:**
- Modify: `src-tauri/src/state.rs` (new field `last_focus_reprobe`)
- Modify: `src-tauri/src/app.rs` (`RunEvent::WindowEvent { event: WindowEvent::Focused(true) }` arm, app.rs:1324+)

**Interfaces:**
- Consumes: `reprobe_known_peers` (Task 3).
- Produces: `AppState.last_focus_reprobe: Arc<Mutex<Option<std::time::Instant>>>`.

- [ ] **Step 1: Add the debounce field**

In `state.rs`, add to the `AppState` struct after `last_known_local_ip` (state.rs:122):

```rust
    /// Debounce stamp for the focus-triggered silent re-probe (app.rs run
    /// loop): at most one kick per 30s regardless of focus churn.
    pub last_focus_reprobe: Arc<Mutex<Option<std::time::Instant>>>,
```

and to `AppState::new()` after the `last_known_local_ip` line (state.rs:198):

```rust
            last_focus_reprobe: Arc::new(Mutex::new(None)),
```

- [ ] **Step 2: Kick a silent re-probe on focus**

In `app.rs`, inside the `tauri::RunEvent::WindowEvent { event: tauri::WindowEvent::Focused(true), .. }` arm (app.rs:1324), after the existing badge-clearing blocks, add:

```rust
                // The user is looking at the window — cheap moment to heal
                // the peer list (e.g. VPN connected while we were showing
                // a stale/empty list). Silent, debounced, and a no-op when
                // no known peer is absent.
                if let Some(state) = app_handle.try_state::<AppState>() {
                    let due = {
                        let mut last = state.last_focus_reprobe.lock().unwrap();
                        let now = std::time::Instant::now();
                        match *last {
                            Some(prev) if now.duration_since(prev) < std::time::Duration::from_secs(30) => false,
                            _ => {
                                *last = Some(now);
                                true
                            }
                        }
                    };
                    if due {
                        let transport_opt = state.transport.lock().unwrap().clone();
                        if let Some(transport) = transport_opt {
                            crate::presence::reprobe_known_peers(
                                (*state).clone(),
                                transport,
                                app_handle.clone(),
                                false,
                                1,
                            );
                        }
                    }
                }
```

- [ ] **Step 3: Build and full test**

Run: `cd src-tauri && cargo test 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/state.rs src-tauri/src/app.rs
git commit -m "feat(presence): silent debounced re-probe when the window gains focus"
```

---

### Task 10: changelog + final verification

**Files:**
- Modify: `CHANGELOG.md` (read its top first; add entries under an `## [Unreleased]` heading above the `0.4.0` section, creating the heading if absent — do NOT create a version number)

- [ ] **Step 1: Changelog entries** (terse — house style)

```markdown
## [Unreleased]

### Fixed
- Peer list now self-heals: network changes (e.g. VPN up), suspend/resume, and missed probes recover automatically instead of requiring Retry or an app restart.
- Resume no longer briefly wipes the peer list; peer removal now requires 3 failed probes instead of 1.

### Added
- Membership sync: devices that joined the cluster while you were offline become reachable without re-pairing.
```

- [ ] **Step 2: Full suite + release build check**

Run: `cd src-tauri && cargo test 2>&1 | tail -5 && cargo build 2>&1 | tail -3`
Expected: all tests pass; build succeeds with no new warnings.

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: changelog for peer-visibility recovery fixes"
```

- [ ] **Step 4: Manual verification notes (device testing is the user's step)**

Scenarios to verify on real hardware (mirrors the report):
1. Office/VPN: start app before VPN, connect VPN → peers should appear within ~30s with no clicks (netmon recovery + anti-entropy).
2. Retry button right after VPN-up → all peers within ~15s (3-attempt bursts).
3. Suspend >5min, resume → list intact (no wipe), refreshed within ~1min.
4. Pair a new device at home while the laptop is off-LAN → laptop learns it within ~30s of reconnecting to any member (membership sync log line), and can then exchange clipboard with it.

---

## Self-Review (completed)

- **Spec coverage:** fix 1 → Tasks 2-4; fix 2 → Tasks 3, 8; fix 2b → Tasks 7, 8; fix 3 → Task 5; fix 4 → Task 6; fix 5 → Task 9. UI unchanged (user decision). ✓
- **Placeholder scan:** every code step contains complete code; the one "existing body unchanged" reference (Task 2, probe_ip success arm; Task 4, Pong arm) points at code that must not change, with the exact insertion shown. ✓
- **Type consistency:** `probe_ip(..., notify: bool) -> bool` (Task 2) matches calls in Tasks 3, 7; `reprobe_known_peers(state, transport, app_handle, notify, attempts)` consistent across Tasks 3, 8, 9; `presence_paused`/`refresh_peer_liveness`/`touch_peer_by_addr`/`merge_cluster_membership` signatures match between definition and call sites. Lock order (`known_peers` → `peers`) respected in `reprobe_known_peers` and `merge_cluster_membership` (kp only). ✓
