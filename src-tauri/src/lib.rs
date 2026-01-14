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
    reset_network_state, load_settings, AppSettings,
};
use tauri::{Emitter, Manager};
use transport::Transport;
use tauri_plugin_notification::NotificationExt;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

// Helper to broadcast a new peer to all known peers (Gossip)
fn send_notification(app_handle: &tauri::AppHandle, title: &str, body: &str) {
    // 1. Native Plugin (All OS)
    if let Err(e) = app_handle.notification()
        .builder()
        .title(title)
        .body(body)
        .sound("Ping")
        .show() 
    {
        eprintln!("[Notification Error] Native plugin failed: {}", e);
    }
    
    // 2. Linux Workaround (notify-send)
    #[cfg(target_os = "linux")]
    {
        println!("[Notification] Linux detected. Attempting notify-send workaround...");
        match std::process::Command::new("notify-send")
            .arg(title)
            .arg(body)
            .spawn() 
        {
            Ok(_) => println!("[Notification] notify-send executed successfully."),
            Err(e) => eprintln!("[Notification Error] notify-send failed: {}", e),
        }
    }
}

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
fn get_device_id(state: tauri::State<'_, AppState>) -> String {
    state.local_device_id.lock().unwrap().clone()
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
fn get_hostname(state: tauri::State<'_, AppState>) -> String {
    let settings = state.settings.lock().unwrap();
    if let Some(custom_name) = &settings.custom_device_name {
        if !custom_name.trim().is_empty() {
             return custom_name.clone();
        }
    }
    
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string())
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> AppSettings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(
    settings: AppSettings,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    *state.settings.lock().unwrap() = settings.clone();
    crate::storage::save_settings(&app_handle, &settings);
    // If auto_receive is now OFF, we might want to do something?
    // If device name changed, we should probably rebroadcast or something, 
    // but the next heartbeat or discovery probe will pick it up.
    // Ideally we emit an event if needed.
    
    // Check if network name changed via Provisioning (this function saves AppSettings, but UI might call separate commands for Network Name/PIN)
    // Wait, the UI for Provisioned Mode will likely update NetworkName/PIN directly? 
    // Or do we store them in AppSettings too? 
    // The requirement says "Provisioned mode, the user can enter a cluster name and PIN". 
    // Those are actually `state.network_name` and `state.network_pin`. 
    // `AppSettings` stores the *mode*. 
    // So the UI should call `save_network_identity` (new command needed?) or Update existing commands?
    // We already have `load_network_name` but no set command exposed.
    // I will add `set_network_identity` command.
}

#[tauri::command]
fn set_network_identity(
    name: String,
    pin: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    // Validate?
    *state.network_name.lock().unwrap() = name.clone();
    *state.network_pin.lock().unwrap() = pin.clone();
    
    crate::storage::save_network_name(&app_handle, &name);
    crate::storage::save_network_pin(&app_handle, &pin);
    
    // Also likely need to reset keys if we are "provisioning" a new identity? 
    // Or do we keep the key? 
    // If I type a new name/pin, I am essentially saying "I belong to THIS network now".
    // I need the key for THAT network. 
    // If I'm creating it, I generate a key. 
    // If I'm joining it (provisioned), I usually need the Key too OR I need to Pair.
    // But "Provisioned" usually means "I set the config manually". 
    // The prompt says "Toggle... default behaviour applies (random)... Provisioned... user can enter".
    // It doesn't say "User enters Key".
    // So "Provisioned" here effectively just means "Manual valid Network Name/PIN" instead of "Random Name/PIN".
    // It implies we are STARTING a cluster with this name/pin.
    // So we keep our current Key (or gen a new one). 
    // Since we are changing identity, a new Key is safer.
    // But if we just rename the cluster, we might want to keep the key.
    // Let's assume re-provisioning = New Identity = Gen New Key too?
    // Actually, if I just want to rename my cluster "My Home", I don't want to break existing peers if I can help it?
    // But existing peers know me by Key? No, they pair with Spake2 using PIN.
    // If I change PIN, they can't pair.
    // If I change Name, they see "My Home" instead of "Fuzzy-Badger".
    // I'll stick to just updating Name/PIN.
    
    // Re-register mDNS with new name
    let device_id = state.local_device_id.lock().unwrap().clone();
    let port = 4654; // TODO: Get actual port from transport? We don't have transport here. 
    // Discovery usually stores port.
    if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
          let _ = discovery.register(&device_id, &name, port);
    }
    
    let _ = app_handle.emit("network-update", ());
}

