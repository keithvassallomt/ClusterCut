mod clipboard;
mod crypto;
mod discovery;
mod peer;
mod protocol;
mod state;
mod storage;
mod transport;

use discovery::Discovery;
use peer::Peer;
use protocol::Message;
use rand::Rng;
use state::AppState;
use storage::{
    load_cluster_key, load_device_id, load_known_peers, save_cluster_key, save_device_id,
    save_known_peers,
};
use tauri::{Emitter, Manager};
use transport::Transport;

// Helper to broadcast a new peer to all known peers (Gossip)
fn gossip_peer(
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
                eprintln!("Failed to gossip peer to {}: {}", addr, e);
            }
        });
    }
}

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn get_peers(state: tauri::State<AppState>) -> std::collections::HashMap<String, Peer> {
    state.get_peers()
}

#[tauri::command]
async fn add_manual_peer(
    ip: String,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // Parse IP:PORT or just IP (Try SocketAddr first, then IP)
    let (addr, port) = if let Ok(sock) = ip.parse::<std::net::SocketAddr>() {
        (sock.ip(), sock.port())
    } else {
        // Just IP?
        if let Ok(ip_addr) = ip.parse::<std::net::IpAddr>() {
            (ip_addr, 0)
        } else {
            return Err("Invalid IP address or format (use IP or IP:PORT)".to_string());
        }
    };

    if port == 0 {
        return Err("Please specify IP:PORT (e.g., 192.168.1.5:4567)".to_string());
    }

    let id = format!("manual-{}", ip);

    let peer = Peer {
        id: id.clone(),
        ip: addr,
        port,
        hostname: format!("Manual ({})", ip),
        last_seen: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        is_trusted: false,
        is_manual: true,
    };

    state.add_peer(peer.clone());
    
    // Save to known peers
    {
        let mut kp = state.known_peers.lock().unwrap();
        if !kp.contains_key(&id) {
            kp.insert(id.clone(), peer.clone());
            save_known_peers(&app_handle, &kp);
        }
    }

    let _ = app_handle.emit("peer-update", &peer);

    // Gossip this new manual peer to others!
    gossip_peer(&peer, &state, &transport, None);

    Ok(())
}

