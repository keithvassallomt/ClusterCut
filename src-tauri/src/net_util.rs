use local_ip_address::list_afinet_netifas;
use tauri::Emitter;

use crate::peer::Peer;
use crate::protocol::Message;
use crate::state::AppState;
use crate::storage::save_known_peers;
use crate::transport::Transport;

/// Parse a "MAJOR.MINOR.PATCH" string. Accepts a trailing `-prerelease`
/// segment (digits-only prefix is taken from the patch component, so e.g.
/// `0.3.0-alpha.1` parses as (0, 3, 0)). Returns `None` for unparseable
/// input — caller treats that as "incompatible" so older peers that don't
/// advertise a `proto` property are flagged.
fn parse_protocol_version(s: &str) -> Option<(u32, u32, u32)> {
    let leading_digits = |segment: &str| -> Option<u32> {
        let head: String = segment.chars().take_while(|c| c.is_ascii_digit()).collect();
        head.parse().ok()
    };
    let mut parts = s.split('.');
    let major = leading_digits(parts.next()?)?;
    let minor = leading_digits(parts.next()?)?;
    let patch = parts.next().and_then(leading_digits).unwrap_or(0);
    Some((major, minor, patch))
}

/// True if the peer's advertised `proto` version is at least the minimum
/// this build can talk to. Returns false for peers that don't advertise
/// the property at all (older builds).
pub(crate) fn is_protocol_compatible(version: Option<&str>) -> bool {
    let Some(v) = version else { return false };
    // 0.3.3 break: the pairing-channel wire format requires the new T2
    // `InitiatorKC` frame; 0.3.1 initiators don't send it and 0.3.1
    // responders don't read it. Bumping the floor surfaces 0.3.1 peers
    // as incompatible in the same UI flow used for the 0.2.x → 0.3.0
    // and 0.3.0 → 0.3.1 breaks.
    parse_protocol_version(v).map_or(false, |parsed| parsed >= (0, 3, 3))
}

/// True if a peer advertising `version` understands `Message::ClusterName`
/// (introduced in wire 0.3.4). Peers without a `proto` property, or older,
/// are not sent the message and keep per-device-name behavior.
pub(crate) fn supports_cluster_name(version: Option<&str>) -> bool {
    let Some(v) = version else { return false };
    parse_protocol_version(v).map_or(false, |parsed| parsed >= (0, 3, 4))
}

