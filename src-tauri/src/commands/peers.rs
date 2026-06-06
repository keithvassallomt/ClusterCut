//! Peer management commands.

use crate::peer::{Peer, PeerView};
use crate::state::AppState;
use crate::{net_util, perform_factory_reset};
use crate::protocol::Message;
use crate::storage::save_known_peers;
use crate::transport::Transport;
use ipnetwork::IpNetwork;
use tauri::{Emitter, State};

#[tauri::command]
pub(crate) fn get_peers(state: State<AppState>) -> std::collections::HashMap<String, PeerView> {
    state.get_peers()
        .into_iter()
        .map(|(id, peer)| (id, PeerView::from_peer(&peer)))
        .collect()
}

#[tauri::command]
pub(crate) fn get_known_peers(state: State<AppState>) -> std::collections::HashMap<String, Peer> {
    state.known_peers.lock().unwrap().clone()
}

/// List of peers loaded from `known_peers.json` without a stored cert
/// fingerprint. Returns an empty Vec for clean v0.3 installs. The frontend
/// reads this on mount to decide whether to show the "please re-pair"
/// banner after a v0.2 → v0.3 upgrade.
#[tauri::command]
pub(crate) fn get_legacy_peers(state: State<AppState>) -> Vec<crate::state::LegacyPeerInfo> {
    state.legacy_peers.lock().unwrap().clone()
}

/// Dismiss the legacy-peer banner for the current run. The banner reappears
/// on next startup if any legacy peers are still present in known_peers,
/// so the user is reminded until they re-pair (or forget) every affected
/// peer.
#[tauri::command]
pub(crate) fn dismiss_legacy_peer_banner(state: State<AppState>) {
    state.legacy_peers.lock().unwrap().clear();
}

/// Returns true when the user has at least one manual peer AND none of those
/// manual peers are on a directly-reachable subnet. This is the gate for the
/// "having trouble connecting?" modal — show it only when we'd actually expect
/// remote/VPN connectivity to a manual peer. If a manual peer is on the local
/// subnet, "no peers online" just means peers are offline, not a connection
/// problem worth surfacing.
#[tauri::command]
pub(crate) fn expects_remote_manual_peers(state: State<AppState>) -> bool {
    let peers = state.known_peers.lock().unwrap();
    let manual: Vec<_> = peers.values().filter(|p| p.is_manual).collect();
    if manual.is_empty() {
        return false;
    }
    !manual.iter().any(|p| net_util::is_in_local_subnet(p.ip))
}

#[tauri::command]
pub(crate) fn get_listening_port(state: State<'_, AppState>) -> u16 {
    if let Some(transport) = state.transport.lock().unwrap().as_ref() {
        transport.local_addr().map(|a| a.port()).unwrap_or(4654)
    } else {
        4654
    }
}

