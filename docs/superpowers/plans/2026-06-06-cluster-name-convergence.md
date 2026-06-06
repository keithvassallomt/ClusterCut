# Cluster Name Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the cluster name a single shared, convergent property across all peers, instead of a per-device label that silently diverges on rename/mode-switch.

**Architecture:** The name becomes a versioned register `(name, version, origin)` replicated with last-write-wins by Lamport-style version (ties broken by `origin` device_id). Renames bump the version and broadcast a new authenticated `Message::ClusterName` over QUIC/mTLS; peers converge on receipt, on (re)connect (anti-entropy via the PeerDiscovery path), and at pairing (extended `ClusterInfo`). Older peers are gated out of the new message and keep current behavior (no pairing-floor change).

**Tech Stack:** Rust (Tauri 2, serde, mdns_sd, quinn), TypeScript/React frontend.

**Reference spec:** `docs/superpowers/specs/2026-06-06-cluster-name-convergence-design.md`

**Test command (Rust):** `cargo test --manifest-path src-tauri/Cargo.toml`
**Build command (Rust):** `cargo build --manifest-path src-tauri/Cargo.toml`
**Build command (frontend):** `npm run build`

---

## Conventions used below

- The versioned register is the triple `(name: String, version: u64, origin: String)` where `origin` is the `device_id` of whoever last set the name.
- "Connected trusted peers" = entries from `state.get_peers()` (runtime peers map), addressed at `SocketAddr::new(peer.ip, peer.port)`.
- Proto gating: only send `ClusterName` to a peer whose advertised `protocol_version` supports it (see Task 6 `supports_cluster_name`).

---

## File Structure

- `src-tauri/src/cluster_name.rs` — **new**: pure register logic (comparison + next-version) with unit tests. Declared in `lib.rs`.
- `src-tauri/src/storage.rs` — load/save `network_name_version` + `network_name_origin` sibling files; bump version inside `regenerate_identity`'s name path is handled by callers, not here.
- `src-tauri/src/state.rs` — add `network_name_version` + `network_name_origin` to `AppState`.
- `src-tauri/src/app.rs` — load version/origin into state at startup.
- `src-tauri/src/protocol.rs` — add `Message::ClusterName`; extend `ClusterInfo`.
- `src-tauri/src/net_util.rs` — `supports_cluster_name`, `send_cluster_name_to`, `broadcast_cluster_name`.
- `src-tauri/src/commands/identity.rs` — `apply_local_rename` helper; route `set_network_identity` + `regenerate_network_identity` through it.
- `src-tauri/src/handlers.rs` — handle inbound `Message::ClusterName`; build extended `ClusterInfo`; anti-entropy send in the `PeerDiscovery` arm.
- `src-tauri/src/pairing/mod.rs` — adopt full register from `ClusterInfo` at join.
- `src-tauri/src/discovery.rs` — bump `CLUSTERCUT_PROTOCOL_VERSION` to `0.3.4`.
- `src/types.ts`, `src/components/SettingsView.tsx`, `src/App.tsx` — confirm dialog on Provisioned→Auto in an active cluster.

---

## Task 1: Pure register logic (new `cluster_name` module)

**Files:**
- Create: `src-tauri/src/cluster_name.rs`
- Modify: `src-tauri/src/lib.rs:8` (add `mod cluster_name;`)

- [ ] **Step 1: Create the module with the failing tests**

Create `src-tauri/src/cluster_name.rs` with the full content:

