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
use crate::transport::Transport;
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
        runtime.insert(
            "clustercut-a".to_string(),
            peer("clustercut-a", "192.168.1.10", 4654),
        );

        assert!(peers_needing_probe(&known, &runtime).is_empty());
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
        assert_eq!(peers_needing_probe(&known, &runtime).len(), 2);
    }
}