#[tauri::command]
fn regenerate_network_identity(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    let (name, pin) = crate::storage::regenerate_identity(&app_handle);
    
    *state.network_name.lock().unwrap() = name.clone();
    *state.network_pin.lock().unwrap() = pin.clone();
    
    let device_id = state.local_device_id.lock().unwrap().clone();
    let port = 4654; 
    
    if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
          let _ = discovery.register(&device_id, &name, port);
    }
    
    let _ = app_handle.emit("network-update", ());
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

use ipnetwork::IpNetwork;

// Signature Helpers
fn generate_signature(key: &[u8; 32], id: &str) -> Option<String> {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let payload = format!("{}:{}", id, ts);
    if let Ok(encrypted) = crypto::encrypt(key, payload.as_bytes()) {
        return Some(BASE64.encode(encrypted));
    }
    None
}

fn verify_signature(key: &[u8; 32], id: &str, signature: &str) -> bool {
    if let Ok(encrypted) = BASE64.decode(signature) {
         if let Ok(decrypted) = crypto::decrypt(key, &encrypted) {
             if let Ok(payload) = String::from_utf8(decrypted) {
                 // Payload: "ID:TIMESTAMP"
                 let parts: Vec<&str> = payload.split(':').collect();
                 if parts.len() == 2 {
                     if parts[0] == id {
                         if let Ok(ts) = parts[1].parse::<u64>() {
                             let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                             // Allow 60s skew/replay window
                             if now >= ts && (now - ts) < 60 {
                                 return true;
                             }
                             // Also allow minor clock drift (future timestamp)? 
                             if ts > now && (ts - now) < 10 {
                                 return true;
                             }
                         }
                     }
                 }
             }
         }
    }
    false
}

// Helper to probe a specific IP/Port
async fn probe_ip(
    ip: std::net::IpAddr,
    port: u16,
    state: AppState,
    transport: Transport,
    app_handle: tauri::AppHandle,
) {
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

    let mut signature = None;
    if let Some(key_vec) = state.cluster_key.lock().unwrap().as_ref() {
        if key_vec.len() == 32 {
            let mut key_arr = [0u8; 32];
            key_arr.copy_from_slice(key_vec);
            signature = generate_signature(&key_arr, &local_id);
        }
    }
    
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
        signature,
    };

    let msg = Message::PeerDiscovery(my_peer);
    let data = serde_json::to_vec(&msg).unwrap_or_default();
    
    println!("Probing {}...", addr);
    
    // We don't want to block too long.
    match tokio::time::timeout(std::time::Duration::from_secs(2), transport.send_message(addr, &data)).await {
        Ok(Ok(_)) => {
            println!("Probe to {} SUCCESS!", addr);
            // We connected! Add them as a temporary manual peer.
            // We don't know their ID yet, so use "manual-<IP>".
            let id = format!("manual-{}", ip);
             let peer = Peer {
                id: id.clone(),
                ip,
                port,
                hostname: format!("Manual ({})", ip),
                last_seen: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                is_trusted: false, // Untrusted! User must Join.
                is_manual: true,
                network_name: Some("Found!".to_string()),
                signature: None,
            };

            state.add_peer(peer.clone());
            {
                 let mut kp = state.known_peers.lock().unwrap();
                 kp.insert(id.clone(), peer.clone());
                 save_known_peers(&app_handle, &kp);
            }
            let _ = app_handle.emit("peer-update", &peer);

            // Trigger Notification
            {
                if state.settings.lock().unwrap().notifications.device_join {
                    println!("[Notification] Triggering 'New Device Found' for manual peer: {}", peer.hostname);
                    send_notification(&app_handle, "New Device Found", &format!("{} has joined your cluster", peer.hostname));
                } else {
                    println!("[Notification] Device join notification suppressed by settings for manual peer: {}", peer.hostname);
                }
            }
        }
        _ => {
            // println!("Probe to {} failed or timed out.", addr);
        }
    }
}