```rust
//! Pure logic for the shared cluster-name register `(name, version, origin)`.
//!
//! The cluster name is replicated across a leaderless set of peers with
//! last-write-wins semantics: a Lamport-style `version` counter decides the
//! winner, and `origin` (the device_id that set the name) breaks ties so two
//! concurrent renames at the same version converge deterministically. See
//! `docs/superpowers/specs/2026-06-06-cluster-name-convergence-design.md`.

/// Returns true if an incoming register should replace the local one.
///
/// Incoming wins iff its version is strictly higher, or the versions are equal
/// and its origin sorts strictly after the local origin (string comparison).
/// Equal version AND equal origin is NOT a win (idempotent — used for gossip
/// de-duplication).
pub(crate) fn incoming_register_wins(
    local_version: u64,
    local_origin: &str,
    incoming_version: u64,
    incoming_origin: &str,
) -> bool {
    if incoming_version != local_version {
        return incoming_version > local_version;
    }
    incoming_origin > local_origin
}

/// The version a local rename should claim: one past the highest version we
/// currently know. Because every accepted incoming register overwrites the
/// local version, the local version always tracks the max seen, so `+1` beats
/// everything currently known.
pub(crate) fn next_local_version(local_version: u64) -> u64 {
    local_version + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_version_wins() {
        assert!(incoming_register_wins(1, "dev-a", 2, "dev-a"));
    }

    #[test]
    fn lower_version_loses() {
        assert!(!incoming_register_wins(2, "dev-a", 1, "dev-z"));
    }

    #[test]
    fn equal_version_higher_origin_wins() {
        // Tie broken by origin: "dev-z" > "dev-a".
        assert!(incoming_register_wins(3, "dev-a", 3, "dev-z"));
    }

    #[test]
    fn equal_version_lower_origin_loses() {
        assert!(!incoming_register_wins(3, "dev-z", 3, "dev-a"));
    }

    #[test]
    fn identical_register_is_not_a_win() {
        // Idempotent: same version + same origin → no adoption, no re-gossip.
        assert!(!incoming_register_wins(5, "dev-a", 5, "dev-a"));
    }

    #[test]
    fn upgrade_zero_version_converges_by_origin() {
        // Two pre-feature peers both at version 0 converge to higher origin.
        assert!(incoming_register_wins(0, "dev-aaa", 0, "dev-bbb"));
        assert!(!incoming_register_wins(0, "dev-bbb", 0, "dev-aaa"));
    }

    #[test]
    fn next_version_is_one_past_local() {
        assert_eq!(next_local_version(0), 1);
        assert_eq!(next_local_version(41), 42);
    }
}
```

- [ ] **Step 2: Add the module declaration**

In `src-tauri/src/lib.rs`, add the line `mod cluster_name;` immediately after `mod clipboard;` (line 2), keeping the list alphabetical-ish:

```rust
mod clipboard;
mod cluster_name;
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml cluster_name`
Expected: PASS (7 tests). (They pass immediately because the module ships with its implementation; this task is the pure, fully-tested core other tasks build on.)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/cluster_name.rs src-tauri/src/lib.rs
git commit -m "feat: pure cluster-name register convergence logic + tests"
```

---

## Task 2: Persist version + origin (storage sibling files)

**Files:**
- Modify: `src-tauri/src/storage.rs` (add four functions near `load_network_name`/`save_network_name`, ~line 8-47)

- [ ] **Step 1: Add load/save functions for the two sibling files**

In `src-tauri/src/storage.rs`, add these four functions immediately after `save_network_name` (after line ~47):

```rust
/// Load the cluster-name version counter. Missing/invalid file → 0 (pre-issue
/// default; an upgraded install starts unversioned and converges by origin).
pub fn load_network_name_version(app: &AppHandle) -> u64 {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name_version", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return 0,
    };
    if let Ok(s) = fs::read_to_string(&path) {
        if let Ok(v) = s.trim().parse::<u64>() {
            return v;
        }
    }
    0
}

pub fn save_network_name_version(app: &AppHandle, version: u64) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name_version", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, version.to_string());
}

/// Load the device_id that set the current cluster name (tie-breaker). Missing
/// file → empty string; callers seed it with the local device_id at startup so
/// an unversioned install has a well-formed origin.
pub fn load_network_name_origin(app: &AppHandle) -> String {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name_origin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return String::new(),
    };
    if let Ok(s) = fs::read_to_string(&path) {
        let trimmed = s.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    String::new()
}

pub fn save_network_name_origin(app: &AppHandle, origin: &str) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name_origin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, origin);
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds (unused-function warnings are acceptable until later tasks wire these in).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/storage.rs
git commit -m "feat: persist cluster-name version + origin as sibling files"
```

---

## Task 3: Add version + origin to AppState and load at startup

**Files:**
- Modify: `src-tauri/src/state.rs` (struct field ~line 52, default ~line 154)
- Modify: `src-tauri/src/app.rs` (startup load, near the existing network_name load ~line 602-603)

- [ ] **Step 1: Add the struct fields**

In `src-tauri/src/state.rs`, add two fields immediately after `pub network_name: Arc<Mutex<String>>,` (line 52):

```rust
    // Cluster-name version counter (Lamport-style) and the device_id that set
    // the current name (tie-breaker). Together with `network_name` these form
    // the replicated cluster-name register. See cluster_name.rs.
    pub network_name_version: Arc<Mutex<u64>>,
    pub network_name_origin: Arc<Mutex<String>>,