pub(crate) fn gossip_peer(
    new_peer: &Peer,
    state: &AppState,
    transport: &Transport,
    exclude_addr: Option<std::net::SocketAddr>,
) {
    let peers = state.get_peers();
    let msg = Message::PeerDiscovery(new_peer.clone());
    let data = serde_json::to_vec(&msg).unwrap_or_default();

    for p in peers.values() {
        // Don't gossip to the new peer itself
        if p.id == new_peer.id {
            continue;
        }
        let addr = std::net::SocketAddr::new(p.ip, p.port);
        if Some(addr) == exclude_addr {
            continue;
        }

        let transport_clone = transport.clone();
        let data_vec = data.clone();

        tauri::async_runtime::spawn(async move {
            if let Err(e) = transport_clone.send_message(addr, &data_vec).await {
                tracing::error!("Failed to gossip peer to {}: {}", addr, e);
            }
        });
    }
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

// Helper to probe a specific IP/Port
/// Probe `ip:port` with a `PeerDiscovery` carrying our own info. Returns
/// `true` iff the QUIC send succeeded. `notify: false` suppresses all probe
/// notifications — background probes (anti-entropy, netmon recovery, focus
/// kicks) would otherwise toast "Connection Failed" for every offline peer
/// on every pass.
pub(crate) async fn probe_ip(
    ip: std::net::IpAddr,
    port: u16,
    state: AppState,
    transport: Transport,
    app_handle: tauri::AppHandle,
    notify: bool,
) -> bool {
    let addr = std::net::SocketAddr::new(ip, port);

    // Attempt connection loop (simple probe)
    // Transport::send_message initiates a connection.
    // We send a lightweight "PeerDiscovery" with our own info.
    // If it succeeds, we add them as Untrusted.

    // Wait... if we send 'PeerDiscovery', they will receive it and add US.
    // But how do we add THEM?
    // We don't get a response from send_message other than Ok/Err.
    // We need a request/response.
    // Or we rely on them reacting to our PeerDiscovery by connecting back?
    // Let's implement a 'Hello' ping.

    let local_id = state.local_device_id.lock().unwrap().clone();
    let hostname = hostname::get().map(|h| h.to_string_lossy().to_string()).unwrap_or("Unknown".to_string());
    let network_name = state.network_name.lock().unwrap().clone();

    // Send OUR info so they can add us.
    let my_peer = Peer {
        id: local_id.clone(),
        ip: transport.local_addr().unwrap().ip(),
        port: transport.local_addr().unwrap().port(),
        hostname,
        last_seen: 0,
        is_trusted: false, // We don't know if we are trusted yet
        is_manual: true,
        network_name: Some(network_name),
        signature: None,
        fingerprint: Some(transport.local_fingerprint()),
        protocol_version: Some(crate::discovery::CLUSTERCUT_PROTOCOL_VERSION.to_string()),
    };

    let msg = Message::PeerDiscovery(my_peer);
    let _data = serde_json::to_vec(&msg).unwrap_or_default();

            tracing::debug!("Probing {}...", addr);

            // Send Peer Discovery via QUIC/UDP
            let data_vec = _data.clone();
            let transport_clone = transport.clone();

            // We use a small timeout for the send operation
            let send_future = async move {
                 transport_clone.send_message(addr, &data_vec).await
            };

            match tokio::time::timeout(std::time::Duration::from_millis(2000), send_future).await {
                Ok(Ok(())) => {
                    tracing::debug!("Probe to {} SUCCESS (Packet Sent)", addr);

                   // NOTIFY SUCCESS (Only if not startup)
                   if notify && state.should_notify() {
                       crate::send_notification(&app_handle, "Connection Established", &format!("Successfully contacted {}.", ip), false, None, "devices", crate::NotificationPayload::None);
                   }

                   // We successfully sent the packet.
                   // Since UDP is connectionless, this doesn't guarantee they received it,
                   // BUT `send_message` in our Transport uses `open_bi` which implies a handshake.
                   // If handshake succeeds, they are there.

                   // Add to manual peers list
                     let mut peers = state.known_peers.lock().unwrap();
                     let id = format!("manual-{}", ip);
                     if !peers.contains_key(&id) {
                         let peer = Peer {
                             id: id.clone(),
                             ip,
                             port,
                             hostname: format!("Manual ({})", ip),
                             last_seen: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                             is_trusted: false,
                             is_manual: true,
                             network_name: None,
                             signature: None,
                             fingerprint: None,
                             protocol_version: None,
                         };
                         peers.insert(id.clone(), peer.clone());
                         let _ = app_handle.emit("peer-update", crate::peer::PeerView::from_peer(&peer));
                         save_known_peers(&app_handle, &peers); // PERSIST manual placeholder

                          let notifications = state.settings.lock().unwrap().notifications.clone();
                          if notifications.device_join {
                             // Check startup timer
                             if notify && state.should_notify() {
                                 tracing::info!("[Notification] Triggering 'Device Joined' for manual peer: {}", peer.hostname);
                                 crate::send_notification(&app_handle, "Device Joined", &format!("Found manual peer: {}", peer.hostname), false, Some(1), "devices", crate::NotificationPayload::None);
                             } else {
                                 tracing::debug!("[Notification] Device join (manual) notification suppressed by startup timer for peer: {}", peer.hostname);
                             }
                          }
                     } else {
                         // Already exists
                         tracing::debug!("Manual peer {} already exists.", id);
                         // Still notify success to confirm connectivity (if not startup)
                         if notify && state.should_notify() {
                             crate::send_notification(&app_handle, "Connection Verified", &format!("Connection to {} is active.", ip), false, None, "devices", crate::NotificationPayload::None);
                         }
                     }
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

/// Description sentinel embedded in the netsh rule. Bumped whenever the rule
/// shape changes so existing too-narrow rules from older versions get
/// force-replaced via UAC on first launch instead of silently passing the
/// "looks correct" check. v0.3 widens to include explicit outbound + scope;
/// v0.3.1 adds TCP/4654 inbound + outbound for the new plaintext-TCP
/// pairing channel that runs alongside QUIC on the same port. Bump this
/// sentinel only when the firewall rule's port/protocol/scope shape
/// changes — NOT for every app or wire-protocol bump. Wire 0.3.3 reuses
/// the same TCP/4654 + UDP/4654 pair, so the sentinel stays at v0.3.1.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const FIREWALL_RULE_SENTINEL: &str = "ClusterCut sync v0.3.1 (UDP+TCP pair)";

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn configure_windows_firewall() {
    // Check if the firewall rule already exists (does not require elevation).
    // Existing rules from 0.2.x are inbound-only with no description, which
    // surfaces in the wild as "Windows accepts QUIC packets but its outbound
    // replies are dropped on restrictive Defender / enterprise policy".
    // The sentinel below lets us detect a too-narrow legacy rule and force a
    // single re-prompt to widen it.
    let check = std::process::Command::new("netsh")
        .args(["advfirewall", "firewall", "show", "rule", "name=ClusterCut", "verbose"])
        .output();

    match check {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if output.status.success()
                && stdout.contains("UDP")
                && stdout.contains("TCP")
                && stdout.contains("4654")
                && stdout.contains(FIREWALL_RULE_SENTINEL)
            {
                tracing::info!(
                    "Windows Firewall rule 'ClusterCut' already up to date ({}), skipping.",
                    FIREWALL_RULE_SENTINEL
                );
                return;
            }
            tracing::info!(
                "Firewall rule missing, missing the {} sentinel, or misconfigured — will create/update (requesting UAC)...",
                FIREWALL_RULE_SENTINEL
            );
        }
        Err(e) => {
            tracing::warn!("Could not check firewall rule: {}, will attempt to create it", e);
        }
    }

    // Five netsh calls in one elevated session:
    //   1. Delete any existing "ClusterCut" rules so we don't end up with
    //      duplicate / stale rules layered on top of each other.
    //   2. Add inbound UDP/4654 (any source IP, edge traversal allowed) —
    //      QUIC steady-state traffic.
    //   3. Add outbound UDP/4654. Windows defaults to "allow outbound" but
    //      Defender / enterprise / "Block all" private-profile configs do
    //      override that — without an explicit allow rule, QUIC handshake
    //      ACKs from this machine get dropped, which surfaces as "Linux
    //      can't reach Windows but Windows can reach Linux" (the Linux
    //      side sees its initial-Initial succeed but never the response).
    //   4. Add inbound TCP/4654 — the plaintext-TCP pairing channel
    //      (same numeric port, different protocol). Without this the
    //      initiator's pairing connect times out silently on Windows.
    //   5. Add outbound TCP/4654 for symmetry with the UDP outbound rule.
    //
    // The description on every rule is the FIREWALL_RULE_SENTINEL so the
    // pre-flight check above can detect it.
    let cmd = format!(
        "netsh advfirewall firewall delete rule name=\\\"ClusterCut\\\" 2>$null; \
         netsh advfirewall firewall add rule name=\\\"ClusterCut\\\" dir=in action=allow protocol=UDP localport=4654 remoteip=any profile=any edge=yes description=\\\"{sentinel}\\\"; \
         netsh advfirewall firewall add rule name=\\\"ClusterCut\\\" dir=out action=allow protocol=UDP localport=4654 remoteip=any profile=any description=\\\"{sentinel}\\\"; \
         netsh advfirewall firewall add rule name=\\\"ClusterCut\\\" dir=in action=allow protocol=TCP localport=4654 remoteip=any profile=any edge=yes description=\\\"{sentinel}\\\"; \
         netsh advfirewall firewall add rule name=\\\"ClusterCut\\\" dir=out action=allow protocol=TCP localport=4654 remoteip=any profile=any description=\\\"{sentinel}\\\"",
        sentinel = FIREWALL_RULE_SENTINEL
    );

    match std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("Start-Process powershell -ArgumentList '-NoProfile -Command \"{}\"' -Verb RunAs -WindowStyle Hidden", cmd)
        ])
        .spawn()
    {
        Ok(_) => {
            tracing::info!("Triggered UAC prompt for Firewall configuration ({}).", FIREWALL_RULE_SENTINEL);
        }
        Err(e) => {
            tracing::error!("Failed to launch elevated PowerShell: {}", e);
        }
    }
}

pub(crate) fn is_local_ip(ip: std::net::IpAddr) -> bool {
    if let Ok(ifaces) = list_afinet_netifas() {
        for (_name, local_ip) in ifaces {
             if local_ip == ip {
                 return true;
             }
        }
    }
    false
}

// Approximate same-subnet check: same first three IPv4 octets as one of our
// own NIC IPs. We don't have netmask info from `local-ip-address`, so this is
// a /24 heuristic — fine for the home/SMB networks ClusterCut targets but
// will miss /23 or wider segments.
pub(crate) fn is_in_local_subnet(ip: std::net::IpAddr) -> bool {
    let target = match ip {
        std::net::IpAddr::V4(v4) => v4.octets(),
        std::net::IpAddr::V6(_) => return false,
    };
    if let Ok(ifaces) = list_afinet_netifas() {
        for (_name, local_ip) in ifaces {
            if let std::net::IpAddr::V4(local_v4) = local_ip {
                let local = local_v4.octets();
                if local[0] == target[0] && local[1] == target[1] && local[2] == target[2] {
                    return true;
                }
            }
        }
    }
    false
}
