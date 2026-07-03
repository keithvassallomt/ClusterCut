mod crypto;

pub(crate) use crypto::{
    derive_pair_subkeys, finish_spake2, fresh_pair_nonce, pair_aead_decrypt,
    pair_aead_encrypt, pairing_transcript, start_spake2, INITIATOR_KC_PLAINTEXT,
};

use crate::state::AppState;
use crate::transport::Transport;
use tauri::Emitter;

/// True if the pairing listener has tripped its global AEAD-failure
/// lockout and is refusing inbound pairing connections until manually
/// re-armed. See WIRE-PROTOCOL-0.3.1 §H1.
#[tauri::command]
pub(crate) fn is_pairing_locked_out(state: tauri::State<'_, AppState>) -> bool {
    state.is_pairing_locked_out()
}

/// Clear the pairing lockout and reset the failure counter. Invoked by the
/// frontend when the user explicitly re-arms via the lockout banner / modal.
#[tauri::command]
pub(crate) fn rearm_pairing(state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle) -> Result<(), String> {
    state.rearm_pairing();
    let _ = app_handle.emit("pairing-rearmed", ());
    tracing::info!("Pairing listener re-armed by user.");
    Ok(())
}

/// Read the user's "accept inbound pairing" flag. The pairing listener is
/// gated on this flag AND on `pairing_locked_out` — both must be clear for
/// inbound SPAKE to proceed. See issue #16.
#[tauri::command]
pub(crate) fn get_pairing_accept(state: tauri::State<'_, AppState>) -> bool {
    state.settings.lock().unwrap().pairing_accept_enabled
}