```

- [ ] **Step 2: Add the defaults**

In `src-tauri/src/state.rs`, in the constructor/default where fields are initialized, add immediately after `network_name: Arc::new(Mutex::new(String::new())),` (line 154):

```rust
            network_name_version: Arc::new(Mutex::new(0)),
            network_name_origin: Arc::new(Mutex::new(String::new())),
```

- [ ] **Step 3: Load them at startup**

In `src-tauri/src/app.rs`, find the existing network_name load (around line 602-603):

```rust
                // 3b. Load Network Name (for mDNS)
                let network_name = load_network_name(app_handle);
                *state.network_name.lock().unwrap() = network_name.clone();
```

Add immediately after it:

```rust
                // 3b-ii. Load the cluster-name register version + origin
                // (issue: cluster-name convergence). A pre-feature install has
                // no version/origin files → version 0, origin seeded to our own
                // device_id so the register is well-formed.
                let nn_version = crate::storage::load_network_name_version(app_handle);
                let mut nn_origin = crate::storage::load_network_name_origin(app_handle);
                if nn_origin.is_empty() {
                    nn_origin = device_id.clone();
                }
                *state.network_name_version.lock().unwrap() = nn_version;
                *state.network_name_origin.lock().unwrap() = nn_origin;
```

NOTE: this must come AFTER `device_id` is loaded (it is set earlier in setup, around line 585-594). Verify `device_id` is in scope at this point; if the network_name load happens before `device_id` is defined, move this block to just after `*state.local_device_id.lock().unwrap() = device_id.clone();`.

- [ ] **Step 4: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/state.rs src-tauri/src/app.rs
git commit -m "feat: cluster-name version + origin in AppState, loaded at startup"
```

---

## Task 4: Wire-protocol — `Message::ClusterName` + extended `ClusterInfo`

**Files:**
- Modify: `src-tauri/src/protocol.rs` (`ClusterInfo` ~line 287-293; `Message` enum, add variant near `ClusterInfo(ClusterInfo)` ~line 321)
- Modify: `src-tauri/src/handlers.rs` (the `ClusterInfo` builder ~line 1268-1273 — add the two new fields)
- Modify: `src-tauri/src/pairing/mod.rs` (the `ClusterInfo { .. }` destructure ~line 358 — add the two new fields)

- [ ] **Step 1: Extend `ClusterInfo` and add the `ClusterName` message**

In `src-tauri/src/protocol.rs`, change the `ClusterInfo` struct (lines ~287-293) to add two fields:

```rust
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClusterInfo {
    /// Stable cluster identifier (UUID). Non-secret handle for grouping in
    /// the UI and gossip-loop suppression.
    pub cluster_id: String,
    pub known_peers: Vec<crate::peer::Peer>,
    pub network_name: String,
    /// Cluster-name register version + origin so a joiner adopts the full
    /// register, not just the string. `#[serde(default)]` keeps wire-compat
    /// with peers that don't send these yet.
    #[serde(default)]
    pub network_name_version: u64,
    #[serde(default)]
    pub network_name_origin: String,
}
```

In the same file, add a new variant to the `Message` enum, immediately after `ClusterInfo(ClusterInfo),` (line ~321):

```rust
    /// Shared cluster-name register announcement, sent post-pairing over
    /// QUIC/mTLS. Carries the full `(name, version, origin)` register so
    /// receivers can converge via last-write-wins. See cluster_name.rs.
    ClusterName {
        name: String,
        version: u64,
        origin: String,
    },
```

- [ ] **Step 2: Fix the `ClusterInfo` builder in handlers.rs**

In `src-tauri/src/handlers.rs`, the `ClusterInfoRequest` arm builds a `ClusterInfo` (around lines 1268-1273). Replace that construction:

```rust
            let network_name = listener_state.network_name.lock().unwrap().clone();
            let info = crate::protocol::ClusterInfo {
                cluster_id,
                known_peers: known_peers_vec,
                network_name,
            };
```

with:

```rust
            let network_name = listener_state.network_name.lock().unwrap().clone();
            let network_name_version = *listener_state.network_name_version.lock().unwrap();
            let network_name_origin = listener_state.network_name_origin.lock().unwrap().clone();
            let info = crate::protocol::ClusterInfo {
                cluster_id,
                known_peers: known_peers_vec,
                network_name,
                network_name_version,
                network_name_origin,
            };