#[tauri::command]
async fn delete_peer(
    peer_id: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
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

#[tauri::command]
async fn initiate_pairing(
    peer_id: String,
    pin: String,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
) -> Result<(), String> {
    // 1. Find peer to get IP
    let peer_addr = {
        let peers = state.get_peers();
        if let Some(peer) = peers.get(&peer_id) {
            std::net::SocketAddr::new(peer.ip, peer.port)
        } else {
            return Err("Peer not found".to_string());
        }
    };

    // 2. Start SPAKE2
    let (spake_state, msg) =
        crypto::start_spake2(&pin, "ucp-connect", "ucp-connect").map_err(|e| e.to_string())?;

    // 3. Store state
    {
        let mut pending = state.pending_handshakes.lock().unwrap();
        pending.insert(peer_addr.to_string(), spake_state); // Store by address
    }

    // 4. Send Message
    let local_id = { state.local_device_id.lock().unwrap().clone() };

    let msg_struct = Message::PairRequest {
        msg,
        device_id: local_id,
    };
    let data = serde_json::to_vec(&msg_struct).map_err(|e| e.to_string())?;

    transport
        .send_message(peer_addr, &data)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
async fn respond_to_pairing(
    peer_id: Option<String>,
    peer_addr: Option<String>,
    device_id: Option<String>,
    pin: String,
    request_msg: Vec<u8>,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // 1. Resolve Peer Address
    let target_addr: std::net::SocketAddr = if let Some(addr_str) = peer_addr {
        addr_str
            .parse()
            .map_err(|e| format!("Invalid Peer Address: {}", e))?
    } else if let Some(id) = &peer_id {
        let peers = state.get_peers();
        if let Some(peer) = peers.get(id) {
            std::net::SocketAddr::new(peer.ip, peer.port)
        } else {
            return Err("Peer not found in list".to_string());
        }
    } else {
        return Err("Must provide either peer_id or peer_addr".to_string());
    };

    let target_id = device_id.clone().unwrap_or_else(|| "unknown".to_string());

    // 2. Start SPAKE2 (Responder Identity)
    let (spake_state, msg_b) =
        crypto::start_spake2(&pin, "ucp-connect", "ucp-connect").map_err(|e| e.to_string())?;

    // 3. Finish Handshake to get Session Key
    let session_key = crypto::finish_spake2(spake_state, &request_msg)
        .map_err(|e| format!("Pairing Failed: {}", e))?;

    println!("Pairing Success! Session Key derived.");

    // 4. Ensure we have a Cluster Key to share
    let cluster_key_bytes = {
        let mut ck_lock = state.cluster_key.lock().unwrap();
        if let Some(key) = ck_lock.as_ref() {
            key.clone()
        } else {
            // Generate new Cluster Key
            let new_key: [u8; 32] = rand::random();
            *ck_lock = Some(new_key.to_vec());
            // Save to disk
            save_cluster_key(&app_handle, &new_key);
            println!("Generated new Cluster Key.");
            new_key.to_vec()
        }
    };

    // 5. Encrypt Cluster Key with Session Key
    let mut session_key_arr = [0u8; 32];
    if session_key.len() != 32 {
        return Err("Invalid session key length".to_string());
    }
    session_key_arr.copy_from_slice(&session_key);

    let encrypted_cluster_key =
        crypto::encrypt(&session_key_arr, &cluster_key_bytes).map_err(|e| e.to_string())?;

    // 6. Send Response AND Welcome
    let local_id = { state.local_device_id.lock().unwrap().clone() };

    // A. Send PairResponse
    let msg_struct = Message::PairResponse {
        msg: msg_b,
        device_id: local_id.clone(),
    };
    let data_resp = serde_json::to_vec(&msg_struct).map_err(|e| e.to_string())?;
    transport
        .send_message(target_addr, &data_resp)
        .await
        .map_err(|e| e.to_string())?;

    // B. Send Welcome (with encrypted key and known peers)
    let known_peers = {
        let kp = state.known_peers.lock().unwrap();
        kp.values().cloned().collect()
    };

    let welcome_struct = Message::Welcome {
        encrypted_cluster_key,
        known_peers,
    };
    let data_welcome = serde_json::to_vec(&welcome_struct).map_err(|e| e.to_string())?;

    // Small delay to ensure order
    std::thread::sleep(std::time::Duration::from_millis(100));

    transport
        .send_message(target_addr, &data_welcome)
        .await
        .map_err(|e| e.to_string())?;

    println!("Sent Welcome Packet to {}", target_addr);

    // C. Add Target to Known Peers locally
    {
        let mut kp_lock = state.known_peers.lock().unwrap();
        // ALWAYS update the peer info on successful pairing!
        // This ensures ip/port updates and is_manual flag is set correctly.
        let p = Peer {
            id: target_id.clone(),
            ip: target_addr.ip(),
            port: target_addr.port(),
            hostname: format!("Peer ({})", target_addr.ip()), 
            last_seen: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            is_trusted: true,
            // If we don't know them via mDNS (runtime peers), treat as manual so we remember them.
            // Also, if they were ALREADY manual, keep them manual.
            is_manual: {
                let runtime = state.peers.lock().unwrap();
                if let Some(existing) = kp_lock.get(&target_id) {
                    if existing.is_manual {
                        true
                    } else {
                        !runtime.contains_key(&target_id)
                    }
                } else {
                    !runtime.contains_key(&target_id)
                }
            },
        };
        kp_lock.insert(target_id.clone(), p.clone());
        save_known_peers(&app_handle, &kp_lock);
        
        // Add to runtime and Gossip!
        state.add_peer(p.clone());
        let _ = app_handle.emit("peer-update", &p);
        
        gossip_peer(&p, &state, &transport, Some(target_addr));
    }

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new())
        .setup(|app| {
            // Initialize QUIC Transport
            let transport = tauri::async_runtime::block_on(async { Transport::new(0) })
                .expect("Failed to create transport");

            let port = transport.local_addr().expect("Failed to get port").port();
            println!("QUIC Transport listening on port {}", port);

            let app_handle = app.handle();

            // Load State
            {
                let state = app.state::<AppState>();

                // 1. Load Cluster Key
                let mut ck_lock = state.cluster_key.lock().unwrap();
                *ck_lock = load_cluster_key(app_handle);

                // 2. Load Known Peers
                let mut kp_lock = state.known_peers.lock().unwrap();
                *kp_lock = load_known_peers(app_handle);
                
                // Load known peers into RUNTIME state too! (Fixes UI not showing known peers on restart)
                // BUT: Only show Manual peers initially. mDNS peers will show up when discovered.
                let mut runtime_peers = state.peers.lock().unwrap();
                for peer in kp_lock.values() {
                     if peer.is_manual {
                        runtime_peers.insert(peer.id.clone(), peer.clone());
                     }
                }

                // 3. Load Device ID
                let mut device_id = load_device_id(app_handle);
                if device_id.is_empty() {
                    let run_id: u32 = rand::rng().random();
                    device_id = format!("ucp-{}", run_id);
                    save_device_id(app_handle, &device_id);
                    println!("Generated new Device ID: {}", device_id);
                } else {
                    println!("Loaded Device ID: {}", device_id);
                }
                *state.local_device_id.lock().unwrap() = device_id.clone();

                // 4. Register Discovery
                let mut discovery = Discovery::new().expect("Failed to initialize discovery");
                discovery
                    .register(&device_id, port)
                    .expect("Failed to register service");
                let receiver = discovery.browse().expect("Failed to browse");
                *state.discovery.lock().unwrap() = Some(discovery);

                // Spawn Discovery Loop
                let d_handle = app_handle.clone();
                let d_state = (*state).clone();

                tauri::async_runtime::spawn(async move {
                    while let Ok(event) = receiver.recv_async().await {
                        match event {
                            mdns_sd::ServiceEvent::ServiceResolved(info) => {
                                if let Some(ip) = info.get_addresses().iter().next() {
                                    let id = info
                                        .get_property_val_str("id")
                                        .unwrap_or("unknown")
                                        .to_string();

                                    let local_id =
                                        { d_state.local_device_id.lock().unwrap().clone() };
                                    if id == local_id {
                                        continue;
                                    }

                                    let is_known = {
                                        let kp = d_state.known_peers.lock().unwrap();
                                        kp.contains_key(&id)
                                    };

                                    let peer = Peer {
                                        id: id.clone(),
                                        ip: ip.to_string().parse().unwrap_or(std::net::IpAddr::V4(
                                            std::net::Ipv4Addr::new(127, 0, 0, 1),
                                        )),
                                        port: info.get_port(),
                                        hostname: info.get_hostname().to_string(),
                                        last_seen: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs(),
                                        is_trusted: is_known,
                                        is_manual: false,
                                    };

                                    d_state.add_peer(peer.clone());
                                    let _ = d_handle.emit("peer-update", &peer);
                                }
                            }
                            mdns_sd::ServiceEvent::ServiceRemoved(_ty, fullname) => {
                                let id =
                                    fullname.split('.').next().unwrap_or("unknown").to_string();
                                {
                                    let mut peers = d_state.peers.lock().unwrap();
                                    peers.remove(&id);
                                }
                                let _ = d_handle.emit("peer-remove", &id);
                            }
                            _ => {}
                        }
                    }
                });
            }

            // Clones for transport listener
            let listener_handle = app.handle().clone();
            let listener_state = (*app.state::<AppState>()).clone();
            let transport_for_ack = transport.clone();

            app.manage(transport.clone());

            // Start Listening
            transport.start_listening(move |data, addr| {
                println!("Received {} bytes from {}", data.len(), addr);

                if let Ok(msg) = serde_json::from_slice::<Message>(&data) {
                    match msg {
                        Message::Clipboard(ciphertext) => {
                            println!("Received Encrypted Clipboard from {}", addr);
                            // Decrypt!
                            let key_opt = {
                                let ck = listener_state.cluster_key.lock().unwrap();
                                ck.clone()
                            };

                            if let Some(key) = key_opt {
                                let mut key_arr = [0u8; 32];
                                if key.len() == 32 {
                                    key_arr.copy_from_slice(&key);
                                    match crypto::decrypt(&key_arr, &ciphertext) {
                                        Ok(plaintext) => {
                                            if let Ok(text) = String::from_utf8(plaintext) {
                                                println!("Decrypted Clipboard: {}", text);
                                                clipboard::set_clipboard(text);
                                            }
                                        }
                                        Err(e) => eprintln!("Failed to decrypt clipboard: {}", e),
                                    }
                                }
                            } else {
                                eprintln!("Received clipboard but no Cluster Key set!");
                            }
                        }
                        Message::PairRequest { msg, device_id } => {
                            println!("Received PairRequest from {} ({})", addr, device_id);
                            let _ = listener_handle.emit(
                                "pairing-request",
                                serde_json::json!({
                                    "peer_addr": addr.to_string(),
                                    "device_id": device_id,
                                    "msg": msg
                                }),
                            );
                        }
                        Message::PairResponse { msg, device_id } => {
                            println!("Received PairResponse from {} ({})", addr, device_id);
                            // Finish Handshake
                            let state_opt = {
                                let mut pending =
                                    listener_state.pending_handshakes.lock().unwrap();
                                pending.remove(&addr.to_string())
                            };

                            if let Some(spake_state) = state_opt {
                                match crypto::finish_spake2(spake_state, &msg) {
                                    Ok(session_key) => {
                                        println!("Auth Success! Storing session key for Welcome packet...");
                                        // Store session key to wait for Welcome
                                        let mut sessions = listener_state.handshake_sessions.lock().unwrap();
                                        sessions.insert(addr.to_string(), session_key);
                                    }
                                    Err(e) => eprintln!("Auth Failed: {}", e),
                                }
                            } else {
                                eprintln!("No pending handshake for PairResponse from {}", addr);
                            }
                        }
                        Message::Welcome {
                            encrypted_cluster_key,
                            known_peers,
                        } => {
                            println!("Received WELCOME from {}", addr);
                            // Retrieve Session Key
                            let session_key_opt = {
                                let mut sessions = listener_state.handshake_sessions.lock().unwrap();
                                sessions.remove(&addr.to_string())
                            };

                            if let Some(session_key) = session_key_opt {
                                let mut session_key_arr = [0u8; 32];
                                if session_key.len() == 32 {
                                    session_key_arr.copy_from_slice(&session_key);
                                    
                                    // Decrypt Cluster Key
                                    match crypto::decrypt(&session_key_arr, &encrypted_cluster_key) {
                                        Ok(cluster_key) => {
                                            println!("Cluster Key Decrypted! Joining Network...");
                                            // 1. Save Cluster Key
                                            {
                                                let mut ck = listener_state.cluster_key.lock().unwrap();
                                                *ck = Some(cluster_key.clone());
                                                save_cluster_key(listener_handle.app_handle(), &cluster_key);
                                            }

                                            // 2. Merge Known Peers (AND Update Runtime)
                                            {
                                                let mut kp_lock = listener_state.known_peers.lock().unwrap();
                                                let mut runtime_peers = listener_state.peers.lock().unwrap();
                                                
                                                for peer in known_peers {
                                                    // Add to Persistent
                                                    kp_lock.insert(peer.id.clone(), peer.clone());
                                                    
                                                    // Update Runtime + UI
                                                    // Only show Manual peers initially. mDNS peers will show up when discovered.
                                                    if peer.is_manual && !runtime_peers.contains_key(&peer.id) {
                                                        runtime_peers.insert(peer.id.clone(), peer.clone());
                                                        let _ = listener_handle.emit("peer-update", &peer);
                                                    }
                                                }
                                                // Save known peers
                                                save_known_peers(listener_handle.app_handle(), &kp_lock);
                                            }

                                            println!("Successfully joined network!");
                                        }
                                        Err(e) => eprintln!("Failed to decrypt Cluster Key: {}", e),
                                    }
                                }
                            } else {
                                eprintln!("Received Welcome but no session key found");
                            }
                        }
                        Message::PeerDiscovery(mut peer) => {
                             println!("Received Gossip about peer {} ({})", peer.hostname, peer.ip);
                             // If it's gossiped, it means it's a Manual peer from someone else's perspective.
                             // We should treat it as manual too, so we persist and see it.
                             peer.is_manual = true; 

                             // Add to Known Peers and Runtime
                             {
                                 let mut kp_lock = listener_state.known_peers.lock().unwrap();
                                 let mut runtime_peers = listener_state.peers.lock().unwrap();
                                 
                                 // Update Persistent
                                 kp_lock.insert(peer.id.clone(), peer.clone());
                                 save_known_peers(listener_handle.app_handle(), &kp_lock);
                                 
                                 // Update Runtime + UI
                                 runtime_peers.insert(peer.id.clone(), peer.clone());
                                 let _ = listener_handle.emit("peer-update", &peer);
                             }
                        }
                    }
                }
            });

            // Start Clipboard Monitor
            let transport_for_clipboard = transport.clone();
            let state_for_clipboard = (*app.state::<AppState>()).clone();

            clipboard::start_monitor(
                app.handle().clone(),
                state_for_clipboard,
                transport_for_clipboard,
            );

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            get_peers,
            initiate_pairing,
            respond_to_pairing,
            add_manual_peer,
            delete_peer
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let state = app_handle.state::<AppState>();
                let mut d_lock = state.discovery.lock().unwrap();
                if let Some(d) = d_lock.take() {
                    drop(d);
                }
            }
        });
}