/// Write the user's "accept inbound pairing" flag, persist the change, and
/// emit `pairing-accept-changed` so any subscribed UI surface stays in sync.
/// Does NOT touch `pairing_locked_out` — abuse defence and user intent are
/// orthogonal. See issue #16.
#[tauri::command]
pub(crate) fn set_pairing_accept(
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

#[tauri::command]
pub(crate) async fn start_pairing(
    peer_id: String,
    pin: String,
    peer_addr: Option<String>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
) -> Result<(), String> {
    use crate::protocol::PairingMessage;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    // Two entry points: discovered peers (looked up by peer_id in the runtime
    // peers map) and manually-added remotes (caller passes the IP[:port]
    // directly, because no mDNS observation has populated the map). When
    // peer_addr is supplied, peer_id is informational only — the SPAKE2-
    // authenticated responder identity from T3 is the canonical id used for
    // storage below.
    let is_manual_pair = peer_addr.is_some();
    let (peer_addr, discovered_proto_version, discovered_hostname) = if let Some(addr_str) = peer_addr {
        let sock = if let Ok(sock) = addr_str.parse::<std::net::SocketAddr>() {
            sock
        } else if let Ok(ip) = addr_str.parse::<std::net::IpAddr>() {
            std::net::SocketAddr::new(ip, 4654)
        } else {
            return Err(format!("Invalid peer address: {}", addr_str));
        };
        // Add-Remote path: no mDNS data, so we can't pre-check the proto.
        // Fall through to the wire-level failure if the remote is incompatible.
        (sock, None, None)
    } else {
        let peers = state.get_peers();
        if let Some(peer) = peers.get(&peer_id) {
            (
                std::net::SocketAddr::new(peer.ip, peer.port),
                peer.protocol_version.clone(),
                Some(peer.hostname.clone()),
            )
        } else {
            return Err("Peer not found".to_string());
        }
    };

    crate::diagnostics::push_diagnostic(
        &*state,
        &app_handle,
        crate::diagnostics::DiagLevel::Minimal,
        "pairing",
        Some(peer_addr.to_string()),
        "Pairing started".to_string(),
    );

    // Pre-flight version check for mDNS-discovered peers. If the peer's
    // advertised proto is missing or below the floor this build can talk
    // to, emit `peer-incompatible` so the existing "Peer needs updating"
    // modal fires and abort before opening the TCP socket. A discovered
    // peer with no proto TXT is treated as incompatible (matches the
    // existing send-path semantics in `report_send_failure`). For the
    // manual Add-Remote path (no discovered proto), we fall through and
    // let the wire-level failure handle it — the user explicitly typed
    // the address and we have no advance signal.
    if !is_manual_pair {
        if !crate::net_util::is_protocol_compatible(discovered_proto_version.as_deref()) {
            let hostname = discovered_hostname.unwrap_or_else(|| peer_id.clone());
            tracing::warn!(
                "Refusing to pair with {} ({}): proto {:?} below floor.",
                hostname,
                peer_id,
                discovered_proto_version
            );
            let _ = app_handle.emit(
                "peer-incompatible",
                serde_json::json!({
                    "id": peer_id,
                    "hostname": hostname,
                }),
            );
            return Err("Peer protocol version is below the minimum compatible floor.".to_string());
        }
    }

    let local_id_raw = { state.local_device_id.lock().unwrap().clone() };
    let local_id = crate::protocol::truncate_device_id(&local_id_raw);

    let (spake_state, spake_msg_i) =
        start_spake2(&pin, &local_id, &peer_id).map_err(|e| e.to_string())?;

    // Per WIRE-PROTOCOL-0.3.1 §H6: the TCP socket is not opened until the
    // user has entered the PIN and pressed OK (i.e. until this function is
    // invoked) — so the responder's single-flight slot is only ever held
    // during the SPAKE2 + AEAD round trips, not during human input.
    let mut stream = crate::transport::pairing_connect(peer_addr)
        .await
        .map_err(|e| format!("Failed to connect to peer: {}", e))?;

    // T0 — opening SPAKE2 element. No identity bytes on the wire.
    let req = PairingMessage::PairRequest { spake_msg: spake_msg_i.clone() };
    crate::transport::write_pairing_frame(&mut stream, &req)
        .await
        .map_err(|e| format!("Failed to send PairRequest: {}", e))?;

    // T1 — answering SPAKE2 element from the responder.
    let spake_msg_r = match crate::transport::read_pairing_frame(&mut stream).await {
        Ok(PairingMessage::PairResponse { spake_msg }) => spake_msg,
        Ok(other) => {
            return Err(format!("Pairing protocol error: expected PairResponse, got {:?}", other));
        }
        Err(e) => {
            let _ = app_handle.emit("pairing-failed", "Pairing connection failed. Please try again.");
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Minimal,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed".to_string(),
            );
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Detailed,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed: Pairing connection failed. Please try again.".to_string(),
            );
            return Err(format!("Failed to read PairResponse: {}", e));
        }
    };

    // Finish SPAKE2 → shared 32-byte session key.
    let session_key = match finish_spake2(spake_state, &spake_msg_r) {
        Ok(k) => k,
        Err(e) => {
            tracing::error!("SPAKE2 finish failed (initiator): {}", e);
            let _ = app_handle.emit("pairing-failed", "Authentication failed. Check the PIN and try again.");
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Minimal,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed".to_string(),
            );
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Detailed,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed: Authentication failed. Check the PIN and try again.".to_string(),
            );
            return Err(e.to_string());
        }
    };
    if session_key.len() != 32 {
        return Err("Invalid SPAKE2 session key length".to_string());
    }

    // Derive role-distinct AEAD sub-keys from the SPAKE2 key + the role-
    // labelled transcript. Any wire-byte rewrite between T0 and T1 produces
    // a different transcript here than the responder reconstructed, the
    // sub-keys diverge, and the T3 ResponderId decrypt below fails closed.
    let transcript = pairing_transcript(&spake_msg_i, &spake_msg_r);
    let (k_i2r, k_r2i) = derive_pair_subkeys(&session_key, &transcript)
        .map_err(|e| format!("HKDF sub-key derivation failed: {}", e))?;
    tracing::info!("SPAKE2 complete (initiator); sending InitiatorKC (T2).");

    // T2 (wire 0.3.3) — explicit key-confirmation under k_i2r. The responder
    // refuses to send T3 (ResponderId) until this AEAD-verifies. Encrypting
    // the fixed KC_PLAINTEXT here is what proves to the responder that we
    // derived the same SPAKE2 key (i.e. we have the right PIN); a wrong-PIN
    // attacker can't forge a tag that decrypts under the responder's k_i2r.
    let nonce_kc = fresh_pair_nonce();
    let ciphertext_kc = pair_aead_encrypt(
        &k_i2r,
        &nonce_kc,
        INITIATOR_KC_PLAINTEXT,
    )
    .map_err(|e| format!("InitiatorKC AEAD encrypt failed: {}", e))?;
    let t2 = PairingMessage::InitiatorKC {
        nonce: nonce_kc.to_vec(),
        ciphertext: ciphertext_kc,
    };
    if let Err(e) = crate::transport::write_pairing_frame(&mut stream, &t2).await {
        let _ = app_handle.emit("pairing-failed", "Pairing connection failed. Please try again.");
        crate::diagnostics::push_diagnostic(
            &*state,
            &app_handle,
            crate::diagnostics::DiagLevel::Minimal,
            "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed".to_string(),
        );
        crate::diagnostics::push_diagnostic(
            &*state,
            &app_handle,
            crate::diagnostics::DiagLevel::Detailed,
            "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed: Pairing connection failed. Please try again.".to_string(),
        );
        return Err(format!("Failed to send InitiatorKC: {}", e));
    }
    tracing::info!("InitiatorKC sent (initiator); awaiting ResponderId (T3).");

    // T3 (wire 0.3.3) — responder's AEAD-wrapped identity (device_id + cert
    // fingerprint). Sent only after the responder verifies our T2 KC frame.
    //
    // If the read returns EOF here, the dominant cause is the responder
    // closing after its T2 AEAD verify failed — i.e. the PIN we sent didn't
    // match the PIN the responder loaded. A genuine connection drop would
    // also surface as EOF, so we phrase the user-visible message to cover
    // both without leaking which one it was. (Previously this was
    // "Pairing session expired", which sent debugging down a TCP-timeout
    // rabbit hole the first time this bug was observed.)
    let (nonce_r, ciphertext_r) = match crate::transport::read_pairing_frame(&mut stream).await {
        Ok(PairingMessage::ResponderId { nonce, ciphertext }) => (nonce, ciphertext),
        Ok(other) => {
            return Err(format!("Pairing protocol error: expected ResponderId, got {:?}", other));
        }
        Err(e) => {
            let _ = app_handle.emit("pairing-failed", "Failed to join network. The PIN may be incorrect.");
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Minimal,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed".to_string(),
            );
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Detailed,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed: Failed to join network. The PIN may be incorrect.".to_string(),
            );
            return Err(format!("Failed to read ResponderId: {}", e));
        }
    };
    let nonce_r_arr: [u8; 12] = nonce_r.as_slice().try_into().map_err(|_| {
        let _ = app_handle.emit("pairing-failed", "Pairing protocol error (bad nonce). Please try again.");
        crate::diagnostics::push_diagnostic(
            &*state,
            &app_handle,
            crate::diagnostics::DiagLevel::Minimal,
            "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed".to_string(),
        );
        crate::diagnostics::push_diagnostic(
            &*state,
            &app_handle,
            crate::diagnostics::DiagLevel::Detailed,
            "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed: Pairing protocol error (bad nonce). Please try again.".to_string(),
        );
        "ResponderId nonce must be 12 bytes".to_string()
    })?;
    let r_inner_bytes = match pair_aead_decrypt(&k_r2i, &nonce_r_arr, &ciphertext_r) {
        Ok(b) => b,
        Err(e) => {
            // Wrong PIN or active MITM. Generic UI message; no detail leaked.
            tracing::warn!("ResponderId AEAD decrypt failed (initiator): {}", e);
            let _ = app_handle.emit("pairing-failed", "Failed to join network. The PIN may be incorrect.");
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Minimal,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed".to_string(),
            );
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Detailed,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed: Failed to join network. The PIN may be incorrect.".to_string(),
            );
            return Err("ResponderId AEAD decrypt failed".to_string());
        }
    };
    let r_inner: crate::protocol::PairIdInner = serde_json::from_slice(&r_inner_bytes)
        .map_err(|e| format!("Malformed ResponderId inner payload: {}", e))?;
    let crate::protocol::PairIdInner { device_id: responder_device_id, fingerprint: responder_fingerprint } = r_inner;
    // Apply the same canonicalisation the responder uses on its T4 receive
    // path, so both sides key on identical bytes.
    let responder_device_id = crate::protocol::truncate_device_id(&responder_device_id);
    tracing::info!(
        "Authenticated responder identity (initiator). Pinning fingerprint for {}.",
        responder_device_id
    );

    // T4 (wire 0.3.3) — initiator's AEAD-wrapped identity. Build, encrypt, send.
    let local_fp = transport.local_fingerprint();
    let i_inner = crate::protocol::PairIdInner {
        device_id: local_id.clone(),
        fingerprint: local_fp,
    };
    let i_inner_bytes = serde_json::to_vec(&i_inner)
        .map_err(|e| format!("Failed to serialise InitiatorId inner: {}", e))?;
    let nonce_i = fresh_pair_nonce();
    let ciphertext_i = pair_aead_encrypt(&k_i2r, &nonce_i, &i_inner_bytes)
        .map_err(|e| format!("InitiatorId AEAD encrypt failed: {}", e))?;
    let t4 = PairingMessage::InitiatorId {
        nonce: nonce_i.to_vec(),
        ciphertext: ciphertext_i,
    };
    if let Err(e) = crate::transport::write_pairing_frame(&mut stream, &t4).await {
        let _ = app_handle.emit("pairing-failed", "Failed to complete pairing. Please try again.");
        crate::diagnostics::push_diagnostic(
            &*state,
            &app_handle,
            crate::diagnostics::DiagLevel::Minimal,
            "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed".to_string(),
        );
        crate::diagnostics::push_diagnostic(
            &*state,
            &app_handle,
            crate::diagnostics::DiagLevel::Detailed,
            "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed: Failed to complete pairing. Please try again.".to_string(),
        );
        return Err(format!("Failed to send InitiatorId: {}", e));
    }

    // Pin the responder's fingerprint locally NOW (after sending T4, before
    // the QUIC step that depends on it). Touching state in this order keeps
    // the pinning visible to a concurrent inbound mTLS verifier on this side.
    //
    // We key on `responder_device_id` (the SPAKE2-authenticated id from T3),
    // not the caller-supplied `peer_id`. For mDNS-discovered peers the two
    // are equal because mDNS announces the same device_id; for the manual
    // remote-add path `peer_id` is unknown ahead of time, so the authenticated
    // value is the only correct key. If a prior mDNS observation already
    // populated a runtime entry under a different key (or under the same key
    // with stale data), inherit its hostname; otherwise fall back to the IP.
    {
        let mut kp_lock = state.known_peers.lock().unwrap();
        let mut runtime_peers = state.peers.lock().unwrap();
        let prior = runtime_peers
            .get(&responder_device_id)
            .or_else(|| runtime_peers.get(&peer_id));
        let hostname = prior
            .map(|p| p.hostname.clone())
            .unwrap_or_else(|| format!("Peer ({})", peer_addr.ip()));
        let inherited_is_manual = prior.map(|p| p.is_manual).unwrap_or(false);
        let pinned = crate::peer::Peer {
            id: responder_device_id.clone(),
            ip: peer_addr.ip(),
            port: peer_addr.port(),
            hostname,
            last_seen: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            is_trusted: true,
            is_manual: is_manual_pair || inherited_is_manual,
            // network_name filled in once ClusterInfo arrives.
            network_name: None,
            signature: None,
            fingerprint: Some(responder_fingerprint.clone()),
            protocol_version: Some(crate::discovery::CLUSTERCUT_PROTOCOL_VERSION.to_string()),
        };
        runtime_peers.insert(responder_device_id.clone(), pinned.clone());
        kp_lock.insert(responder_device_id.clone(), pinned.clone());

        // Prune superseded records for this IP. A successful pair
        // authoritatively identifies the device now at `peer_addr`, so any
        // OTHER stored entry sharing this IP under a different id (a stale
        // old-device-id record from before the peer re-generated its id, a
        // `manual-<ip>` placeholder) is dead weight — and previously could
        // shadow this fresh pin during fingerprint lookup. Restrict to
        // local-subnet IPs, where each device has a unique address so a
        // same-IP/different-id entry is unambiguously stale; a NATed remote IP
        // can legitimately host several distinct peers, so leave those alone.
        let ip = peer_addr.ip();
        if crate::net_util::is_in_local_subnet(ip) {
            let stale: Vec<String> = kp_lock
                .iter()
                .filter(|(k, p)| **k != responder_device_id && p.ip == ip)
                .map(|(k, _)| k.clone())
                .collect();
            for k in &stale {
                kp_lock.remove(k);
                runtime_peers.remove(k);
                let _ = app_handle.emit("peer-remove", k);
                tracing::info!(
                    "Pruned stale same-IP peer {} at {} (superseded by pairing with {})",
                    k, ip, responder_device_id
                );
            }
        }

        crate::storage::save_known_peers(&app_handle, &kp_lock);
        let _ = app_handle.emit("peer-update", crate::peer::PeerView::from_peer(&pinned));
    }

    // T5 (wire 0.3.3) — wait for the responder to finish processing T4
    // (pinning our fingerprint) and close its side of the TCP socket.
    // Reading to EOF on a connection whose write half we've shut down gives
    // the initiator a deterministic "responder is ready to accept our QUIC"
    // signal, which is what unblocks the post-pairing mTLS handshake.
    let _ = stream.shutdown().await;
    let mut sink = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut sink),
    )
    .await;

    // Post-pairing cluster bootstrap over QUIC/mTLS. The pairing channel did
    // one job (pin fingerprints); cluster_id / known_peers / network_name now
    // ride the already-authenticated QUIC channel.
    let (info_tx, info_rx) = tokio::sync::oneshot::channel::<crate::protocol::ClusterInfo>();
    {
        let mut slot = state.pending_cluster_info.lock().unwrap();
        *slot = Some(info_tx);
    }
    let req_bytes = serde_json::to_vec(&crate::protocol::Message::ClusterInfoRequest)
        .map_err(|e| format!("Failed to serialise ClusterInfoRequest: {}", e))?;
    if let Err(e) = transport.send_message(peer_addr, &req_bytes).await {
        // Clear the slot so a stray ClusterInfo doesn't sit in it forever.
        let _ = state.pending_cluster_info.lock().unwrap().take();
        let _ = app_handle.emit("pairing-failed", "Failed to fetch cluster info after pairing. Please try again.");
        crate::diagnostics::push_diagnostic(
            &*state,
            &app_handle,
            crate::diagnostics::DiagLevel::Minimal,
            "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed".to_string(),
        );
        crate::diagnostics::push_diagnostic(
            &*state,
            &app_handle,
            crate::diagnostics::DiagLevel::Detailed,
            "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed: Failed to fetch cluster info after pairing. Please try again.".to_string(),
        );
        return Err(format!("Failed to send ClusterInfoRequest: {}", e));
    }
    let cluster_info = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        info_rx,
    )
    .await
    {
        Ok(Ok(info)) => info,
        _ => {
            let _ = state.pending_cluster_info.lock().unwrap().take();
            let _ = app_handle.emit("pairing-failed", "Timed out waiting for cluster info. Please try again.");
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Minimal,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed".to_string(),
            );
            crate::diagnostics::push_diagnostic(
                &*state,
                &app_handle,
                crate::diagnostics::DiagLevel::Detailed,
                "pairing",
                Some(peer_addr.to_string()),
                "Pairing failed: Timed out waiting for cluster info. Please try again.".to_string(),
            );
            return Err("ClusterInfo response timed out".to_string());
        }
    };

    let crate::protocol::ClusterInfo {
        cluster_id,
        known_peers,
        network_name,
        network_name_version,
        network_name_origin,
        cluster_mode: responder_cluster_mode,
    } = cluster_info;
    tracing::info!("Joined Network: {} (cluster {})", network_name, cluster_id);
    {
        let mut cid = state.cluster_id.lock().unwrap();
        *cid = cluster_id.clone();
        crate::storage::save_cluster_id(&app_handle, &cluster_id);

        let mut nn = state.network_name.lock().unwrap();
        *nn = network_name.clone();
        crate::storage::save_network_name(&app_handle, &network_name);
        drop(nn);

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
    }

    let local_quic_port = transport.local_addr().map(|a| a.port()).unwrap_or(0);
    if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
        let _ = discovery.register(&local_id, &network_name, local_quic_port);
    }

    {
        let mut kp_lock = state.known_peers.lock().unwrap();
        let mut runtime_peers = state.peers.lock().unwrap();
        for peer in known_peers {
            // The cluster's view of the responder shouldn't clobber the
            // local pinned record we just wrote (which carries our pinned
            // fingerprint and any is_manual flag).
            if peer.id == responder_device_id {
                continue;
            }
            // The responder added us during T4 and includes our own record
            // in its known_peers snapshot — but from the responder's vantage
            // point we live at whatever source IP they saw (e.g. a WireGuard
            // tunnel address), with a placeholder hostname. Re-importing that
            // would surface us as a peer of ourselves in the UI. Always drop
            // any entry matching our local device_id.
            if peer.id == local_id {
                continue;
            }
            kp_lock.insert(peer.id.clone(), peer.clone());
            runtime_peers.insert(peer.id.clone(), peer.clone());
            let _ = app_handle.emit("peer-update", crate::peer::PeerView::from_peer(&peer));
        }
        // Tag the responder's record with the cluster's network_name now
        // that we have it.
        if let Some(peer) = runtime_peers.get_mut(&responder_device_id) {
            peer.network_name = Some(network_name.clone());
            kp_lock.insert(responder_device_id.clone(), peer.clone());
            let _ = app_handle.emit("peer-update", crate::peer::PeerView::from_peer(&*peer));
        }
        crate::storage::save_known_peers(&app_handle, &kp_lock);
    }

    // Provisioned-cluster PIN convergence. In a provisioned cluster every
    // device shares one PIN, but the join handshake never carried it — so a
    // joiner kept its own (usually auto-generated) PIN and later devices
    // couldn't pair with it using the admin's PIN. We gate on the *cluster's*
    // mode (reported by the responder in ClusterInfo), NOT the joiner's local
    // mode, because a joiner is typically still in auto mode when it pairs in.
    //
    // The joiner already typed the cluster PIN to complete SPAKE2 above (it IS
    // the responder's == the cluster's PIN), so we adopt that value without
    // putting the PIN on the wire. Adopting also requires flipping the joiner
    // into provisioned mode: otherwise the next launch's
    // `establish_network_pin("auto")` would delete the adopted PIN and generate
    // a fresh ephemeral one, breaking the cluster again on restart.
    if crate::storage::should_adopt_cluster_pin(&responder_cluster_mode) {
        let pin_changed = {
            let mut np = state.network_pin.lock().unwrap();
            if *np != pin {
                *np = pin.clone();
                true
            } else {
                false
            }
        };
        let settings_snapshot = {
            let mut s = state.settings.lock().unwrap();
            let mode_changed = s.cluster_mode != "provisioned";
            s.cluster_mode = "provisioned".to_string();
            if mode_changed || pin_changed {
                Some(s.clone())
            } else {
                None
            }
        };
        if pin_changed {
            crate::storage::save_network_pin(&app_handle, &pin);
        }
        if let Some(snapshot) = settings_snapshot {
            crate::storage::save_settings(&app_handle, &snapshot);
            tracing::info!("Joined provisioned cluster: adopted shared PIN and switched to provisioned mode.");
            // Refresh the UI: `network-update` re-fetches the displayed PIN,
            // `settings-changed` updates the mode toggle in Settings.
            let _ = app_handle.emit("network-update", ());
            let _ = app_handle.emit("settings-changed", snapshot);
        }
    }

    // Signal pairing completion to the UI. Distinct from `peer-update`
    // (which also fires on mDNS rediscovery and would race the PIN dialog).
    let _ = app_handle.emit("pairing-success", &responder_device_id);
    crate::diagnostics::push_diagnostic(
        &*state,
        &app_handle,
        crate::diagnostics::DiagLevel::Minimal,
        "pairing",
        Some(responder_device_id.clone()),
        "Pairing succeeded".to_string(),
    );
    Ok(())
}