```

- [ ] **Step 3: Fix the `ClusterInfo` destructure in pairing/mod.rs**

In `src-tauri/src/pairing/mod.rs` (line ~358), the join path destructures `ClusterInfo`. Replace:

```rust
    let crate::protocol::ClusterInfo { cluster_id, known_peers, network_name } = cluster_info;
```

with (Task 7 will use the two new bindings; for now bind them so the struct destructure is exhaustive):

```rust
    let crate::protocol::ClusterInfo {
        cluster_id,
        known_peers,
        network_name,
        network_name_version,
        network_name_origin,
    } = cluster_info;
```

To avoid an "unused variable" error before Task 7 wires them in, prefix with underscore for now: `network_name_version: _nn_version` and `network_name_origin: _nn_origin`. Concretely:

```rust
    let crate::protocol::ClusterInfo {
        cluster_id,
        known_peers,
        network_name,
        network_name_version: _nn_version,
        network_name_origin: _nn_origin,
    } = cluster_info;
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors. (The new `Message::ClusterName` variant is not yet handled in the match — Rust enum matches in this codebase use a catch-all or will warn; if `handle_message`'s match is non-exhaustive the build will FAIL. If so, that is fixed in Task 6. To keep this task green, add a temporary no-op arm in `handlers.rs` `handle_message`: `Message::ClusterName { .. } => {}` and replace it with the real handler in Task 6.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/protocol.rs src-tauri/src/handlers.rs src-tauri/src/pairing/mod.rs
git commit -m "feat: add Message::ClusterName + version/origin on ClusterInfo"
```

---

## Task 5: Proto bump + send/broadcast helpers

**Files:**
- Modify: `src-tauri/src/discovery.rs:23` (`CLUSTERCUT_PROTOCOL_VERSION`)
- Modify: `src-tauri/src/net_util.rs` (add `supports_cluster_name`, `send_cluster_name_to`, `broadcast_cluster_name`)

- [ ] **Step 1: Bump the advertised protocol version**

In `src-tauri/src/discovery.rs`, change line 23:

```rust
pub const CLUSTERCUT_PROTOCOL_VERSION: &str = "0.3.3";
```

to:

```rust
pub const CLUSTERCUT_PROTOCOL_VERSION: &str = "0.3.4";
```

Add a brief doc note above it (append to the existing doc comment block) explaining 0.3.4 advertises `Message::ClusterName` availability and does NOT raise the pairing floor (`is_protocol_compatible` stays at 0.3.3):

```rust
/// - 0.3.4: advertises support for the `ClusterName` register-sync message
///   (shared cluster-name convergence). NOT a pairing break — the
///   compatibility floor in `is_protocol_compatible` stays at 0.3.3; this
///   version is only used to gate whether we send `ClusterName` to a peer.
```

- [ ] **Step 2: Add the helpers to net_util.rs**

In `src-tauri/src/net_util.rs`, add near the existing `is_protocol_compatible` (after line ~38) and `gossip_peer`:

```rust
/// True if a peer advertising `version` understands `Message::ClusterName`
/// (introduced in wire 0.3.4). Peers without a `proto` property, or older,
/// are not sent the message and keep per-device-name behavior.
pub(crate) fn supports_cluster_name(version: Option<&str>) -> bool {
    let Some(v) = version else { return false };
    parse_protocol_version(v).map_or(false, |parsed| parsed >= (0, 3, 4))
}

/// Send our current cluster-name register to a single peer address, if that
/// peer's protocol supports it. Fire-and-forget.
pub(crate) fn send_cluster_name_to(
    addr: std::net::SocketAddr,
    peer_protocol_version: Option<&str>,
    name: String,
    version: u64,
    origin: String,
    transport: &Transport,
) {
    if !supports_cluster_name(peer_protocol_version) {
        return;
    }
    let msg = Message::ClusterName { name, version, origin };
    let data = match serde_json::to_vec(&msg) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Failed to serialise ClusterName: {}", e);
            return;
        }
    };
    let transport_clone = transport.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = transport_clone.send_message(addr, &data).await {
            tracing::debug!("Failed to send ClusterName to {}: {}", addr, e);
        }
    });
}

