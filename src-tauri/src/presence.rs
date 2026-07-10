//! Peer-presence convergence: shared re-probe used by startup/retry/netmon
//! recovery, the anti-entropy loop (absent-peer re-probe + membership sync),
//! and helpers that pause presence bookkeeping around suspend/outages.
//!
//! Background: the 5s heartbeat only targets peers already in the runtime
//! map, and mdns-sd's long-lived browse won't re-emit `ServiceResolved` for
//! records it still caches. So any peer that drops out of the runtime map
//! needs an active `PeerDiscovery` re-probe to come back — that's this
//! module's job. (Ping/Pong used to be presence-inert, which is why the old
//! netmon recovery never repopulated the list; they now refresh liveness via
//! `touch_peer_by_addr`, but only `PeerDiscovery` can ADD a peer.)

use crate::peer::Peer;
use crate::state::AppState;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

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

/// Outcome of observing the local IP for a possible network change.
/// `changed` drives recovery (silent wipe + re-probe); `notify` drives the
/// one-shot user-facing "network changed" toast (debounced).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NetworkChange {
    pub changed: bool,
    pub notify: bool,
}

/// Record the current local IP and react if it differs from the last one we
/// saw. A changed IP means the machine switched networks or a VPN came
/// up/down — the OS connectivity monitor often does NOT flag these because
/// full internet access is preserved, so the peer left/joined suppression
/// (which keys off [`presence_paused`]) would otherwise stay off and every
/// now-unreachable peer would emit a spurious "left the cluster" toast
/// (issue #19).
///
/// On a real change this opens the `grace` window (silences left/joined via
/// `should_notify` → `presence_paused`) and flags a silent peer wipe
/// (`pending_network_wipe`, consumed by `start_recovery_tasks`), exactly like
/// the IP-changed branch of `resume_recovery`. `notify` is true only when we
/// were NOT already inside a grace window, so a flapping VPN or a change
/// right after a resume refreshes the window without firing a second toast.
///
/// `current_ip == None` (we couldn't read an address, e.g. mid-outage) is a
/// no-op that leaves the last-known IP intact — the next observation decides.
pub(crate) fn note_ip_change(
    state: &AppState,
    current_ip: Option<std::net::IpAddr>,
    now: std::time::Instant,
    grace: std::time::Duration,
) -> NetworkChange {
    let Some(current_ip) = current_ip else {
        return NetworkChange { changed: false, notify: false };
    };
    let changed = {
        let mut last = state.last_known_local_ip.lock().unwrap();
        let changed = last.map_or(false, |prev| prev != current_ip);
        *last = Some(current_ip);
        changed
    };
    if !changed {
        return NetworkChange { changed: false, notify: false };
    }
    // Debounce the toast: only the change that OPENS a grace window announces
    // itself. A change while one is still open (flapping VPN, or right after a
    // resume) refreshes the window and re-flags the wipe, but stays silent.
    let already_in_grace = state
        .resume_grace_until
        .lock()
        .unwrap()
        .map_or(false, |end| now < end);
    *state.resume_grace_until.lock().unwrap() = Some(now + grace);
    state.pending_network_wipe.store(true, Ordering::Relaxed);
    NetworkChange {
        changed: true,
        notify: !already_in_grace,
    }
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
/// listening on an (ip, port) endpoint some runtime peer already answers on
/// (a `manual-<ip>` placeholder and its real peer share an endpoint). The
/// match is on the full endpoint, not the IP alone — several distinct
/// devices can share one NATed/port-forwarded IP.
///
/// `include_placeholders: false` also skips `manual-<ip>` entries: those are
/// reachability hints for addresses that once accepted QUIC (e.g. a VPN
/// gateway) and are never garbage-collected, so a periodic caller would dial
/// dead ones forever. Event-driven bursts (startup, retry, netmon recovery)
/// pass true to preserve the old probe-everything behavior.
pub(crate) fn peers_needing_probe(
    known: &HashMap<String, Peer>,
    runtime: &HashMap<String, Peer>,
    include_placeholders: bool,
) -> Vec<Peer> {
    let online: std::collections::HashSet<(std::net::IpAddr, u16)> =
        runtime.values().map(|p| (p.ip, p.port)).collect();
    known
        .values()
        .filter(|p| include_placeholders || !p.id.starts_with("manual-"))
        .filter(|p| !runtime.contains_key(&p.id))
        .filter(|p| !online.contains(&(p.ip, p.port)))
        .cloned()
        .collect()
}

/// Merge cluster membership from an authenticated `ClusterInfo` snapshot.
///
/// Closes the "joined while I was away" hole: pairing-time gossip goes only
/// to peers online at that instant and never repeats, so a member that
/// paired in while we were offline is mutually unreachable forever (neither
/// side has the other's fingerprint pinned) until a manual re-pair.
///
/// Conservative by design — inserts only entries that are new to us,
/// fingerprinted, not ourselves, not `manual-<ip>` placeholders (those
/// are the sender's local reachability hints, not members), and not
/// tombstoned (deleted this session — a member that missed the removal
/// broadcast must not gossip the deleted device straight back). Existing
/// entries are never overwritten: direct contact refreshes those. Runtime
/// presence is untouched; the caller probes imports to surface them.
/// Cluster name/id/PIN adoption is pairing-only and does NOT happen here.
///
/// Returns the imported peers. Caller persists `known_peers` if non-empty.
pub(crate) fn merge_cluster_membership(
    state: &AppState,
    info: &crate::protocol::ClusterInfo,
) -> Vec<Peer> {
    let local_id = state.local_device_id.lock().unwrap().clone();
    let tombstones = state.removed_peer_tombstones.lock().unwrap().clone();
    let mut kp = state.known_peers.lock().unwrap();
    let mut imported = Vec::new();
    for peer in &info.known_peers {
        if peer.id == local_id {
            continue;
        }
        if peer.id.starts_with("manual-") {
            continue;
        }
        if tombstones.contains(&peer.id) {
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

/// Burst re-probe of every known peer that is absent from the runtime map.
/// Returns how many peers were queued for probing.
///
/// Sends the `PeerDiscovery` probe (`net_util::probe_ip`) — NOT `Ping` —
/// because only `PeerDiscovery` makes the far side record us and heartbeat
/// back, which is what actually repopulates both peer lists. Each absent
/// peer gets up to `attempts` tries (2s, then 4s apart): right after a VPN
/// or resume the first packets often race route/ARP/firewall setup.
///
/// `notify: true` surfaces the final attempt's outcome as notifications
/// (user-initiated retry); earlier attempts are always silent.
/// `include_placeholders` — see [`peers_needing_probe`].
pub(crate) fn reprobe_known_peers(
    state: AppState,
    app_handle: tauri::AppHandle,
    notify: bool,
    attempts: u32,
    include_placeholders: bool,
) -> usize {
    let Some(transport) = state.transport.lock().unwrap().clone() else {
        return 0;
    };
    let targets = {
        // Lock order: known_peers before peers (matches prune/reset).
        let kp = state.known_peers.lock().unwrap();
        let rt = state.peers.lock().unwrap();
        peers_needing_probe(&kp, &rt, include_placeholders)
    };
    if targets.is_empty() {
        return 0;
    }
    let attempts = attempts.max(1);
    tracing::info!(
        "[Presence] Re-probing {} absent known peer(s) ({} attempt(s) each)",
        targets.len(),
        attempts
    );
    let count = targets.len();
    for peer in targets {
        tracing::debug!(
            "[Presence] Re-probe target: {} ({}:{}, manual: {})",
            peer.id, peer.ip, peer.port, peer.is_manual
        );
        let s = state.clone();
        let t = transport.clone();
        let a = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            for attempt in 0..attempts {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                    // The peer may have surfaced meanwhile (e.g. its own
                    // heartbeat reached us) — stop burning probes on it.
                    let surfaced = s
                        .peers
                        .lock()
                        .unwrap()
                        .values()
                        .any(|p| p.ip == peer.ip && p.port == peer.port);
                    if surfaced {
                        return;
                    }
                }
                let last = attempt + 1 == attempts;
                if crate::net_util::probe_ip(
                    peer.ip,
                    peer.port,
                    s.clone(),
                    t.clone(),
                    a.clone(),
                    notify && last,
                )
                .await
                {
                    return;
                }
            }
        });
    }
    count
}

/// Anti-entropy cadence. One tick bounds how long a missed-but-reachable
/// KNOWN peer stays invisible (the old behavior was "forever, until app
/// restart"). Membership sync converges slower: one member is queried per
/// tick round-robin, so a brand-new member propagates in O(cluster size)
/// ticks worst case. Traffic per tick: one 2s QUIC dial per absent peer +
/// one small ClusterInfoRequest/reply.
const ANTI_ENTROPY_PERIOD_SECS: u64 = 30;

/// Periodic self-healing for the peer list. Every tick (while presence is
/// not paused): re-probe absent known peers (single silent attempt — the
/// tick period is the retry cadence; `manual-<ip>` placeholders are skipped,
/// see `peers_needing_probe`), then ask one online trusted member for its
/// known_peers so membership converges (see `merge_cluster_membership`).
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

            reprobe_known_peers(state.clone(), app_handle.clone(), false, 1, false);

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

    // ── note_ip_change ─────────────────────────────────────────────────

    fn ip(s: &str) -> std::net::IpAddr {
        s.parse().unwrap()
    }

    const GRACE: std::time::Duration = std::time::Duration::from_secs(45);

    #[test]
    fn note_ip_change_records_first_ip_without_reacting() {
        // Fresh state has no last-known IP; the first observation just records
        // it — no "change", no grace, no wipe, no notification.
        let state = AppState::new();
        let out = note_ip_change(&state, Some(ip("192.168.1.10")), std::time::Instant::now(), GRACE);
        assert_eq!(out, NetworkChange { changed: false, notify: false });
        assert_eq!(*state.last_known_local_ip.lock().unwrap(), Some(ip("192.168.1.10")));
        assert!(!presence_paused(&state));
        assert!(!state.pending_network_wipe.load(Ordering::Relaxed));
    }

    #[test]
    fn note_ip_change_same_ip_is_noop() {
        let state = AppState::new();
        *state.last_known_local_ip.lock().unwrap() = Some(ip("192.168.1.10"));
        let out = note_ip_change(&state, Some(ip("192.168.1.10")), std::time::Instant::now(), GRACE);
        assert_eq!(out, NetworkChange { changed: false, notify: false });
        assert!(!presence_paused(&state));
        assert!(!state.pending_network_wipe.load(Ordering::Relaxed));
    }

    #[test]
    fn note_ip_change_none_current_is_noop_and_preserves_last() {
        let state = AppState::new();
        *state.last_known_local_ip.lock().unwrap() = Some(ip("192.168.1.10"));
        let out = note_ip_change(&state, None, std::time::Instant::now(), GRACE);
        assert_eq!(out, NetworkChange { changed: false, notify: false });
        // Must NOT clobber the last-known IP just because we couldn't read one.
        assert_eq!(*state.last_known_local_ip.lock().unwrap(), Some(ip("192.168.1.10")));
        assert!(!presence_paused(&state));
    }

    #[test]
    fn note_ip_change_detects_change_sets_grace_wipe_and_notifies() {
        let state = AppState::new();
        *state.last_known_local_ip.lock().unwrap() = Some(ip("192.168.1.10"));
        let out = note_ip_change(&state, Some(ip("10.8.0.5")), std::time::Instant::now(), GRACE);
        assert_eq!(out, NetworkChange { changed: true, notify: true });
        assert_eq!(*state.last_known_local_ip.lock().unwrap(), Some(ip("10.8.0.5")));
        // Grace window silences peer left/joined via should_notify -> presence_paused.
        assert!(presence_paused(&state));
        // Old-network peers get silently wiped + re-probed by start_recovery_tasks.
        assert!(state.pending_network_wipe.load(Ordering::Relaxed));
    }

    #[test]
    fn note_ip_change_debounces_notification_when_already_in_grace() {
        // A second change while a grace window is still open (flapping VPN, or a
        // change right after a resume) refreshes the window but must NOT fire a
        // second toast.
        let state = AppState::new();
        *state.last_known_local_ip.lock().unwrap() = Some(ip("192.168.1.10"));
        *state.resume_grace_until.lock().unwrap() =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
        let out = note_ip_change(&state, Some(ip("10.8.0.5")), std::time::Instant::now(), GRACE);
        assert_eq!(out, NetworkChange { changed: true, notify: false });
        assert!(presence_paused(&state));
        assert!(state.pending_network_wipe.load(Ordering::Relaxed));
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
        known.insert(
            "clustercut-a".to_string(),
            peer("clustercut-a", "192.168.1.10", 4654),
        );
        known.insert(
            "clustercut-b".to_string(),
            peer("clustercut-b", "192.168.1.11", 4654),
        );
        let mut runtime = HashMap::new();
        runtime.insert(
            "clustercut-a".to_string(),
            peer("clustercut-a", "192.168.1.10", 4654),
        );

        let need = peers_needing_probe(&known, &runtime, true);
        assert_eq!(need.len(), 1);
        assert_eq!(need[0].id, "clustercut-b");
    }

    #[test]
    fn same_ip_different_port_is_still_probed() {
        // Two distinct devices can share one NATed/port-forwarded IP; the
        // online one must not mask the absent one.
        let mut known = HashMap::new();
        known.insert(
            "clustercut-a".to_string(),
            peer("clustercut-a", "203.0.113.9", 4654),
        );
        known.insert(
            "clustercut-b".to_string(),
            peer("clustercut-b", "203.0.113.9", 4655),
        );
        let mut runtime = HashMap::new();
        runtime.insert(
            "clustercut-a".to_string(),
            peer("clustercut-a", "203.0.113.9", 4654),
        );

        let need = peers_needing_probe(&known, &runtime, true);
        assert_eq!(need.len(), 1);
        assert_eq!(need[0].id, "clustercut-b");
    }

    #[test]
    fn placeholders_skipped_when_excluded() {
        let mut known = HashMap::new();
        known.insert("manual-192.168.1.40".to_string(), {
            let mut p = peer("manual-192.168.1.40", "192.168.1.40", 4654);
            p.is_manual = true;
            p
        });
        known.insert(
            "clustercut-b".to_string(),
            peer("clustercut-b", "192.168.1.11", 4654),
        );
        let runtime = HashMap::new();

        let need = peers_needing_probe(&known, &runtime, false);
        assert_eq!(need.len(), 1);
        assert_eq!(need[0].id, "clustercut-b");
        assert_eq!(peers_needing_probe(&known, &runtime, true).len(), 2);
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
        runtime.insert(
            "clustercut-a".to_string(),
            peer("clustercut-a", "192.168.1.10", 4654),
        );

        assert!(peers_needing_probe(&known, &runtime, true).is_empty());
    }

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
        state.known_peers.lock().unwrap().insert(
            "clustercut-known".to_string(),
            peer("clustercut-known", "192.168.1.30", 4654),
        );

        let me = peer("clustercut-me", "10.8.0.5", 4654);
        let existing = peer("clustercut-known", "192.168.1.99", 4654); // sender's differing view
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

    #[test]
    fn all_selected_when_runtime_empty() {
        let mut known = HashMap::new();
        known.insert(
            "clustercut-a".to_string(),
            peer("clustercut-a", "192.168.1.10", 4654),
        );
        known.insert(
            "clustercut-b".to_string(),
            peer("clustercut-b", "192.168.1.11", 4654),
        );
        let runtime = HashMap::new();
        assert_eq!(peers_needing_probe(&known, &runtime, true).len(), 2);
    }

    #[test]
    fn merge_skips_tombstoned_peer() {
        let state = AppState::new();
        *state.local_device_id.lock().unwrap() = "clustercut-me".to_string();
        state
            .removed_peer_tombstones
            .lock()
            .unwrap()
            .insert("clustercut-kicked".to_string());
        let info = cluster_info(vec![
            peer("clustercut-kicked", "192.168.1.60", 4654),
            peer("clustercut-new", "192.168.1.61", 4654),
        ]);

        let imported = merge_cluster_membership(&state, &info);

        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].id, "clustercut-new");
        assert!(!state
            .known_peers
            .lock()
            .unwrap()
            .contains_key("clustercut-kicked"));
    }
}