/// Log every pairing-channel failure with a single generic message at WARN
/// level when `pairing_debug_logs` is off — per WIRE-PROTOCOL-0.3.1 §H7, the
/// log channel must not leak whether a failure was a wrong-PIN attempt vs.
/// any other framing/decrypt error. When the user toggles the debug switch
/// on, the same call site emits the underlying diagnostic.
fn log_pairing_failure(state: &AppState, peer_addr: std::net::SocketAddr, detail: &str) {
    let verbose = state
        .settings
        .lock()
        .map(|s| s.pairing_debug_logs)
        .unwrap_or(false);
    if verbose {
        tracing::warn!("Pairing failed from {}: {}", peer_addr, detail);
    } else {
        tracing::warn!("Pairing failed from {}.", peer_addr);
    }
}

/// Treat an AEAD-decrypt failure as a brute-force attempt: bump the global
/// counter, and if it crosses `PAIRING_FAILURE_LOCKOUT_THRESHOLD`, lock the
/// pairing listener and surface an urgent, user-actionable notification +
/// frontend event. See WIRE-PROTOCOL-0.3.1 §H1.
fn record_pairing_aead_failure(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    peer_addr: std::net::SocketAddr,
    detail: &str,
) {
    log_pairing_failure(state, peer_addr, detail);
    if state.record_pairing_failure() {
        tracing::error!(
            "Pairing listener LOCKED OUT after {} AEAD failures — user must re-arm via the UI.",
            crate::state::PAIRING_FAILURE_LOCKOUT_THRESHOLD,
        );
        let _ = app_handle.emit("pairing-locked-out", ());
        crate::diagnostics::push_diagnostic(
            state,
            app_handle,
            crate::diagnostics::DiagLevel::Minimal,
            "pairing",
            None,
            "Pairing listener locked out (too many failures)".to_string(),
        );
        // Urgent OS-level notification so the user actually sees the lockout
        // rather than only spotting it the next time they open Settings.
        crate::send_notification(
            app_handle,
            "ClusterCut pairing locked",
            "Too many failed PIN attempts. Pairing is paused — open ClusterCut to re-enable it.",
            true, // urgent
            None,
            "pairing",
            crate::NotificationPayload::None,
        );
    }
}