/// Broadcast our current cluster-name register to all known peers (optionally
/// excluding one address, e.g. the peer we just received it from). Each send is
/// gated on the peer's advertised protocol version.
pub(crate) fn broadcast_cluster_name(
    name: &str,
    version: u64,
    origin: &str,
    state: &AppState,
    transport: &Transport,
    exclude_addr: Option<std::net::SocketAddr>,
) {
    for p in state.get_peers().values() {
        let addr = std::net::SocketAddr::new(p.ip, p.port);
        if Some(addr) == exclude_addr {
            continue;
        }
        send_cluster_name_to(
            addr,
            p.protocol_version.as_deref(),
            name.to_string(),
            version,
            origin.to_string(),
            transport,
        );
    }
}
```

NOTE: `Message`, `AppState`, and `Transport` are already imported at the top of `net_util.rs` (used by `gossip_peer`/`probe_ip`). `parse_protocol_version` is a private fn in this same file.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds (unused-function warnings acceptable until Tasks 6/7 call them).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/discovery.rs src-tauri/src/net_util.rs
git commit -m "feat: proto 0.3.4 gate + ClusterName send/broadcast helpers"
```

---

## Task 6: Inbound `ClusterName` convergence + anti-entropy on PeerDiscovery

**Files:**
- Modify: `src-tauri/src/handlers.rs` (replace the temporary `Message::ClusterName { .. } => {}` arm from Task 4; add a send in the `PeerDiscovery` arm ~line 865)

- [ ] **Step 1: Implement the `ClusterName` handler**

In `src-tauri/src/handlers.rs`, replace the temporary arm `Message::ClusterName { .. } => {}` (added in Task 4) with the full handler. Place it near the other cluster arms:

```rust
        Message::ClusterName { name, version, origin } => {
            // Converge the shared cluster-name register (last-write-wins by
            // version, ties by origin). On a win: adopt, persist, re-register
            // mDNS, notify the UI, and re-gossip to other peers (excluding the
            // sender). If we are strictly newer, push our register back so the
            // sender converges up.
            let (local_version, local_origin) = {
                let v = *listener_state.network_name_version.lock().unwrap();
                let o = listener_state.network_name_origin.lock().unwrap().clone();
                (v, o)
            };

            if crate::cluster_name::incoming_register_wins(
                local_version, &local_origin, version, &origin,
            ) {
                {
                    *listener_state.network_name.lock().unwrap() = name.clone();
                    *listener_state.network_name_version.lock().unwrap() = version;
                    *listener_state.network_name_origin.lock().unwrap() = origin.clone();
                }
                crate::storage::save_network_name(&listener_handle, &name);
                crate::storage::save_network_name_version(&listener_handle, version);
                crate::storage::save_network_name_origin(&listener_handle, &origin);

                // Re-register mDNS with the adopted name.
                let device_id = listener_state.local_device_id.lock().unwrap().clone();
                let port = listener_state
                    .transport
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|t| t.local_addr().ok())
                    .map(|a| a.port())
                    .unwrap_or(4654);
                if let Some(discovery) = listener_state.discovery.lock().unwrap().as_mut() {
                    let _ = discovery.register(&device_id, &name, port);
                }

                let _ = listener_handle.emit("network-update", ());
                tracing::info!("Adopted cluster name '{}' (v{} from {})", name, version, origin);

                // Re-gossip to everyone except the sender.
                crate::net_util::broadcast_cluster_name(
                    &name, version, &origin,
                    &listener_state, &transport_inside, Some(addr),
                );
            } else if local_version > version
                || (local_version == version && local_origin > origin)
            {
                // We hold a strictly-newer register; push it back to the sender
                // so it converges up. (Equal version + equal origin is a no-op.)
                let local_name = listener_state.network_name.lock().unwrap().clone();
                // Look up the sender's protocol version from known peers by addr.
                let sender_proto = listener_state
                    .get_peers()
                    .values()
                    .find(|p| std::net::SocketAddr::new(p.ip, p.port) == addr)
                    .and_then(|p| p.protocol_version.clone());
                crate::net_util::send_cluster_name_to(
                    addr,
                    sender_proto.as_deref(),
                    local_name,
                    local_version,
                    local_origin,
                    &transport_inside,
                );
            }
        }
```

- [ ] **Step 2: Anti-entropy — announce our register when a peer appears**