#[tauri::command]
pub(crate) fn get_local_ip() -> String {
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

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

#[tauri::command]
pub(crate) async fn add_manual_peer(
    ip: String, // Can be IP or CIDR
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {

    // 1. Try parsing as CIDR
    if let Ok(net) = ip.parse::<IpNetwork>() {
        tracing::info!("Scanning range: {}", net);
        let ips: Vec<std::net::IpAddr> = net.iter().collect();

        // Scan in small batches with concurrency
        let batch_size = 50;
        for chunk in ips.chunks(batch_size) {
            let mut tasks = Vec::new();
            for ip_addr in chunk {
                 let s = (*state).clone();
                 let t = (*transport).clone();
                 let a = app_handle.clone();
                 let addr = *ip_addr;

                 // Skip own IP
                 if let Ok(local) = t.local_addr() {
                     if local.ip() == addr { continue; }
                 }

                 tasks.push(tauri::async_runtime::spawn(async move {
                     net_util::probe_ip(addr, 4654, s, t, a).await; // Fixed Port 4654
                 }));
            }
            futures::future::join_all(tasks).await;
        }
        Ok(())
    } else {
         // 2. Try parsing as normal IP or SocketAddr
        // If just IP, assume port 4654.
        let (addr, port) = if let Ok(sock) = ip.parse::<std::net::SocketAddr>() {
            (sock.ip(), sock.port())
        } else if let Ok(ip_addr) = ip.parse::<std::net::IpAddr>() {
            (ip_addr, 4654)
        } else {
             return Err("Invalid Format. Use IP, IP:PORT, or CIDR (e.g. 192.168.1.0/24)".to_string());
        };

        // For single IP, PROBE IT.
        net_util::probe_ip(addr, port, (*state).clone(), (*transport).clone(), app_handle).await;
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn leave_network(
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let local_id = state.local_device_id.lock().unwrap().clone();

    // 1. Broadcast "Self-Removal" to Network
    let removal_msg = Message::PeerRemoval(local_id.clone());
    let data = serde_json::to_vec(&removal_msg).unwrap_or_default();

    let peers_snapshot = state.get_peers();
    for (id, p) in peers_snapshot.iter() {
         if *id == local_id { continue; }

         let addr = std::net::SocketAddr::new(p.ip, p.port);
         let transport_clone = (*transport).clone();
         let data_vec = data.clone();

         tauri::async_runtime::spawn(async move {
             let _ = transport_clone.send_message(addr, &data_vec).await;
         });
    }

    // 2. Perform Factory Reset Locally
    let port = transport.local_addr().map(|a| a.port()).unwrap_or(0);
    perform_factory_reset(&app_handle, &state, port);

    Ok(())
}

#[tauri::command]
pub(crate) async fn delete_peer(
    peer_id: String,
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // 0. Broadcast Removal (Kick) to Network
    let removal_msg = Message::PeerRemoval(peer_id.clone());
    let data = serde_json::to_vec(&removal_msg).unwrap_or_default();

    // We can allow gossip_peer or manual iteration.
    // Manual iteration is safer to ensure it hits everyone including the target.
    let peers_snapshot = state.get_peers();
    for (id, p) in peers_snapshot.iter() {
         // Don't gossip to self (obv)
         if *id == state.local_device_id.lock().unwrap().clone() {
             continue;
         }

         let addr = std::net::SocketAddr::new(p.ip, p.port);
         let transport_clone = (*transport).clone();
         let data_vec = data.clone();

         tauri::async_runtime::spawn(async move {
             let _ = transport_clone.send_message(addr, &data_vec).await;
         });
    }

    // 1. Remove from Known Peers
    {
        let mut kp = state.known_peers.lock().unwrap();
        if kp.remove(&peer_id).is_some() {
            save_known_peers(&app_handle, &kp);
        }
    }

    // 2. Remove from Runtime Peers
    {
        let mut peers = state.peers.lock().unwrap();
        peers.remove(&peer_id);
    }

    // 3. Emit Removal
    let _ = app_handle.emit("peer-remove", &peer_id);

    Ok(())
}

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

#[tauri::command]
pub(crate) async fn retry_connection(
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // Clone inner values to own them for the async task
    let state_owned = (*state).clone();
    let transport_owned = (*transport).clone();
    let app_handle_clone = app_handle.clone();

    // Re-run the startup probe logic
    tauri::async_runtime::spawn(async move {
         let known_peers = {
             state_owned.known_peers.lock().unwrap().clone()
         };

         if !known_peers.is_empty() {
             tracing::info!("Retry Connection: Probing {} known peers...", known_peers.len());
             for (_id, peer) in known_peers {
                 let s = state_owned.clone();
                 let t = transport_owned.clone();
                 let a = app_handle_clone.clone();

                 tauri::async_runtime::spawn(async move {
                     net_util::probe_ip(peer.ip, peer.port, s, t, a).await;
                 });
             }
         } else {
             // If no known peers, maybe we should try scanning?
             // But for now, we only care about reconnecting to knowns.
             tracing::warn!("Retry Connection: No known peers to probe.");
         }
    });

    Ok(())
}
