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
    load_cluster_key, load_device_id, load_known_peers, load_network_name, load_network_pin,
    save_cluster_key, save_device_id, save_known_peers, save_network_name, save_network_pin,
    reset_network_state,
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
fn get_network_name(state: tauri::State<'_, AppState>) -> String {
    state.network_name.lock().unwrap().clone()
}

#[tauri::command]
fn get_network_pin(state: tauri::State<'_, AppState>) -> String {
    state.network_pin.lock().unwrap().clone()
}

#[tauri::command]
fn get_peers(state: tauri::State<AppState>) -> std::collections::HashMap<String, Peer> {
    state.get_peers()
}

#[tauri::command]
fn get_local_ip() -> String {
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
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
        is_trusted: true,
        is_manual: true,
        network_name: None, // Manual peers don't annouce it via IP add
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
async fn leave_network(
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
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
async fn delete_peer(
    peer_id: String,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
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

#[tauri::command]
async fn start_pairing(
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

// Helper to wipe state and restart network identity
fn perform_factory_reset(app_handle: &tauri::AppHandle, state: &AppState, port: u16) {
    // 1. Reset Config on Disk
    reset_network_state(app_handle);
    
    // 2. Clear Runtime State & Generate New Identity
    {
        let mut kp = state.known_peers.lock().unwrap();
        kp.clear();
        let mut peers = state.peers.lock().unwrap();
        // Don't clear peers! mDNS has already discovered them. 
        // Just mark them as untrusted so they show up in "Nearby Networks".
        for p in peers.values_mut() {
            p.is_trusted = false;
            // Note: their network_name remains whatever it was (e.g. "OldNetwork").
            // Since our network_name changes below, they will rightfully be Foreign.
        }
        let mut ck = state.cluster_key.lock().unwrap();
        *ck = None;
        
        // Generate new Name & PIN
        let new_name = load_network_name(app_handle);
        let new_pin = load_network_pin(app_handle);
        
        *state.network_name.lock().unwrap() = new_name.clone();
        *state.network_pin.lock().unwrap() = new_pin.clone();
        
        println!("Reset to New Network: {} (PIN: {})", new_name, new_pin);
        
        // Re-register mDNS with new name
        let local_id = state.local_device_id.lock().unwrap().clone();
        if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
             let _ = discovery.register(&local_id, &new_name, port);
        }
    }
    
    // 3. Notify Frontend
    let _ = app_handle.emit("network-reset", ());
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
                
                // 3b. Load Network Name (for mDNS)
                let network_name = load_network_name(app_handle);
                *state.network_name.lock().unwrap() = network_name.clone();

                // 3c. Load Network PIN
                let network_pin = load_network_pin(app_handle);
                *state.network_pin.lock().unwrap() = network_pin.clone();
                println!("Network PIN: {}", network_pin);

                // 4. Register Discovery
                let mut discovery = Discovery::new().expect("Failed to initialize discovery");
                discovery
                    .register(&device_id, &network_name, port)
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

                                    let network_name_prop = info
                                        .get_property_val_str("n")
                                        .map(|s| s.to_string());
                                    
                                    if let Some(n) = &network_name_prop {
                                        println!("Discovered peer {} with network name: {}", id, n);
                                    } else {
                                        println!("Discovered peer {} WITHOUT network name (properties: {:?})", id, info.get_properties());
                                    }

                                    // Lock known_peers to prevent race with PairRequest
                                    let kp = d_state.known_peers.lock().unwrap();
                                    let is_known = kp.contains_key(&id);

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
                                        is_manual: false, // Discovered via mDNS
                                        network_name: network_name_prop,
                                    };

                                    d_state.add_peer(peer.clone());
                                    let _ = d_handle.emit("peer-update", &peer);
                                    // Lock drops here
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
            let transport_inside = transport.clone();
            transport.start_listening(move |data, addr| {
                println!("Received {} bytes from {}", data.len(), addr);

                if let Ok(msg) = serde_json::from_slice::<Message>(&data) {
                    let listener_handle = listener_handle.clone();
                    let listener_state = listener_state.clone();
                    let transport_inside = transport_inside.clone();

                    tauri::async_runtime::spawn(async move {
                        match msg {
                            Message::Clipboard(ciphertext) => {
                                println!("Received Encrypted Clipboard from {}", addr);
                                let key_opt = {
                                    listener_state.cluster_key.lock().unwrap().clone()
                                };

                                if let Some(key) = key_opt {
                                    let mut key_arr = [0u8; 32];
                                    if key.len() == 32 {
                                        key_arr.copy_from_slice(&key);
                                        match crypto::decrypt(&key_arr, &ciphertext).map_err(|e| e.to_string()) {
                                            Ok(plaintext) => {
                                                if let Ok(text) = String::from_utf8(plaintext) {
                                                    // Loop Check
                                                    {
                                                        let mut last = listener_state.last_clipboard_content.lock().unwrap();
                                                        if *last == text { return; }
                                                        *last = text.clone();
                                                    }
                                                    
                                                    println!("Decrypted Clipboard: {}", text);
                                                    clipboard::set_clipboard(text.clone());
                                                    let _ = listener_handle.emit("clipboard-change", &text);

                                                    // Relay
                                                    let state_relay = listener_state.clone();
                                                    let transport_relay = transport_inside.clone(); 
                                                    let sender_addr = addr;
                                                    let relay_text = text.clone();
                                                    let relay_key_arr = key_arr; 
                                                    
                                                    // Re-encrypt for relay (fresh nonce)
                                                    if let Ok(relay_ciphertext) = crypto::encrypt(&relay_key_arr, relay_text.as_bytes()).map_err(|e| e.to_string()) {
                                                        let relay_data = serde_json::to_vec(&Message::Clipboard(relay_ciphertext)).unwrap_or_default();
                                                        let peers = state_relay.get_peers();
                                                        for p in peers.values() {
                                                            let p_addr = std::net::SocketAddr::new(p.ip, p.port);
                                                            if p_addr == sender_addr { continue; }
                                                            let _ = transport_relay.send_message(p_addr, &relay_data).await;
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => eprintln!("Failed to decrypt clipboard: {}", e),
                                        }
                                    }
                                }
                            }
                            Message::PairRequest { msg, device_id } => {
                                println!("Received PairRequest from {} ({}). Authenticating...", addr, device_id);
                                let local_id = listener_state.local_device_id.lock().unwrap().clone();
                                let pin = listener_state.network_pin.lock().unwrap().clone();
                                
                                match crypto::start_spake2(&pin, &local_id, &device_id).map_err(|e| e.to_string()) {
                                    Ok((spake_state, response_msg)) => {
                                        let resp_struct = Message::PairResponse {
                                            msg: response_msg,
                                            device_id: local_id.clone(),
                                        };
                                        if let Ok(resp_data) = serde_json::to_vec(&resp_struct) {
                                            if transport_inside.send_message(addr, &resp_data).await.map_err(|e| e.to_string()).is_ok() {
                                                 match crypto::finish_spake2(spake_state, &msg).map_err(|e| e.to_string()) {
                                                     Ok(session_key) => {
                                                         println!("Authentication Success for {}!", device_id);
                                                         // Encrypt Cluster Key
                                                         let cluster_key_opt = {
                                                             listener_state.cluster_key.lock().unwrap().clone()
                                                         };
                                                         if let Some(cluster_key) = cluster_key_opt {
                                                             let mut session_key_arr = [0u8; 32];
                                                             if session_key.len() == 32 {
                                                                 session_key_arr.copy_from_slice(&session_key);
                                                                 if let Ok(encrypted_ck) = crypto::encrypt(&session_key_arr, &cluster_key).map_err(|e| e.to_string()) {
                                                                     let known_peers = listener_state.known_peers.lock().unwrap().values().cloned().collect();
                                                                     let network_name = listener_state.network_name.lock().unwrap().clone();
                                                                     let network_pin = listener_state.network_pin.lock().unwrap().clone();
                                                                     let welcome = Message::Welcome {
                                                                         encrypted_cluster_key: encrypted_ck,
                                                                         known_peers,
                                                                         network_name: network_name.clone(),
                                                                         network_pin
                                                                     };
                                                                     if let Ok(welcome_data) = serde_json::to_vec(&welcome) {
                                                                         let _ = transport_inside.send_message(addr, &welcome_data).await;
                                                                         
                                                                         // Trust them
                                                                         let mut kp_lock = listener_state.known_peers.lock().unwrap();
                                                                         let p = crate::peer::Peer {
                                                                             id: device_id.clone(),
                                                                             ip: addr.ip(),
                                                                             port: addr.port(),
                                                                             hostname: format!("Peer ({})", addr.ip()), 
                                                                             last_seen: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                                                             is_trusted: true,
                                                                             is_manual: false,
                                                                             network_name: Some(network_name),
                                                                         };
                                                                         kp_lock.insert(device_id.clone(), p.clone());
                                                                         save_known_peers(listener_handle.app_handle(), &kp_lock);
                                                                         listener_state.add_peer(p.clone());
                                                                         let _ = listener_handle.emit("peer-update", &p);
                                                                         gossip_peer(&p, &listener_state, &transport_inside, Some(addr));
                                                                     }
                                                                 }
                                                             }
                                                         }
                                                     }
                                                     Err(e) => eprintln!("Auth Failed: {}", e),
                                                 }
                                            }
                                        }
                                    }
                                    Err(e) => eprintln!("SPAKE2 Error: {}", e),
                                }
                            }
                            Message::PairResponse { msg, device_id } => {
                                println!("Received PairResponse from {} ({})", addr, device_id);
                                let spake_state = {
                                    let mut pending = listener_state.pending_handshakes.lock().unwrap();
                                    pending.remove(&addr.to_string())
                                };
                                if let Some(state) = spake_state {
                                    match crypto::finish_spake2(state, &msg).map_err(|e| e.to_string()) {
                                        Ok(session_key) => {
                                            println!("Auth Success (Initiator)! Waiting for Welcome...");
                                            let mut sessions = listener_state.handshake_sessions.lock().unwrap();
                                            sessions.insert(addr.to_string(), session_key);
                                        }
                                        Err(e) => eprintln!("Auth Failed: {}", e),
                                    }
                                }
                            }
                            Message::Welcome { encrypted_cluster_key, known_peers, network_name, network_pin } => {
                                println!("Received WELCOME from {}", addr);
                                let session_key = {
                                    let sessions = listener_state.handshake_sessions.lock().unwrap();
                                    sessions.get(&addr.to_string()).cloned()
                                };
                                if let Some(sk) = session_key {
                                    let mut sk_arr = [0u8; 32];
                                    if sk.len() == 32 {
                                        sk_arr.copy_from_slice(&sk);
                                        match crypto::decrypt(&sk_arr, &encrypted_cluster_key).map_err(|e| e.to_string()) {
                                            Ok(cluster_key) => {
                                                println!("Joined Network: {} (PIN: {})", network_name, network_pin);
                                                // Save Keys & Name
                                                {
                                                    let mut ck = listener_state.cluster_key.lock().unwrap();
                                                    *ck = Some(cluster_key.clone());
                                                    save_cluster_key(listener_handle.app_handle(), &cluster_key);
                                                    
                                                    let mut nn = listener_state.network_name.lock().unwrap();
                                                    *nn = network_name.clone();
                                                    save_network_name(listener_handle.app_handle(), &network_name);
                                                    
                                                    let mut np = listener_state.network_pin.lock().unwrap();
                                                    *np = network_pin.clone();
                                                    save_network_pin(listener_handle.app_handle(), &network_pin);
                                                }
                                                // Re-register mDNS
                                                let device_id = listener_state.local_device_id.lock().unwrap().clone();
                                                let port = transport_inside.local_addr().map(|a| a.port()).unwrap_or(0);
                                                if let Some(discovery) = listener_state.discovery.lock().unwrap().as_mut() {
                                                     let _ = discovery.register(&device_id, &network_name, port);
                                                }
                                                // Merge Peers
                                                let mut kp_lock = listener_state.known_peers.lock().unwrap();
                                                let mut runtime_peers = listener_state.peers.lock().unwrap();
                                                for peer in known_peers {
                                                    kp_lock.insert(peer.id.clone(), peer.clone());
                                                    
                                                    // Always update runtime peer to ensure Trust is reflected
                                                    // (Even if mDNS discovered it first as untrusted)
                                                    runtime_peers.insert(peer.id.clone(), peer.clone());
                                                    let _ = listener_handle.emit("peer-update", &peer);
                                                }
                                                save_known_peers(listener_handle.app_handle(), &kp_lock);
                                                
                                                // Trust Gateway
                                                for (id, peer) in runtime_peers.iter_mut() {
                                                    if peer.ip == addr.ip() {
                                                        peer.is_trusted = true;
                                                        peer.network_name = Some(network_name.clone());
                                                        let _ = listener_handle.emit("peer-update", &*peer);
                                                        // Update known peers for gateway too
                                                        kp_lock.insert(id.clone(), peer.clone());
                                                        break;
                                                    }
                                                }
                                                save_known_peers(listener_handle.app_handle(), &kp_lock);
                                            }
                                            Err(e) => eprintln!("Decryption Error: {}", e),
                                        }
                                    }
                                }
                            }
                            Message::PeerDiscovery(mut peer) => {
                                println!("Received PeerDiscovery for {}", peer.hostname);
                                peer.is_manual = true; 
                                {
                                     let mut kp_lock = listener_state.known_peers.lock().unwrap();
                                     kp_lock.insert(peer.id.clone(), peer.clone());
                                     save_known_peers(listener_handle.app_handle(), &kp_lock);
                                     listener_state.add_peer(peer.clone());
                                     let _ = listener_handle.emit("peer-update", &peer);
                                }
                            }
                            Message::PeerRemoval(target_id) => {
                                println!("Received PeerRemoval for {}", target_id);
                                let local_id = listener_state.local_device_id.lock().unwrap().clone();
                                
                                if target_id == local_id {
                                    println!("I have been removed from the network! resetting state...");
                                    perform_factory_reset(
                                        &listener_handle,
                                        &listener_state,
                                        transport_inside.local_addr().map(|a| a.port()).unwrap_or(0)
                                    );
                                } else {
                                    // Remove the peer
                                    {
                                        let mut kp = listener_state.known_peers.lock().unwrap();
                                        if kp.remove(&target_id).is_some() {
                                            save_known_peers(listener_handle.app_handle(), &kp);
                                        }
                                    }
                                    {
                                        let mut peers = listener_state.peers.lock().unwrap();
                                        peers.remove(&target_id);
                                    }
                                    let _ = listener_handle.emit("peer-remove", &target_id);
                                }
                            }
                        }
                    });
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
            get_local_ip,
            get_peers,
            add_manual_peer,
            start_pairing,
            delete_peer,
            leave_network,
            get_network_name,
            get_network_pin
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