/// Responder side of the TCP pairing flow (wire 0.3.3).
///
/// Drives T0 (PairRequest) → T1 (PairResponse) → T2 (InitiatorKC, AEAD) →
/// T3 (ResponderId, AEAD) → T4 (InitiatorId, AEAD) to completion. After
/// T4 decrypts cleanly, the responder pins the initiator's fingerprint
/// and gossips the new peer to the rest of the cluster. The TCP socket
/// then closes at T5 — the initiator uses the close as its signal to
/// open QUIC for the post-pairing `ClusterInfo` exchange.
pub(crate) async fn handle_pairing_connection(
    mut stream: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    state: AppState,
    app_handle: tauri::AppHandle,
    transport: Transport,
) {
    use crate::protocol::PairingMessage;

    crate::diagnostics::push_diagnostic(
        &state,
        &app_handle,
        crate::diagnostics::DiagLevel::Minimal,
        "pairing",
        Some(peer_addr.to_string()),
        "Pairing started".to_string(),
    );

    // T0 — opening SPAKE2 element from the initiator. No identity bytes.
    let spake_msg_i = match crate::transport::read_pairing_frame(&mut stream).await {
        Ok(PairingMessage::PairRequest { spake_msg }) => spake_msg,
        Ok(other) => {
            let d = format!("expected PairRequest, got {:?}", other);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
        Err(e) => {
            let d = format!("read PairRequest failed: {}", e);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
    };
    tracing::info!("Received PairRequest from {}; running SPAKE2.", peer_addr);

    // T1 — responder's SPAKE2 element. The PIN comes from local state.
    let local_id_raw = state.local_device_id.lock().unwrap().clone();
    let local_id = crate::protocol::truncate_device_id(&local_id_raw);
    let pin = state.network_pin.lock().unwrap().clone();
    let (spake_state, spake_msg_r) = match start_spake2(&pin, &local_id, "initiator") {
        Ok(v) => v,
        Err(e) => {
            let d = format!("SPAKE2 init error: {}", e);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
    };
    let resp = PairingMessage::PairResponse { spake_msg: spake_msg_r.clone() };
    if let Err(e) = crate::transport::write_pairing_frame(&mut stream, &resp).await {
        let d = format!("send PairResponse failed: {}", e);
        log_pairing_failure(&state, peer_addr, &d);
        crate::diagnostics::push_diagnostic(
            &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
            Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
        );
        return;
    }

    // Finish SPAKE2 → shared 32-byte session key.
    let session_key = match finish_spake2(spake_state, &spake_msg_i) {
        Ok(k) => k,
        Err(e) => {
            // SPAKE2.finish() doesn't actually fail on PIN mismatch — wrong
            // PINs still produce a (different) 32-byte key. So this branch
            // is more about malformed inbound bytes. Treat as a generic
            // pairing failure (counter not bumped — only AEAD-tag failures
            // count toward lockout per §H1).
            let d = format!("SPAKE2 finish failed: {}", e);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
    };
    if session_key.len() != 32 {
        log_pairing_failure(&state, peer_addr, "SPAKE2 produced wrong key length");
        crate::diagnostics::push_diagnostic(
            &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed (responder): SPAKE2 produced wrong key length".to_string(),
        );
        return;
    }
    let transcript = pairing_transcript(&spake_msg_i, &spake_msg_r);
    let (k_i2r, k_r2i) = match derive_pair_subkeys(&session_key, &transcript) {
        Ok(pair) => pair,
        Err(e) => {
            let d = format!("HKDF derive failed: {}", e);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
    };
    tracing::info!(
        "SPAKE2 complete (responder) for {}; awaiting InitiatorKC (T2).",
        peer_addr
    );

    // T2 (wire 0.3.3) — initiator's key-confirmation frame. Must AEAD-verify
    // under our k_i2r before we reveal any encrypted identity material.
    // A wrong-PIN attacker can't produce a tag the responder will accept;
    // tag failures, malformed nonces, and plaintext mismatches all count
    // toward the H1 lockout, the same way a wrong T3 InitiatorId did in
    // 0.3.1.
    let (kc_nonce_vec, kc_ciphertext) = match crate::transport::read_pairing_frame(&mut stream).await {
        Ok(PairingMessage::InitiatorKC { nonce, ciphertext }) => (nonce, ciphertext),
        Ok(other) => {
            // Wrong variant — almost certainly a 0.3.1 client sending its old
            // `InitiatorId` at the T2 slot. Don't bump the AEAD counter, just
            // log + close. The pre-flight version check in start_pairing will
            // catch this for the initiator side; here we just hang up cleanly.
            let d = format!("expected InitiatorKC, got {:?}", other);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
        Err(e) => {
            let d = format!("read InitiatorKC failed: {}", e);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
    };
    let kc_nonce_arr: [u8; 12] = match kc_nonce_vec.as_slice().try_into() {
        Ok(arr) => arr,
        Err(_) => {
            // Malformed nonce — treat as a tampered/garbage frame and
            // count it toward the lockout, same as any AEAD failure.
            record_pairing_aead_failure(&state, &app_handle, peer_addr, "InitiatorKC nonce length");
            return;
        }
    };
    match pair_aead_decrypt(&k_i2r, &kc_nonce_arr, &kc_ciphertext) {
        Ok(plaintext) => {
            // Defence in depth: also require the plaintext byte string match,
            // so a future variant of the wire that re-uses the InitiatorKC
            // shape can't be replayed against a 0.3.3 responder.
            if plaintext.as_slice() != INITIATOR_KC_PLAINTEXT {
                record_pairing_aead_failure(
                    &state,
                    &app_handle,
                    peer_addr,
                    "InitiatorKC plaintext mismatch",
                );
                return;
            }
        }
        Err(e) => {
            // The big one: wrong PIN or active MITM forging T2. Counter++.
            //
            // Capture the byte-level form of the PIN this responder plugged
            // into SPAKE2 to the in-memory diagnostics channel at Debug level.
            // Combined with the initiator-side trim boundary, this is what
            // diagnoses an invisible-whitespace or encoding-divergence cause
            // directly instead of by elimination. This material is PIN secret
            // and so it goes ONLY to the never-persisted in-memory channel
            // (never to the `tracing` file log), surfaced only when the user
            // views the diagnostics panel at the Debug level.
            crate::diagnostics::push_diagnostic(
                &state,
                &app_handle,
                crate::diagnostics::DiagLevel::Debug,
                "pairing",
                Some(peer_addr.to_string()),
                format!(
                    "Responder PIN at T2-AEAD-failure: len={} bytes={:02x?}",
                    pin.len(),
                    pin.as_bytes()
                ),
            );
            record_pairing_aead_failure(
                &state,
                &app_handle,
                peer_addr,
                &format!("InitiatorKC AEAD decrypt failed: {}", e),
            );
            return;
        }
    }
    tracing::info!(
        "InitiatorKC verified for {}; sending ResponderId (T3).",
        peer_addr
    );

    // Refuse to advance to T3 if we have no cluster identity to bind to.
    // Responding here would leak a valid ResponderId for a half-built
    // cluster; better to abort early.
    if state.cluster_id.lock().unwrap().is_empty() {
        log_pairing_failure(&state, peer_addr, "responder has no cluster_id");
        crate::diagnostics::push_diagnostic(
            &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
            Some(peer_addr.to_string()),
            "Pairing failed (responder): responder has no cluster_id".to_string(),
        );
        return;
    }

    // T3 (wire 0.3.3) — responder's AEAD-wrapped identity, decryptable by
    // the initiator only if it derived the same SPAKE2 key (i.e. correct
    // PIN). Sent only after T2 InitiatorKC has been verified.
    let r_inner = crate::protocol::PairIdInner {
        device_id: local_id.clone(),
        fingerprint: transport.local_fingerprint(),
    };
    let r_inner_bytes = match serde_json::to_vec(&r_inner) {
        Ok(b) => b,
        Err(e) => {
            let d = format!("serialise ResponderId failed: {}", e);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
    };
    let nonce_r = fresh_pair_nonce();
    let ciphertext_r = match pair_aead_encrypt(&k_r2i, &nonce_r, &r_inner_bytes) {
        Ok(ct) => ct,
        Err(e) => {
            let d = format!("ResponderId AEAD encrypt failed: {}", e);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
    };
    let t3 = PairingMessage::ResponderId {
        nonce: nonce_r.to_vec(),
        ciphertext: ciphertext_r,
    };
    if let Err(e) = crate::transport::write_pairing_frame(&mut stream, &t3).await {
        let d = format!("send ResponderId failed: {}", e);
        log_pairing_failure(&state, peer_addr, &d);
        crate::diagnostics::push_diagnostic(
            &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
            Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
        );
        return;
    }

    // T4 (wire 0.3.3) — initiator's AEAD-wrapped identity.
    let (nonce_i_vec, ciphertext_i) = match crate::transport::read_pairing_frame(&mut stream).await {
        Ok(PairingMessage::InitiatorId { nonce, ciphertext }) => (nonce, ciphertext),
        Ok(other) => {
            let d = format!("expected InitiatorId, got {:?}", other);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
        Err(e) => {
            let d = format!("read InitiatorId failed: {}", e);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
    };
    let nonce_i_arr: [u8; 12] = match nonce_i_vec.as_slice().try_into() {
        Ok(arr) => arr,
        Err(_) => {
            // Malformed nonce — treat as a tampered/garbage frame and
            // count it toward the lockout, same as any AEAD failure.
            record_pairing_aead_failure(&state, &app_handle, peer_addr, "InitiatorId nonce length");
            return;
        }
    };
    let i_inner_bytes = match pair_aead_decrypt(&k_i2r, &nonce_i_arr, &ciphertext_i) {
        Ok(b) => b,
        Err(e) => {
            // The big one: AEAD-tag verify failed. Either the PIN was wrong
            // (online brute force) or an active MITM tried to forge T4. Bump
            // the global lockout counter — see §H1.
            record_pairing_aead_failure(&state, &app_handle, peer_addr, &format!("InitiatorId AEAD decrypt failed: {}", e));
            return;
        }
    };
    let i_inner: crate::protocol::PairIdInner = match serde_json::from_slice(&i_inner_bytes) {
        Ok(v) => v,
        Err(e) => {
            let d = format!("malformed InitiatorId inner: {}", e);
            log_pairing_failure(&state, peer_addr, &d);
            crate::diagnostics::push_diagnostic(
                &state, &app_handle, crate::diagnostics::DiagLevel::Detailed, "pairing",
                Some(peer_addr.to_string()), format!("Pairing failed (responder): {}", d),
            );
            return;
        }
    };
    let crate::protocol::PairIdInner {
        device_id: initiator_device_id,
        fingerprint: initiator_fingerprint,
    } = i_inner;

    // Apply truncation defensively on the receive side too — the spec
    // requires both ends to apply the same canonicalisation so the pinned
    // identifier matches what the initiator believes its device_id to be.
    let initiator_device_id = crate::protocol::truncate_device_id(&initiator_device_id);
    tracing::info!(
        "Authenticated initiator identity ({}) from {}; pinning fingerprint.",
        initiator_device_id,
        peer_addr
    );

    // Insert / refresh the peer record with the pinned fingerprint. Pull
    // the hostname from any prior mDNS observation; otherwise placeholder.
    let prior_hostname = {
        let runtime_peers = state.peers.lock().unwrap();
        runtime_peers
            .get(&initiator_device_id)
            .map(|p| p.hostname.clone())
            .or_else(|| {
                state
                    .known_peers
                    .lock()
                    .unwrap()
                    .get(&initiator_device_id)
                    .map(|p| p.hostname.clone())
            })
    };
    let network_name = state.network_name.lock().unwrap().clone();
    let pinned = crate::peer::Peer {
        id: initiator_device_id.clone(),
        ip: peer_addr.ip(),
        port: peer_addr.port(),
        hostname: prior_hostname.unwrap_or_else(|| format!("Peer ({})", peer_addr.ip())),
        last_seen: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        is_trusted: true,
        is_manual: false,
        network_name: Some(network_name.clone()),
        signature: None,
        fingerprint: Some(initiator_fingerprint),
        protocol_version: Some(crate::discovery::CLUSTERCUT_PROTOCOL_VERSION.to_string()),
    };
    {
        let mut kp_lock = state.known_peers.lock().unwrap();
        kp_lock.insert(initiator_device_id.clone(), pinned.clone());
        crate::storage::save_known_peers(&app_handle, &kp_lock);
    }
    state.add_peer(pinned.clone());
    let _ = app_handle.emit("peer-update", crate::peer::PeerView::from_peer(&pinned));

    // Gossip the new peer to the rest of the cluster ONLY after T4 succeeds —
    // existing mTLS peers need the new fingerprint to accept its inbound
    // connections.
    crate::net_util::gossip_peer(&pinned, &state, &transport, Some(peer_addr));

    crate::diagnostics::push_diagnostic(
        &state,
        &app_handle,
        crate::diagnostics::DiagLevel::Minimal,
        "pairing",
        Some(peer_addr.to_string()),
        "Pairing succeeded".to_string(),
    );

    // T5 (wire 0.3.3) — drop the stream. The kernel closes the TCP
    // connection, which the initiator reads as the "responder is ready
    // for QUIC" signal.
    drop(stream);
}
