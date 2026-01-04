mod clipboard;
mod crypto;
mod discovery;
mod peer;
mod protocol;
mod state;
mod transport;

use discovery::Discovery;
use peer::Peer;
use protocol::Message;
use rand::Rng;
use state::AppState;
use tauri::{Emitter, Manager};
use transport::Transport;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn get_peers(state: tauri::State<AppState>) -> std::collections::HashMap<String, Peer> {
    state.get_peers()
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
    // For symmetric, we can just use "initiator" and "responder" identity, or device IDs interaction.
    // Let's use "initiator" / "responder" as generic IDs for now, or the PIN itself is enough context.
    // crypto::start_spake2 wants id_a and id_b.
    // Let's use our device_id? We don't have it easily here without passing it securely.
    // Symmetric start only cares about password usually?
    // The wrapper start_spake2(password, id_a, id_b)

    let (spake_state, msg) =
        crypto::start_spake2(&pin, "initiator", "responder").map_err(|e| e.to_string())?;

    // 3. Store state
    {
        let mut pending = state.pending_handshakes.lock().unwrap();
        pending.insert(peer_addr.to_string(), spake_state); // Store by address for handshake correlation
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
    peer_id: String,
    pin: String,
    request_msg: Vec<u8>,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
) -> Result<(), String> {
    // 1. Find peer address
    let peer_addr = {
        let peers = state.get_peers();
        if let Some(peer) = peers.get(&peer_id) {
            std::net::SocketAddr::new(peer.ip, peer.port)
        } else {
            return Err("Peer not found".to_string());
        }
    };

    // 2. Start SPAKE2 (Responder Identity)
    // IMPORTANT: If initiator used "initiator", responder used "responder".
    // Or if symmetric, we just use different IDs.
    let (spake_state, msg_b) =
        crypto::start_spake2(&pin, "responder", "initiator").map_err(|e| e.to_string())?;

    // 3. Finish Handshake immediately (as we have Msg A)
    let shared_key = crypto::finish_spake2(spake_state, &request_msg)
        .map_err(|e| format!("Pairing Failed: {}", e))?;

    println!("Pairing Success! Shared Key derived.");

    // 4. Store Key (Trust this peer)
    {
        let mut trusted = state.trusted_keys.lock().unwrap();
        trusted.insert(peer_id.clone(), shared_key);
        // Also might want to map Address -> Key for incoming packets?
        // But address changes. PeerID is stable.
        // We will need to map PeerID to connection.
    }

    // 5. Send Response
    let local_id = { state.local_device_id.lock().unwrap().clone() };

    let msg_struct = Message::PairResponse {
        msg: msg_b,
        device_id: local_id,
    };
    let data = serde_json::to_vec(&msg_struct).map_err(|e| e.to_string())?;

    transport
        .send_message(peer_addr, &data)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new())
        .setup(|app| {
            // Initialize QUIC Transport on random port
            // Must be built within an async runtime context for Quinn
            let transport = tauri::async_runtime::block_on(async { Transport::new(0) })
                .expect("Failed to create transport");

            let port = transport.local_addr().expect("Failed to get port").port();
            println!("QUIC Transport listening on port {}", port);

            // Store transport in state? For now just keep it alive by moving to a leaked global or app state
            // But we need to keep it running.
            // For MVP, we can spawn the server accept loop here.

            let discovery = Discovery::new().expect("Failed to initialize discovery");

            // Generate a random device ID for this session
            let run_id: u32 = rand::rng().random();
            let device_id = format!("ucp-{}", run_id);

            // Store local ID in state
            {
                let state = app.state::<AppState>();
                let mut id_lock = state.local_device_id.lock().unwrap();
                *id_lock = device_id.clone();
            }

            // Register this device with the actual QUIC port
            discovery
                .register(&device_id, port)
                .expect("Failed to register service");

            // Start browsing for peers
            let receiver = discovery.browse().expect("Failed to browse");

            let app_handle = app.handle().clone();
            let state_ref = app.state::<AppState>();
            let state_for_thread = (*state_ref).clone();

            // Clones for discovery loop
            let discovery_handle = app_handle.clone();
            let discovery_state = state_for_thread.clone();

            tauri::async_runtime::spawn(async move {
                while let Ok(event) = receiver.recv_async().await {
                    match event {
                        mdns_sd::ServiceEvent::ServiceResolved(info) => {
                            if let Some(ip) = info.get_addresses().iter().next() {
                                let id = info
                                    .get_property_val_str("id")
                                    .unwrap_or("unknown")
                                    .to_string();

                                // Filter out self
                                let local_id =
                                    { discovery_state.local_device_id.lock().unwrap().clone() };
                                if id == local_id {
                                    continue;
                                }

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
                                    is_trusted: {
                                        let trusted = discovery_state.trusted_keys.lock().unwrap();
                                        trusted.contains_key(&id)
                                    },
                                };

                                println!("Added Peer: {:?}", peer);
                                discovery_state.add_peer(peer.clone());
                                let _ = discovery_handle.emit("peer-update", &peer);
                            }
                        }
                        _ => {}
                    }
                }
            });

            // Keep variables alive? Transport endpoint drops if not stored.
            app.manage(transport.clone());

            // Clones for transport listener
            let listener_handle = app_handle.clone();
            let listener_state = state_for_thread.clone();

            // Start Listening for incoming sync
            transport.start_listening(move |data, addr| {
                println!("Received {} bytes from {}", data.len(), addr);
                // Try to deserialize as Message
                if let Ok(msg) = serde_json::from_slice::<Message>(&data) {
                    match msg {
                        Message::Clipboard(text) => {
                            println!("Received Clipboard message from {}", addr);
                            clipboard::set_clipboard(text);
                        }
                        Message::PairRequest { msg, device_id } => {
                            println!("Received PairRequest from {} ({})", addr, device_id);
                            // Find Peer ID by address? Or use the device_id in packet?
                            // Packet device_id currently placeholder.
                            // Ideally we use mdns discovery map to reverse lookup IP -> PeerID.
                            // Or we just broadcast "pairing-request" with IP and let frontend match.
                            // Let's assume frontend can match IP or we look it up.
                            // For now, emit event with IP and Msg.

                            // We need to pass the msg bytes to frontend so it can pass them back to respond_to_pairing
                            let _ = listener_handle.emit(
                                "pairing-request",
                                serde_json::json!({
                                    "peer_ip": addr.ip().to_string(), // Frontend might need to look up Peer ID
                                    "msg": msg
                                }),
                            );
                        }
                        Message::PairResponse { msg, device_id } => {
                            println!("Received PairResponse from {} ({})", addr, device_id);
                            // Initiator receives this.
                            // 1. Retrieve pending state
                            // We stored it by ID probably. or IP.
                            // In initiate_pairing we stored by IP and ID.
                            // Let's lookup by IP.
                            let state_opt = {
                                let mut pending = listener_state.pending_handshakes.lock().unwrap();
                                pending.remove(&addr.to_string()) // Take ownership
                            };

                            if let Some(spake_state) = state_opt {
                                // 2. Finish
                                match crypto::finish_spake2(spake_state, &msg) {
                                    Ok(key) => {
                                        println!("Pairing Completed! Key derived.");
                                        // 3. Store Key
                                        // We need PeerID. If we don't have it, we only have IP.
                                        // We really need to know WHO we just paired with.
                                        // We can lookup Peer by IP in peers list.
                                        let mut trusted =
                                            listener_state.trusted_keys.lock().unwrap();

                                        // Lookup peer ID by IP
                                        let peers = listener_state.get_peers();
                                        // Iterate to find IP
                                        let mut found_id = None;
                                        for (p_id, p) in peers {
                                            if p.ip == addr.ip() {
                                                found_id = Some(p_id);
                                                break;
                                            }
                                        }

                                        if let Some(id) = found_id {
                                            trusted.insert(id.clone(), key);

                                            // Update Peer status in map
                                            {
                                                let mut peers_guard =
                                                    listener_state.peers.lock().unwrap();
                                                if let Some(peer) = peers_guard.get_mut(&id) {
                                                    peer.is_trusted = true;
                                                }
                                            }

                                            // Emit update
                                            if let Some(peer) = listener_state.get_peers().get(&id)
                                            {
                                                let _ = listener_handle.emit("peer-update", &peer);
                                            }

                                            println!("Peer Trusted: {}", addr);
                                        } else {
                                            eprintln!(
                                                "Unknown peer IP completed pairing: {}",
                                                addr
                                            );
                                        }
                                    }
                                    Err(e) => eprintln!("Pairing Verification Failed: {}", e),
                                }
                            } else {
                                eprintln!(
                                    "Received PairResponse but no pending handshake found for {}",
                                    addr
                                );
                            }
                        }
                    }
                } else {
                    // Fallback for backward compatibility or raw strings (optional)
                    if let Ok(text) = String::from_utf8(data) {
                        println!("Received legacy string from {}", addr);
                        clipboard::set_clipboard(text);
                    }
                }
            });

            // Start Clipboard Monitor
            let transport_for_clipboard = transport.clone();
            // state_ref is borrowed from app. app is still valid here.
            // But we need AppState (inner)
            let state_for_clipboard = (*state_ref).clone();

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
            respond_to_pairing
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