In `src-tauri/src/handlers.rs`, in the `Message::PeerDiscovery(mut peer) => {` arm (starts ~line 865), at the END of the arm (after the peer has been added/updated and any existing gossip-back happens), add a send of our current cluster name to that peer:

```rust
            // Anti-entropy: every time we hear from a peer (startup probe, mDNS
            // rediscovery, gossip), tell it our current cluster-name register so
            // peers that were offline during a rename converge. Gated on the
            // peer's protocol version inside send_cluster_name_to.
            {
                let name = listener_state.network_name.lock().unwrap().clone();
                let version = *listener_state.network_name_version.lock().unwrap();
                let origin = listener_state.network_name_origin.lock().unwrap().clone();
                let peer_addr = std::net::SocketAddr::new(peer.ip, peer.port);
                crate::net_util::send_cluster_name_to(
                    peer_addr,
                    peer.protocol_version.as_deref(),
                    name, version, origin,
                    &transport_inside,
                );
            }
```

NOTE: confirm the variable holding the inbound peer in that arm is named `peer` and exposes `.ip`, `.port`, `.protocol_version` (it is `crate::peer::Peer`). Place this block where `peer` is still in scope and after its fields are finalized.

- [ ] **Step 3: Verify it compiles and the pure tests still pass**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors.
Run: `cargo test --manifest-path src-tauri/Cargo.toml cluster_name`
Expected: PASS (7 tests).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/handlers.rs
git commit -m "feat: converge inbound ClusterName + anti-entropy on PeerDiscovery"
```

---

## Task 7: Adopt the full register at pairing

**Files:**
- Modify: `src-tauri/src/pairing/mod.rs` (~line 358-368, the join adopt block)

- [ ] **Step 1: Adopt version + origin alongside the name**

In `src-tauri/src/pairing/mod.rs`, update the destructure from Task 4 to bind the new fields normally (drop the underscores):

```rust
    let crate::protocol::ClusterInfo {
        cluster_id,
        known_peers,
        network_name,
        network_name_version,
        network_name_origin,
    } = cluster_info;
```

Then, in the block that saves cluster_id + network_name (lines ~360-368), add persistence + state for the version/origin. Replace:

```rust
        let mut nn = state.network_name.lock().unwrap();
        *nn = network_name.clone();
        crate::storage::save_network_name(&app_handle, &network_name);
```

with:

```rust
        let mut nn = state.network_name.lock().unwrap();
        *nn = network_name.clone();
        crate::storage::save_network_name(&app_handle, &network_name);

        // Adopt the responder's cluster-name register version + origin so the
        // joiner participates in convergence from the start. If the responder
        // sent an empty origin (pre-0.3.4 responder), fall back to its
        // device_id so the register stays well-formed.
        let adopted_origin = if network_name_origin.is_empty() {
            responder_device_id.clone()
        } else {
            network_name_origin.clone()
        };
        *state.network_name_version.lock().unwrap() = network_name_version;
        *state.network_name_origin.lock().unwrap() = adopted_origin.clone();
        crate::storage::save_network_name_version(&app_handle, network_name_version);
        crate::storage::save_network_name_origin(&app_handle, &adopted_origin);
```

NOTE: verify `responder_device_id` is in scope here (it is used a few lines below in the known_peers loop). If its binding is later in the function, use `network_name_origin` directly without the fallback (still correct, just possibly empty for a pre-0.3.4 responder).

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors, and no "unused variable" warnings for `network_name_version`/`network_name_origin`.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/pairing/mod.rs
git commit -m "feat: adopt cluster-name register (version+origin) at pairing"
```

---

## Task 8: Route renames through `apply_local_rename` (version bump + propagate)

**Files:**
- Modify: `src-tauri/src/commands/identity.rs` (add helper; rewrite `set_network_identity` + `regenerate_network_identity`)

This task needs `Transport` available to the commands for broadcasting. `Transport` is managed Tauri state (used as `transport: State<'_, Transport>` in `commands/peers.rs`), so add it as a parameter.

- [ ] **Step 1: Add the `apply_local_rename` helper**

In `src-tauri/src/commands/identity.rs`, add this private helper (not a command) above `set_network_identity`:

```rust
/// Apply a local cluster-name change: bump the register version, set origin to
/// this device, persist all three fields, re-register mDNS, and broadcast the
/// new register to connected peers. Shared by provisioned set-name and auto
/// regenerate. Does NOT touch the PIN.
fn apply_local_rename(
    name: &str,
    state: &AppState,
    transport: &crate::transport::Transport,
    app_handle: &tauri::AppHandle,
) {
    let device_id = state.local_device_id.lock().unwrap().clone();
    let new_version = {
        let cur = *state.network_name_version.lock().unwrap();
        crate::cluster_name::next_local_version(cur)
    };

    *state.network_name.lock().unwrap() = name.to_string();
    *state.network_name_version.lock().unwrap() = new_version;
    *state.network_name_origin.lock().unwrap() = device_id.clone();

    crate::storage::save_network_name(app_handle, name);
    crate::storage::save_network_name_version(app_handle, new_version);
    crate::storage::save_network_name_origin(app_handle, &device_id);

    // Re-register mDNS with the new name.
    let port = state
        .transport
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|t| t.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(4654);
    if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
        let _ = discovery.register(&device_id, name, port);
    }

    // Propagate to connected peers.
    crate::net_util::broadcast_cluster_name(
        name, new_version, &device_id, state, transport, None,
    );

    let _ = app_handle.emit("network-update", ());
}
```

NOTE: the imports at the top of `identity.rs` are currently `use crate::state::AppState; use tauri::{Emitter, State};`. `Emitter` is already imported (for `.emit`). `AppState` is imported. Fully-qualify `crate::transport::Transport`, `crate::cluster_name`, `crate::net_util`, `crate::storage` inline as shown (no new `use` lines required).

- [ ] **Step 2: Rewrite `set_network_identity` to use it**

Replace the entire `set_network_identity` command in `src-tauri/src/commands/identity.rs` with:

```rust
#[tauri::command]
pub(crate) fn set_network_identity(
    name: String,
    pin: String,
    state: State<'_, AppState>,
    transport: State<'_, crate::transport::Transport>,
    app_handle: tauri::AppHandle,
) {
    // PIN stays per-device; persist it as before.
    *state.network_pin.lock().unwrap() = pin.clone();
    crate::storage::save_network_pin(&app_handle, &pin);

    // The name is shared cluster state: bump the register + propagate.
    apply_local_rename(&name, &state, &transport, &app_handle);
}
```

- [ ] **Step 3: Rewrite `regenerate_network_identity` to use it**

Replace the entire `regenerate_network_identity` command with:

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

NOTE: `regenerate_identity` already writes a fresh `network_name` file; `apply_local_rename` then overwrites it with the same name plus the bumped version/origin files — correct and harmless.