#[tauri::command]
async fn add_manual_peer(
    ip: String, // Can be IP or CIDR
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    
    // 1. Try parsing as CIDR
    if let Ok(net) = ip.parse::<IpNetwork>() {
        println!("Scanning range: {}", net);
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
                     probe_ip(addr, 4654, s, t, a).await; // Fixed Port 4654
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
        probe_ip(addr, port, (*state).clone(), (*transport).clone(), app_handle).await;
        Ok(())
    }
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
    // 2. Clear Runtime State & Generate New Identity
    {
        // ACQUIRE LOCKS FIRST
        // This prevents background tasks (like Pruning or Heartbeat) from 
        // reading/writing stale state while we are resetting.
        let mut kp = state.known_peers.lock().unwrap();
        let mut peers = state.peers.lock().unwrap();
        
        // 1. Reset Config on Disk
        // Now safe because we hold the locks that background tasks would need to write back.
        reset_network_state(app_handle);

        kp.clear();
        // Don't clear peers! mDNS has already discovered them. 
        // Just mark them as untrusted so they show up in "Nearby Networks".
        for p in peers.values_mut() {
            p.is_trusted = false;
            // Note: their network_name remains whatever it was (e.g. "OldNetwork").
            // Since our network_name changes below, they will rightfully be Foreign.
        }
        let mut ck = state.cluster_key.lock().unwrap();
        // Generate new Cluster Key for the new network
        let mut new_key = [0u8; 32];
        rand::thread_rng().fill(&mut new_key);
        *ck = Some(new_key.to_vec());
        save_cluster_key(app_handle, &new_key);
        
        // Clear Handshake State
        state.pending_handshakes.lock().unwrap().clear();
        state.handshake_sessions.lock().unwrap().clear();
        
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
        .plugin(tauri_plugin_notification::init())
        .manage(AppState::new())
        .setup(|app| {
// Initialize QUIC Transport (Fixed Port 4654 for Discovery, or random fallback)
            let transport = tauri::async_runtime::block_on(async { 
                match Transport::new(4654) {
                    Ok(t) => Ok(t),
                    Err(e) => {
                        println!("Failed to bind port 4654 ({}). Falling back to random port.", e);
                        Transport::new(0)
                    }
                }
            }).expect("Failed to create transport");

            let port = transport.local_addr().expect("Failed to get port").port();
            println!("QUIC Transport listening on port {}", port);

            let app_handle = app.handle();

            // Load State
            {
                let state = app.state::<AppState>();

                // 1. Load Cluster Key
                let mut ck_lock = state.cluster_key.lock().unwrap();
                if let Some(key) = load_cluster_key(app_handle) {
                    *ck_lock = Some(key);
                } else {
                    println!("No Cluster Key found. Generating new one...");
                    let mut new_key = [0u8; 32];
                    rand::thread_rng().fill(&mut new_key);
                    save_cluster_key(app_handle, &new_key);
                    *ck_lock = Some(new_key.to_vec());
                }

                // 2. Load Known Peers
                let mut kp_lock = state.known_peers.lock().unwrap();
                *kp_lock = load_known_peers(app_handle);
                
                // Load known peers into RUNTIME state too! (Fixes UI not showing known peers on restart)
                // 3. Load Device ID
                let mut device_id = load_device_id(app_handle);
                if device_id.is_empty() {
                    let run_id: u32 = rand::thread_rng().gen();
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
                // 3c. Load Network PIN
                let network_pin = load_network_pin(app_handle);
                *state.network_pin.lock().unwrap() = network_pin.clone();
                println!("Network PIN: {}", network_pin);

                // 3d. Load Settings
                let settings = load_settings(app_handle);
                *state.settings.lock().unwrap() = settings;
                println!("Loaded Settings");

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

                                    // Extract hostname from property or fallback to mDNS hostname
                                    let h_prop = info.get_property_val_str("h");
                                    let hostname_prop = h_prop
                                        .or_else(|| info.get_property_val_str("hostname"))
                                        .map(|s| s.to_string())
                                        .unwrap_or_else(|| info.get_hostname().to_string());

                                    println!("[Discovery] Peer {} resolved. 'h' prop: {:?}, Final hostname: {}", id, h_prop, hostname_prop);

                                    let peer = Peer {
                                        id: id.clone(),
                                        ip: ip.to_string().parse().unwrap_or(std::net::IpAddr::V4(
                                            std::net::Ipv4Addr::new(127, 0, 0, 1),
                                        )),
                                        port: info.get_port(),
                                        hostname: hostname_prop,
                                        last_seen: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs(),
                                        is_trusted: is_known,
                                        is_manual: false, // Discovered via mDNS
                                        network_name: network_name_prop,
                                        signature: None,
                                    };

                                    d_state.add_peer(peer.clone());
                                    let _ = d_handle.emit("peer-update", &peer);

                                    // Trigger Notification
                                    {
                                        let should_notify = {
                                            let local_net = d_state.network_name.lock().unwrap();
                                            if let Some(remote_net) = &peer.network_name {
                                                *remote_net == *local_net
                                            } else {
                                                false
                                            }
                                        };

                                        if should_notify {
                                            if d_state.settings.lock().unwrap().notifications.device_join {
                                                println!("[Notification] Triggering 'New Device Found' for discovered peer: {}", peer.hostname);
                                                send_notification(&d_handle, "New Device Found", &format!("{} has joined your cluster", peer.hostname));
                                            } else {
                                                println!("[Notification] Device join notification suppressed by settings for discovered peer: {}", peer.hostname);
                                            }
                                        } else {
                                            // println!("[Notification] suppressed - different cluster name.");
                                        }
                                    }
                                    // Lock drops here
                                }
                            }
                            mdns_sd::ServiceEvent::ServiceRemoved(_ty, fullname) => {
                                let id =
                                    fullname.split('.').next().unwrap_or("unknown").to_string();
                                println!("[Discovery] Service Removed: {} -> ID: {}", fullname, id);
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
                                                    
                                                    // Check Auto-Receive Setting
                                                    let auto_receive = listener_state.settings.lock().unwrap().auto_receive;
                                                    
                                                    println!("Decrypted Clipboard: {}", text);
                                                    
                                                    if auto_receive {
                                                        clipboard::set_clipboard(text.clone());
                                                    } else {
                                                        println!("Auto-receive disabled. Content not set locally.");
                                                    }
                                                    
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
                                                                             signature: None,
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
                                // TRUST THE PACKET SOURCE for IP/Port (fixes 0.0.0.0 issue)
                                peer.ip = addr.ip();
                                peer.port = addr.port();
                                peer.last_seen = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                                peer.is_manual = true; 
                                
                                let mut should_reply = false;
                                {
                                     let mut kp_lock = listener_state.known_peers.lock().unwrap();
                                     // If we don't know them, OR if we only know them as a manual placeholder...
                                     // Check for placeholder? ID is "manual-IP". Real ID is UUID.
                                     // If we have a manual placeholder for this IP, remove it.
                                     let manual_id = format!("manual-{}", peer.ip);
                                     if kp_lock.contains_key(&manual_id) {
                                         println!("Replacing manual placeholder {} with real peer {}", manual_id, peer.id);
                                         kp_lock.remove(&manual_id);
                                         listener_state.peers.lock().unwrap().remove(&manual_id);
                                         let _ = listener_handle.emit("peer-remove", &manual_id);
                                         should_reply = true; // We initiated this connection via placeholder, so we should finish the handshake
                                     }
                                     
                                     // If we don't know them at all, reply!
                                     // If we don't know them at all, reply!
                                     if !kp_lock.contains_key(&peer.id) {
                                         should_reply = true;
                                     }

                                     // Trust Arbitration
                                     // Rule: Trust is strictly based on Key Possession (Signature).
                                     // If signature is valid -> Trust (Grant or Renew).
                                     // If signature is invalid/missing -> Distrust (Revoke or Ignore).
                                     
                                     let mut is_signature_valid = false;
                                     if let Some(sig) = &peer.signature {
                                         if let Some(key_vec) = listener_state.cluster_key.lock().unwrap().as_ref() {
                                             if key_vec.len() == 32 {
                                                 let mut key_arr = [0u8; 32];
                                                 key_arr.copy_from_slice(key_vec);
                                                 if verify_signature(&key_arr, &peer.id, sig) {
                                                     is_signature_valid = true;
                                                 }
                                             }
                                         }
                                     }
                                     
                                     if is_signature_valid {
                                         // Valid Signature: We TRUST this peer.
                                         println!("Verified Signature for {}! Trust maintained/granted.", peer.id);
                                         peer.is_trusted = true;
                                     } else {
                                         // Invalid/Missing Signature: We DO NOT TRUST this peer.
                                         // If we previously trusted them, this REVOKES trust (e.g. they reset/left).
                                         if let Some(existing) = kp_lock.get(&peer.id) {
                                             if existing.is_trusted {
                                                println!("Revoking Trust for {}: Invalid/Missing Signature.", peer.id);
                                             }
                                         }
                                         peer.is_trusted = false;
                                     }

                                     // Insert the Real Peer (Update info even if known)
                                     kp_lock.insert(peer.id.clone(), peer.clone());
                                     save_known_peers(listener_handle.app_handle(), &kp_lock);
                                     listener_state.add_peer(peer.clone());
                                     let _ = listener_handle.emit("peer-update", &peer);
                                }
                                
                                // Send OUR info back so they can update their placeholder!
                                if should_reply {
                                    println!("Sending Discovery Reply to {}", addr);
                                    let local_id = listener_state.local_device_id.lock().unwrap().clone();
                                    let hostname = hostname::get().map(|h| h.to_string_lossy().to_string()).unwrap_or("Unknown".to_string());
                                    let network_name = listener_state.network_name.lock().unwrap().clone();
                                    
                                    let mut signature = None;
                                    if let Some(key_vec) = listener_state.cluster_key.lock().unwrap().as_ref() {
                                        if key_vec.len() == 32 {
                                            let mut key_arr = [0u8; 32];
                                            key_arr.copy_from_slice(key_vec);
                                            signature = generate_signature(&key_arr, &local_id);
                                        }
                                    }
                                    
                                    let my_peer = Peer {
                                        id: local_id,
                                        // payload IP is 0.0.0.0 if bound to all, but receiver will fix it using addr.ip() as above!
                                        ip: transport_inside.local_addr().unwrap().ip(),
                                        port: transport_inside.local_addr().unwrap().port(),
                                        hostname,
                                        last_seen: 0,
                                        is_trusted: false, // We aren't trusted yet
                                        is_manual: true,
                                        network_name: Some(network_name),
                                        signature,
                                    };
                                    
                                    let msg = Message::PeerDiscovery(my_peer);
                                    let data = serde_json::to_vec(&msg).unwrap_or_default();
                                    // Send back to sender
                                    tauri::async_runtime::spawn(async move {
                                        let _ = transport_inside.send_message(addr, &data).await;
                                    });
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

            // Background Task: Heartbeat (Keep Manual Peers Alive)

            let hb_state = (*app.state::<AppState>()).clone();
            let hb_transport = transport.clone();

            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                    
                    let peers: Vec<Peer> = {
                        // FIX: Heartbeat ALL runtime peers, not just known (connected) ones.
                        // This prevents pruning of discovered-but-not-yet-trusted peers.
                        let peers_map = hb_state.get_peers();
                        peers_map.values().cloned().collect()
                    };

                    if peers.is_empty() { continue; }

                    let local_id = hb_state.local_device_id.lock().unwrap().clone();
                    let hostname = hostname::get().map(|h| h.to_string_lossy().to_string()).unwrap_or("Unknown".to_string());
                    let network_name = hb_state.network_name.lock().unwrap().clone();

                    let mut signature = None;
                    if let Some(key_vec) = hb_state.cluster_key.lock().unwrap().as_ref() {
                        if key_vec.len() == 32 {
                            let mut key_arr = [0u8; 32];
                            key_arr.copy_from_slice(key_vec);
                            signature = generate_signature(&key_arr, &local_id);
                        }
                    }

                    // Self Peer (for payload)
                    let my_peer = Peer {
                        id: local_id,
                        ip: hb_transport.local_addr().unwrap().ip(),
                        port: hb_transport.local_addr().unwrap().port(),
                        hostname,
                        last_seen: 0,
                        is_trusted: false, 
                        is_manual: true,
                        network_name: Some(network_name),
                        signature,
                    };
                    
                    let msg = Message::PeerDiscovery(my_peer);
                    let data = serde_json::to_vec(&msg).unwrap_or_default();

                    for p in peers {
                        // Don't ping self (shouldn't be in list, but sanity check)
                        let addr = std::net::SocketAddr::new(p.ip, p.port);
                        
                        // We skip sending if wait, we want to broadcast to everyone we know.
                        let _ = hb_transport.send_message(addr, &data).await;
                    }
                }
            });

            // Background Task: Pruning (Remove Stale Untrusted Peers)
            let prune_handle = app.handle().clone();
            let prune_state = (*app.state::<AppState>()).clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                    let timeout = 60; // 60 seconds timeout

                    let mut params_to_remove = Vec::new();
                    
                    {
                        let peers = prune_state.get_peers();
                        for (id, p) in peers {
                            // Prune ANY peer that hasn't been seen in timeout seconds
                            if now - p.last_seen > timeout {
                                println!("Pruning stale peer: {} ({}) - Last seen {}s ago", p.hostname, id, now - p.last_seen);
                                params_to_remove.push((id, p.is_trusted));
                            }
                        }
                    }

                    if !params_to_remove.is_empty() {
                         let mut peers_lock = prune_state.peers.lock().unwrap();
                         let mut kp_lock = prune_state.known_peers.lock().unwrap();
                         
                         for (id, was_trusted) in params_to_remove {
                             // Always remove from RUNTIME peers (UI)
                             peers_lock.remove(&id);

                             // If Untrusted, forget them completely.
                             // If Trusted, KEEP them in known_peers (Reverse Discovery)
                             if !was_trusted {
                                 kp_lock.remove(&id);
                             }
                             
                             let _ = prune_handle.emit("peer-remove", &id);
                         }
                         save_known_peers(prune_handle.app_handle(), &kp_lock);
                    }
                }
            });

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
            get_network_pin,
            get_device_id,
            get_hostname,
            get_settings,
            save_settings,
            set_network_identity,
            regenerate_network_identity,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle: &tauri::AppHandle, event: tauri::RunEvent| {
        match event {
            tauri::RunEvent::Exit => {
                println!("App exiting, dropping discovery service...");
                let state = app_handle.state::<AppState>();
                let mut discovery = state.discovery.lock().unwrap();
                *discovery = None; // Explicitly drop to trigger unregister
            }
            _ => {}
        }
    });
}