- [ ] **Step 4: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds with no errors. Both commands are already registered in the invoke_handler under their existing names; adding a `transport` parameter does not change registration.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/identity.rs
git commit -m "feat: route cluster renames through apply_local_rename (bump + propagate)"
```

---

## Task 9: Frontend — confirm dialog on Provisioned→Auto in an active cluster

**Files:**
- Modify: `src/components/SettingsView.tsx` (Autogenerated button ~line 259; new dialog; new prop)
- Modify: `src/App.tsx` (pass `hasClusterPeers` prop ~line 1159)

- [ ] **Step 1: Accept a `hasClusterPeers` prop**

In `src/components/SettingsView.tsx`, change the component signature (line ~17):

```tsx
export function SettingsView({
  onSettingsRefreshed
}: {
  onSettingsRefreshed?: () => void;
}) {
```

to:

```tsx
export function SettingsView({
  onSettingsRefreshed,
  hasClusterPeers = false,
}: {
  onSettingsRefreshed?: () => void;
  hasClusterPeers?: boolean;
}) {
```

- [ ] **Step 2: Add dialog state**

In `src/components/SettingsView.tsx`, next to the existing `const [compressDialogOpen, setCompressDialogOpen] = useState(false);` (line ~34), add:

```tsx
  const [autoRenameDialogOpen, setAutoRenameDialogOpen] = useState(false);
```

- [ ] **Step 3: Gate the Autogenerated button**

In `src/components/SettingsView.tsx`, the "Autogenerated" button currently is (line ~259):

```tsx
              onClick={() => setSettings({ ...settings, cluster_mode: "auto" })}
```

Replace that `onClick` with a guarded handler that confirms when switching from provisioned while in an active cluster:

```tsx
              onClick={() => {
                if (settings.cluster_mode === "provisioned" && hasClusterPeers) {
                  // Switching to Auto regenerates the name and renames the
                  // cluster for everyone — confirm first (issue: cluster-name
                  // convergence).
                  setAutoRenameDialogOpen(true);
                } else {
                  setSettings({ ...settings, cluster_mode: "auto" });
                }
              }}
```

- [ ] **Step 4: Add the confirm dialog**

In `src/components/SettingsView.tsx`, next to the existing compress `<Dialog .../>` (around line 590-602), add a second dialog:

```tsx
      <Dialog
        open={autoRenameDialogOpen}
        title="Switch to an auto-generated name?"
        description="This will rename the cluster for all connected devices to a new auto-generated name. Continue?"
        type="danger"
        confirmLabel="Rename cluster"
        onConfirm={() => {
          setSettings({ ...settings, cluster_mode: "auto" });
          setAutoRenameDialogOpen(false);
        }}
        onCancel={() => setAutoRenameDialogOpen(false)}
      />
```

NOTE: on confirm, switching `cluster_mode` to `"auto"` triggers the existing autosave effect, which detects provisioned→auto and invokes `regenerate_network_identity` — now version-bumped + propagated. On cancel, nothing changes and the mode stays Provisioned.

- [ ] **Step 5: Pass the prop from App.tsx**

In `src/App.tsx`, the render is (line ~1159):

```tsx
              <SettingsView onSettingsRefreshed={fetchSettings} />
```

`myPeers` (trusted peers) is defined at line ~773 (`const myPeers = peers.filter(p => p.is_trusted);`), which is in scope at render. Change the render to:

```tsx
              <SettingsView onSettingsRefreshed={fetchSettings} hasClusterPeers={myPeers.length > 0} />
```

- [ ] **Step 6: Verify it builds**

Run: `npm run build`
Expected: build succeeds, no type errors referencing `hasClusterPeers` or `autoRenameDialogOpen`.

- [ ] **Step 7: Commit**

```bash
git add src/components/SettingsView.tsx src/App.tsx
git commit -m "feat: confirm before renaming cluster on Provisioned->Auto switch"
```

---

## Task 10: Full build + test sweep

- [ ] **Step 1: Run the full Rust suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all tests pass, including the 7 new `cluster_name` tests.

- [ ] **Step 2: Run the frontend build**

Run: `npm run build`
Expected: succeeds, no type errors.

- [ ] **Step 3: Manual verification checklist (record results)**

- Two-peer cluster, both on this build: rename on A via Settings → Provisioned (type a name). B's "My Cluster" updates live to the new name.
- Rename on A while B is closed; reopen B → B converges to A's name on reconnect (anti-entropy via PeerDiscovery).
- A in active cluster: Settings → Autogenerated shows the confirm dialog; **Confirm** renames both A and B to the new auto name; **Cancel** leaves the name and keeps Provisioned selected.
- Pair a fresh device into the cluster → it shows the current cluster name immediately after joining.
- (Optional) One peer left on 0.3.3: it pairs fine but keeps its own local name (no convergence) — confirms graceful degradation.

- [ ] **Step 4: Final commit (only if manual-test fixups were needed)**

```bash
git add -A
git commit -m "test: cluster-name convergence verification fixups"
```

---

## Self-Review Notes

- **Spec coverage:** versioned register → Tasks 2,3; convergence rule → Task 1 (+ applied in Task 6); local rename + version bump → Task 8; propagation push → Task 8, anti-entropy → Task 6 (PeerDiscovery), pairing seed → Tasks 4,7; safe degradation / proto gate → Task 5; mode-switch confirm UX → Task 9; upgrade auto-converge → Task 1 (`version 0` tie by origin) + Task 3 (origin seeded to local device_id). PIN untouched → Task 8 keeps PIN paths separate.
- **Type consistency:** the register triple is `(name: String, version: u64, origin: String)` everywhere; `Message::ClusterName { name, version, origin }`; `ClusterInfo.network_name_version: u64` / `network_name_origin: String`; helpers `incoming_register_wins`, `next_local_version`, `supports_cluster_name`, `send_cluster_name_to`, `broadcast_cluster_name`, `apply_local_rename` — names used identically across tasks.
- **No placeholders:** every code step contains complete code.
- **Known coupling:** Task 4 deliberately adds a temporary `Message::ClusterName { .. } => {}` arm so each task compiles green; Task 6 replaces it with the real handler. Called out explicitly so it isn't missed.
