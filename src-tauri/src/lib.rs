mod clipboard;
#[cfg(target_os = "linux")]
mod dbus;
mod crypto;
mod discovery;
mod peer;
mod protocol;
mod state;
mod storage;
mod transport;
mod tray;

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState, ShortcutEvent};

use tauri_plugin_clipboard::Clipboard;
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncBufReadExt, BufReader};
use std::str::FromStr;
use std::path::PathBuf;
use tokio::fs::File;
use crate::protocol::Message;


#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, default_value = "debug")]
    log_level: String,
}

fn init_logging() {
    // 1. Parse CLI Args (ignoring unknown args that Tauri might use)
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(_) => {
            // Keep default if parsing fails (e.g. extra args)
            Args { log_level: "debug".to_string() }
        }
    };

    let level = match args.log_level.to_lowercase().as_str() {
        "error" => tracing::Level::ERROR,
        "warn" => tracing::Level::WARN,
        "info" => tracing::Level::INFO,
        "debug" => tracing::Level::DEBUG,
        "trace" => tracing::Level::TRACE,
        _ => tracing::Level::DEBUG,
    };

    // 2. Setup Stdout Layer (Colored)
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false) // Don't show target (module path) for cleaner output? Or maybe show it.
        .with_ansi(true)
        .compact(); // Compact format

    // 3. Setup File Layer (Rolling Daily)
    // We need a path. Since we are before AppHandle, we can't easily get AppDataDir.
    // We'll trust XDG or standard paths or just current dir for now?
    // User requested "sinks".
    // Better to use `tauri::api::path::app_log_dir`? No, we don't have app handle yet.
    // Let's us `directories` crate? Or just `.logs` in CWD for development as requested?
    // "We need each log line to be timestamped, and include hostname."
    
    // Use temp_dir for logs to ensure we can write even if CWD is / (macOS Bundle)
    let log_dir = std::env::temp_dir().join("ClusterCutLogs");
    let file_appender = tracing_appender::rolling::daily(&log_dir, "clustercut.log");
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_appender)
        .with_ansi(false)
        .with_target(true);

    // 4. Init Registry
    // Base Level: INFO (for external crates) + User Level for US
    let filter = tracing_subscriber::EnvFilter::new("info")
        .add_directive(format!("tauri_app={}", args.log_level.to_lowercase()).parse().unwrap())
        // Silence noisy networking crates
        .add_directive("rustls=warn".parse().unwrap())
        .add_directive("quinn=warn".parse().unwrap())
        .add_directive("zbus=warn".parse().unwrap()); 

    tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();
        
    tracing::info!("Logging initialized. Level: {}, Hostname: {}", level, get_hostname_internal());
}

fn get_hostname_internal() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string())
}
use discovery::Discovery;
use peer::Peer;
use rand::Rng;
use state::AppState;
use storage::{
    load_cluster_key, load_device_id, load_known_peers, load_network_name, load_network_pin,
    save_cluster_key, save_device_id, save_known_peers, save_network_name, save_network_pin,
    reset_network_state, load_settings, AppSettings,
};
use tauri::{Emitter, Manager};
use transport::Transport;
// use tauri_plugin_notification::NotificationExt;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

// Helper to broadcast a new peer to all known peers (Gossip)
pub(crate) fn send_notification(app_handle: &tauri::AppHandle, title: &str, body: &str, increment_badge: bool, _id: Option<i32>) {
    // 1. Native Plugin (Windows/macOS)
    #[cfg(not(target_os = "linux"))]
    {
        use tauri_plugin_notification::NotificationExt;
        let mut builder = app_handle.notification().builder()
            .title(title)
            .body(body)
            .sound("Ping");
            
        if let Some(id_val) = _id {
            builder = builder.id(id_val).group("ClusterCut");
        }
            
        // Note: The native plugin might auto-increment badge on macOS depending on permission config.
        // There is no explicit "increment_badge" builder method in v2 plugin yet documented, 
        // but we can manually set the count if needed.
        if increment_badge {
             // For now, let's assume default behavior is acceptable or we use app.badging() if available.
             // But the user specifically asked to NOT show it for others.
             // If the plugin defaults to showing it, we might need to clear it?
             // Leaving as standard plugin behavior for now, can refine if MacOS users also complain.
        }
        
        if let Err(e) = builder.show() {
             tracing::error!("[Notification Error] Native plugin failed: {}", e);
        }
    }
    
    // 2. Linux Workaround (notify-rust via DBus)
    #[cfg(target_os = "linux")]
    {
        use notify_rust::Notification;
        tracing::debug!("[Notification] Linux detected. Using notify-rust via DBus...");
        
        let title = title.to_string();
        let body = body.to_string();
        let app = app_handle.clone();
        // Spawn to avoid blocking
        tauri::async_runtime::spawn(async move {
            let mut notification = Notification::new();
            notification
                .summary(&title)
                .body(&body)
                .appname("ClusterCut")
                .timeout(notify_rust::Timeout::Milliseconds(5000));
            
            // Ubuntu/Dock Badge Logic:
            // "Transient" hint often prevents the notification from persisting in the history/dock.
            // If we DO NOT want to increment the badge (increment_badge = false), we set Transient(true).
            if !increment_badge {
                notification.hint(notify_rust::Hint::Transient(true));
            } else {
                notification.hint(notify_rust::Hint::Transient(false));
            }
            
            // If it's a "File Available" type notification (which implies we want action)
            // We can add an action.
            // For now, we add "Open" action for everything, or we can make it conditional.
            // User requested click to open.
            notification.action("default", "Open");

            // Hints for Badge logic (Linux doesn't have standard badges, but some docks do)
            // We'll ignore badge increment on Linux for now as it doesn't map cleanly to DBus notify,
            // unless we use specific extension hints.
            
            match notification.show_async().await {
                Ok(handler) => {
                    tracing::debug!("[Notification] Notification sent successfully.");
                    // Setup listener for action (blocking, involves closure)
                    handler.wait_for_action(move |action| {
                         tracing::info!("[Notification] Action Invoked: {:?}", action);
                         if action == "default" || action == "Open" {
                             // Emit event to frontend to navigate
                             tracing::info!("Emitting 'notification-clicked' event");
                             let _ = app.emit("notification-clicked", ());
                             
                             // Also focus window? 
                             let _ = app.get_webview_window("main").map(|w| w.set_focus());
                         }
                    });
                },
                Err(e) => {
                    tracing::error!("[Notification Error] notify-rust failed: {}", e);
                }
            }
        });
    }
}

fn check_and_notify_leave(app_handle: &tauri::AppHandle, state: &AppState, peer: &Peer) {
    let notifications = state.settings.lock().unwrap().notifications.clone();
    if notifications.device_leave {
        let local_net = state.network_name.lock().unwrap().clone();
        if let Some(remote_net) = &peer.network_name {
            if *remote_net == local_net {
                tracing::info!("[Notification] Device Left: {}", peer.hostname);
                send_notification(app_handle, "Device Left", &format!("{} has left the cluster", peer.hostname), false, Some(1));
            }
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
                tracing::error!("Failed to gossip peer to {}: {}", addr, e);
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
    let _ = app_handle.emit("settings-changed", settings.clone());
    
    #[cfg(desktop)]
    crate::tray::update_tray_menu(&app_handle);
    
    // Update Shortcuts
    register_shortcuts(&app_handle);
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
    let _data = serde_json::to_vec(&msg).unwrap_or_default();
    
            tracing::debug!("Probing {}...", addr);
            
            // Send Peer Discovery via QUIC/UDP
            let data_vec = _data.clone();
            let transport_clone = transport.clone();
            
            // We use a small timeout for the send operation
            let send_future = async move {
                 transport_clone.send_message(addr, &data_vec).await
            };
            
            match tokio::time::timeout(std::time::Duration::from_millis(500), send_future).await {
                Ok(Ok(())) => {
                   tracing::debug!("Probe to {} SUCCESS (Packet Sent)", addr);
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
                         };
                         peers.insert(id.clone(), peer.clone());
                         let _ = app_handle.emit("peer-update", &peer);
                         
                          let notifications = state.settings.lock().unwrap().notifications.clone();
                          if notifications.device_join {
                             tracing::info!("[Notification] Triggering 'Device Joined' for manual peer: {}", peer.hostname);
                             send_notification(&app_handle, "Device Joined", &format!("Found manual peer: {}", peer.hostname), false, Some(1));
                          }
                     }
                },
                Ok(Err(e)) => {
                    tracing::debug!("Probe to {} failed: {}", addr, e);
                },
                Err(_) => {
                    tracing::debug!("Probe to {} timed out.", addr);
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
        crypto::start_spake2(&pin, "clustercut-connect", "clustercut-connect").map_err(|e| e.to_string())?;

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

    // 2. Update Runtime State
    {
        let mut kp = state.known_peers.lock().unwrap();
        let mut peers = state.peers.lock().unwrap();
        let mut ck = state.cluster_key.lock().unwrap();
        let mut ph = state.pending_handshakes.lock().unwrap();
        let mut hs = state.handshake_sessions.lock().unwrap();
        let mut nn = state.network_name.lock().unwrap();
        let mut np = state.network_pin.lock().unwrap();

        kp.clear();
        // Mark peers untrusted
        for p in peers.values_mut() {
            p.is_trusted = false;
        }
        
        // Generate new Cluster Key
        let mut new_key = [0u8; 32];
        rand::thread_rng().fill(&mut new_key);
        *ck = Some(new_key.to_vec());
        save_cluster_key(app_handle, &new_key);
        
        ph.clear();
        hs.clear();

        // Load new identity (generated by accessors if missing)
        let new_name_val = load_network_name(app_handle);
        let new_pin_val = load_network_pin(app_handle);
        
        *nn = new_name_val.clone();
        *np = new_pin_val.clone();
        
        tracing::info!("Reset to New Network: {} (PIN: {})", new_name_val, new_pin_val);
    }
    
    // 3. Re-register mDNS
    {
        let local_id = state.local_device_id.lock().unwrap().clone();
        let new_name = state.network_name.lock().unwrap().clone();
        if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
             let _ = discovery.register(&local_id, &new_name, port);
        }
    }
    
    // 4. Notify Frontend
    let _ = app_handle.emit("network-reset", ());
}

#[tauri::command]
async fn send_clipboard(
    text: String,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    
    // Manual Send Command
    clipboard::set_clipboard(&app_handle, text.clone()); // Update local clipboard too? Yes, usually.
    
    // Construct Payload
    let local_id = state.local_device_id.lock().unwrap().clone();
    let hostname = get_hostname_internal();
    let msg_id = uuid::Uuid::new_v4().to_string();
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();

    let payload_obj = crate::protocol::ClipboardPayload {
        id: msg_id.clone(),
        text: text.clone(),
        timestamp: ts,
        sender: hostname,
        sender_id: local_id,
        files: None,
    };

    // Emit local event so history updates
    let _ = app_handle.emit("clipboard-change", &payload_obj);

    // Encrypt & Send
    let ck_lock = state.cluster_key.lock().unwrap();
    if let Some(key) = ck_lock.as_ref() {
        if key.len() == 32 {
             let mut key_arr = [0u8; 32];
             key_arr.copy_from_slice(key);
             let json_payload = serde_json::to_vec(&payload_obj).map_err(|e| e.to_string())?;
             
             match crypto::encrypt(&key_arr, &json_payload) {
                 Ok(cipher) => {
                     let msg = Message::Clipboard(cipher);
                     let data = serde_json::to_vec(&msg).map_err(|e| e.to_string())?;
                     
                     let peers = state.get_peers();
                     for p in peers.values() {
                         let addr = std::net::SocketAddr::new(p.ip, p.port);
                         let transport_clone = (*transport).clone();
                         let data_vec = data.clone();
                         tauri::async_runtime::spawn(async move {
                             if let Err(e) = transport_clone.send_message(addr, &data_vec).await {
                                 tracing::error!("[Clipboard] Failed to send to {}: {}", addr, e);
                             } else {
                                 tracing::debug!("[Clipboard] Sent to {}", addr);
                             }
                         });
                     }
                     
                     // Notify locally
                     let notifications = state.settings.lock().unwrap().notifications.clone();
                     if notifications.data_sent {
                         send_notification(&app_handle, "Clipboard Sent", "Manual broadcast successful.", false, Some(2));
                     }
                     
                     Ok(())
                 },
                 Err(e) => Err(format!("Encryption failed: {}", e))
             }
        } else {
            Err("Invalid Cluster Key".to_string())
        }
    } else {
        Err("No Cluster Key set".to_string())
    }
}

#[tauri::command]
async fn delete_history_item(
    id: String,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
) -> Result<(), String> {
    let msg = Message::HistoryDelete(id);
    let data = serde_json::to_vec(&msg).map_err(|e| e.to_string())?;
    
    let peers = state.get_peers();
    for p in peers.values() {
         let addr = std::net::SocketAddr::new(p.ip, p.port);
         let transport_clone = (*transport).clone();
         let data_vec = data.clone();
         tauri::async_runtime::spawn(async move {
             let _ = transport_clone.send_message(addr, &data_vec).await;
         });
    }
    Ok(())
}

#[tauri::command]
async fn set_local_clipboard(app: tauri::AppHandle, text: String) -> Result<(), String> {
    clipboard::set_clipboard(&app, text);
    Ok(())
}

#[tauri::command]
async fn confirm_pending_clipboard(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let pending_opt = {
        let mut lock = state.pending_clipboard.lock().unwrap();
        lock.take() // Take it (clearing it)
    };

    if let Some(payload) = pending_opt {
        tracing::info!("Confirming pending clipboard from {}", payload.sender);
        clipboard::set_clipboard(&app_handle, payload.text.clone());
        
        // Emit change event so history updates
        let _ = app_handle.emit("clipboard-change", &payload);
        
        Ok(())
    } else {
        Err("No pending clipboard content".to_string())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    
    // Initialize Logging
    init_logging();
    
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec![])))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().with_handler(handle_shortcut).build())
        .manage(AppState::new())
        .setup(|app| {
            // Initialize QUIC Transport (Fixed Port 4654 for Discovery, or random fallback)
            let transport = tauri::async_runtime::block_on(async { 
                match Transport::new(4654) {
                    Ok(t) => Ok(t),
                    Err(e) => {
                        tracing::warn!("Failed to bind port 4654 ({}). Falling back to random port.", e);
                        Transport::new(0)
                    }
                }
            }).expect("Failed to create transport");

            let port = transport.local_addr().expect("Failed to get port").port();
            tracing::info!("QUIC Transport listening on port {}", port);

            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            #[cfg(target_os = "linux")]
            {
                 // Explicitly enforce window settings on Linux/Flatpak to ensure WM respects them
                 if let Some(window) = app.get_webview_window("main") {
                     let _ = window.set_maximizable(false);
                     // Match native GNOME behavior: hide minimize button on Linux (Flatpak usually shows it by default otherwise)
                     let _ = window.set_minimizable(false); 
                 }
            }

            let app_handle = app.handle();
            
            #[cfg(desktop)]
            {
                let _ = crate::tray::create_tray(&app_handle);
            }

            #[cfg(target_os = "linux")]
            {
                let dbus_handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                     if let Err(e) = crate::dbus::start_dbus_server(dbus_handle).await {
                         tracing::error!("Failed to start D-Bus service: {}", e);
                     }
                });
            }

            // Load State
            {
                let state = app.state::<AppState>();

                // 1. Load Cluster Key
                let mut ck_lock = state.cluster_key.lock().unwrap();
                if let Some(key) = load_cluster_key(app_handle) {
                    *ck_lock = Some(key);
                } else {
                    tracing::info!("No Cluster Key found. Generating new one...");
                    let mut new_key = [0u8; 32];
                    rand::thread_rng().fill(&mut new_key);
                    save_cluster_key(app_handle, &new_key);
                    *ck_lock = Some(new_key.to_vec());
                }

                // 2. Load Known Peers
                let mut kp_lock = state.known_peers.lock().unwrap();
                *kp_lock = load_known_peers(app_handle);
                
                
                // 4. Load Settings
                let mut settings_lock = state.settings.lock().unwrap();
                *settings_lock = load_settings(app_handle);
                drop(settings_lock); // Unlock to allow registration to access it if needed (though register_shortcuts locks it again)
                
                // Register Shortcuts on Startup
                register_shortcuts(app_handle);
                let mut device_id = load_device_id(app_handle);
                if device_id.is_empty() {
                    let run_id: u32 = rand::thread_rng().gen();
                    device_id = format!("clustercut-{}", run_id);
                    save_device_id(app_handle, &device_id);
                    tracing::info!("Generated new Device ID: {}", device_id);
                } else {
                    tracing::info!("Loaded Device ID: {}", device_id);
                }
                *state.local_device_id.lock().unwrap() = device_id.clone();
                
                // 3b. Load Network Name (for mDNS)
                let network_name = load_network_name(app_handle);
                *state.network_name.lock().unwrap() = network_name.clone();

                // 3c. Load Network PIN
                let network_pin = load_network_pin(app_handle);
                *state.network_pin.lock().unwrap() = network_pin.clone();
                tracing::info!("Network PIN: {}", network_pin);

                // 3d. Load Settings
                let settings = load_settings(app_handle);
                *state.settings.lock().unwrap() = settings;
                tracing::info!("Loaded Settings");

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

                                    // DEBOUNCE: Cancel any pending removal for this peer
                                    {
                                        let mut pending = d_state.pending_removals.lock().unwrap();
                                        if pending.remove(&id).is_some() {
                                            tracing::debug!("[Discovery] Debounce: Cancelled pending removal for reappearing peer {}", id);
                                        }
                                    }

                                    let network_name_prop = info
                                        .get_property_val_str("n")
                                        .map(|s| s.to_string());
                                    
                                    if let Some(n) = &network_name_prop {
                                        tracing::debug!("Discovered peer {} with network name: {}", id, n);
                                    } else {
                                        tracing::warn!("Discovered peer {} WITHOUT network name (properties: {:?})", id, info.get_properties());
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

                                    tracing::info!("[Discovery] Peer {} resolved. 'h' prop: {:?}, Final hostname: {}", id, h_prop, hostname_prop);

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
                                                tracing::info!("[Notification] Triggering 'Device Joined' for discovered peer: {}", peer.hostname);
                                                send_notification(&d_handle, "Device Joined", &format!("{} has joined your cluster", peer.hostname), false, Some(1));
                                            } else {
                                                tracing::debug!("[Notification] Device join notification suppressed by settings for discovered peer: {}", peer.hostname);
                                            }                                      } else {
                                            // tracing::debug!("[Notification] suppressed - different cluster name.");
                                        }
                                    }
                                    // Lock drops here
                                }

                            }
                            mdns_sd::ServiceEvent::ServiceRemoved(_ty, fullname) => {
                                let id =
                                    fullname.split('.').next().unwrap_or("unknown").to_string();
                                tracing::info!("[Discovery] Service Removed: {} -> ID: {}", fullname, id);
                                
                                // Safety Check: If we effectively just saw this peer (in the last 2 seconds),
                                // ignore this removal as a "phantom" or out-of-order packet.
                                // This happens often when devices re-announce themselves.
                                {
                                    let peers = d_state.peers.lock().unwrap();
                                    if let Some(peer) = peers.get(&id) {
                                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                                        if now.saturating_sub(peer.last_seen) < 2 {
                                             tracing::warn!("[Discovery] Ignoring ServiceRemoved for {} (seen {}s ago) - likely phantom.", id, now.saturating_sub(peer.last_seen));
                                             return;
                                        }
                                    }
                                }

                                // DEBOUNCE: Don't remove immediately. Wait 8 seconds.
                                let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_micros() as u64;
                                {
                                    let mut pending = d_state.pending_removals.lock().unwrap();
                                    pending.insert(id.clone(), nonce);
                                }
                                
                                let r_state = d_state.clone();
                                let r_handle = d_handle.clone();
                                let r_id = id.clone();
                                
                                tauri::async_runtime::spawn(async move {
                                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                                    
                                    let mut pending = r_state.pending_removals.lock().unwrap();
                                    if let Some(n) = pending.get(&r_id) {
                                        if *n == nonce {
                                            // Confirmed! Still pending and nonce matches (not overwritten by newer removal?)
                                            pending.remove(&r_id);
                                            drop(pending); // Drop lock
                                            
                                            // Proceed with removal
                                            {
                                                tracing::info!("[Discovery] Debounce expired. Removing peer {}", r_id);
                                                let mut peers = r_state.peers.lock().unwrap();
                                                if let Some(peer) = peers.remove(&r_id) {
                                                     drop(peers); // Drop lock before notifying
                                                     check_and_notify_leave(&r_handle, &r_state, &peer);
                                                }
                                            }
                                            let _ = r_handle.emit("peer-remove", &r_id);
                                        } else {
                                            tracing::debug!("[Discovery] Removal Debounce cancelled (Nonce mismatch) for {}", r_id);
                                        }
                                    } else {
                                        tracing::debug!("[Discovery] Removal Debounce cancelled (Entry gone) for {}", r_id);
                                    }
                                });
                            }
                            _ => {}
                        }
                    }
                });
            }

            // Clones for transport listener
            let listener_handle = app.handle().clone();
            let listener_state = (*app.state::<AppState>()).clone();

            {
                let mut t_lock = listener_state.transport.lock().unwrap();
                *t_lock = Some(transport.clone());
            }

            app.manage(transport.clone());

            // Start Listening
            // Start Listening
            let transport_inside = transport.clone();
            let file_state = listener_state.clone();
            let file_handle = listener_handle.clone();

            transport.start_listening(
                move |data, addr| {
                    tracing::trace!("Received {} bytes from {}", data.len(), addr);
                    let listener_handle = listener_handle.clone();
                    let listener_state = listener_state.clone();
                    let transport_inside = transport_inside.clone();

                    // ... Existing Message Handler Code ...
                    tauri::async_runtime::spawn(async move {
                         match serde_json::from_slice::<Message>(&data) {
                             Ok(msg) => handle_message(msg, addr, listener_state, listener_handle, transport_inside).await,
                             Err(e) => tracing::error!("Failed to parse message: {}", e), 
                         }
                    });
                },
                move |recv, addr| {
                    tracing::info!("Received FILE stream from {}", addr);
                    let state = file_state.clone();
                    let handle = file_handle.clone();
                    
                    tauri::async_runtime::spawn(async move {
                         handle_incoming_file_stream(recv, addr, state, handle).await;
                    });
                }
            );
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
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    
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
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                    let timeout = 300; // 300 seconds (5 minutes) timeout to allow for network hiccups

                    // Fix Deadlock: Acquire known_peers FIRST, then peers.
                    // This matches perform_factory_reset and PeerDiscovery.
                    let mut kp_lock = prune_state.known_peers.lock().unwrap();
                    let mut peers_lock = prune_state.peers.lock().unwrap();

                    let mut to_remove = Vec::new();
                    
                    // Iterate over peers to find stale ones
                    for (id, p) in peers_lock.iter() {
                        if now - p.last_seen > timeout {
                            tracing::info!("Pruning stale peer: {} ({}) - Last seen {}s ago", p.hostname, id, now - p.last_seen);
                            to_remove.push(p.clone());
                        }
                    }

                    if !to_remove.is_empty() {
                         for peer in to_remove {
                             let id = peer.id.clone();
                             let was_trusted = peer.is_trusted;

                             // Always remove from RUNTIME peers (UI)
                             peers_lock.remove(&id);

                             // If Untrusted, forget them completely.
                             // If Trusted, KEEP them in known_peers (Reverse Discovery)
                             if !was_trusted {
                                 kp_lock.remove(&id);
                             }
                             
                             check_and_notify_leave(&prune_handle, &prune_state, &peer);
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
            request_file,
            delete_history_item,
            check_gnome_extension_status,
            get_network_pin,
            get_device_id,
            get_hostname,
            get_settings,
            save_settings,
            set_network_identity,
            regenerate_network_identity,
            send_clipboard,
            set_local_clipboard,
            confirm_pending_clipboard,
        ])
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                     // Minimize to Tray behavior
                     let _ = window.hide();
                     api.prevent_close();
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle: &tauri::AppHandle, event: tauri::RunEvent| {
        match event {
            tauri::RunEvent::WindowEvent { event: tauri::WindowEvent::Focused(true), .. } => {
                // Clear badge on focus
                #[cfg(target_os = "linux")]
                {
                    // Linux does not have a standard way to clear badges via notification hints 
                    // that is consistent across all DEs without side effects (like empty notifications).
                    // We simply do nothing here for now to avoid the "Empty Notification" bug.
                }
                #[cfg(desktop)]
                {
                     // Clear custom tray badge
                     crate::tray::set_badge(app_handle, false);
                }

                #[cfg(target_os = "macos")]
                {
                     // In Tauri v2, badge API is often on the window or requires trait.
                     // We use the main window.
                     use tauri::Manager; // Ensure Manager trait is in scope for get_webview_window
                     if let Some(window) = app_handle.get_webview_window("main") {
                         let _ = window.set_badge_count(Some(0));
                     }
                }
            }
            tauri::RunEvent::Exit => {
                tracing::info!("App exiting, signaling shutdown to background threads...");
                let state = app_handle.state::<AppState>();

                // Signal shutdown to background threads FIRST
                // This allows the clipboard monitor to exit gracefully before cleanup
                state.request_shutdown();

                // Give threads a moment to notice the shutdown signal
                std::thread::sleep(std::time::Duration::from_millis(100));

                tracing::info!("Dropping discovery service...");
                let mut discovery = state.discovery.lock().unwrap();
                *discovery = None; // Explicitly drop to trigger unregister

                // Broadcast Goodbye
                let local_id = state.local_device_id.lock().unwrap().clone();
                let msg = crate::protocol::Message::PeerRemoval(local_id);
                if let Ok(data) = serde_json::to_vec(&msg) {
                    let peers = state.get_peers();
                    tracing::info!("Broadcasting Goodbye to {} peers...", peers.len());
                    
                    // Best effort send (blocking/sync context or fire-and-forget)
                    // Since we are exiting, async runtime might be shutting down.
                    // We can try to spawn on the handle if it's still valid, or just hope.
                    // Actually, 'app_handle' is valid.
                    
                    for p in peers.values() {
                         let addr = std::net::SocketAddr::new(p.ip, p.port);
                         let data_vec = data.clone();
                         // We create a new transport instance or use existing? 
                         // Existing transport is in state, but we need to use it.
                         // Quickest way: spawn and give it a few millis.
                         let t_state = (*state).clone();
                         tauri::async_runtime::spawn(async move {
                             let transport_opt = {
                                 let lock = t_state.transport.lock().unwrap();
                                 lock.clone()
                             };
                             if let Some(transport) = transport_opt {
                                 let _ = transport.send_message(addr, &data_vec).await;
                             }
                         });
                    }
                    // Give a brief moment for packets to fly
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
            }
            _ => {}
        }
    });
}

async fn handle_incoming_file_stream(recv: quinn::RecvStream, addr: std::net::SocketAddr, state: AppState, app: tauri::AppHandle) {
    tracing::info!("Starting File Stream Handler for {}", addr);
    
    let mut reader = BufReader::new(recv);
    let mut header_line = String::new();
    
    // 1. Read Header (JSON + Newline)
    if let Err(e) = reader.read_line(&mut header_line).await {
        tracing::error!("Failed to read file stream header from {}: {}", addr, e);
        return;
    }
    
    let header: crate::protocol::FileStreamHeader = match serde_json::from_str(&header_line) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("Failed to parse file stream header '{}': {}", header_line.trim(), e);
            return;
        }
    };
    
    tracing::info!("Receiving File: {} ({} bytes) [ID: {}]", header.file_name, header.file_size, header.id);
    
    // 2. Prepare Output File
    // Use Cache Directory
    let cache_dir = match app.path().app_cache_dir() {
        Ok(p) => p,
        Err(e) => {
             tracing::error!("Failed to get cache dir: {}", e);
             return;
        }
    };
    
    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        tracing::error!("Failed to create cache dir: {}", e);
        return;
    }
    
    // Use ID/Index subfolder to avoid collisions? Or just flat?
    // Flat for now, verify uniqueness?
    // unique_name = header.file_name
    let file_path = cache_dir.join(&header.file_name);
    // TODO: Handle name collision (append _1, etc)?
    
    let mut file = match File::create(&file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Failed to create file {:?}: {}", file_path, e);
            return;
        }
    };
    
    // 3. Verify Auth Token
    let mut session_key = [0u8; 32];
    {
         let ck_lock = state.cluster_key.lock().unwrap();
         if let Some(key) = ck_lock.as_ref() {
             if key.len() == 32 {
                 session_key.copy_from_slice(key);
             } else {
                 tracing::error!("Cluster Key invalid length!");
                 return;
             }
         } else {
             tracing::error!("Cluster Key missing!");
             return;
         }
    }

    match BASE64.decode(&header.auth_token) {
        Ok(token_cipher) => {
            match crypto::decrypt(&session_key, &token_cipher) {
                Ok(plaintext) => {
                    if plaintext.len() == 8 {
                        // TODO: Verify timestamp freshness if desired
                        tracing::info!("Auth Token Verified. Starting Download...");
                    } else {
                        tracing::error!("Invalid Auth Token length");
                        return;
                    }
                },
                Err(e) => {
                    tracing::error!("Auth Token Decryption Failed: {}", e);
                    return;
                }
            }
        },
        Err(e) => {
            tracing::error!("Invalid Auth Token Base64: {}", e);
            return;
        }
    }

    // 4. Stream Data (Zero-Copy-ish)
    let start_time = std::time::Instant::now();
    
    // reader is BufReader<RecvStream>. We can just copy.
    // However, we want progress updates?
    // tokio::io::copy doesn't give progress.
    // If we want progress, we need a loop, but without length framing.
    // Simple loop: read(buf), write(buf).
    
    let mut buf = vec![0u8; 1024 * 1024]; // 1MB Buffer
    let mut total_written = 0u64;
    let mut last_emit = std::time::Instant::now();
    let mut chunk_count = 0;

    tracing::info!("[Receiver] Starting RAW Stream. Expecting {} bytes.", header.file_size);
    
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break, // EOF
            Ok(n) => {
                if let Err(e) = file.write_all(&buf[0..n]).await {
                     tracing::error!("File Write Error: {}", e);
                     break;
                }
                total_written += n as u64;
                chunk_count += 1;
                
                // Emit Progress (Throttled 200ms)
                if last_emit.elapsed().as_millis() > 200 {
                     let _ = app.emit("file-progress", serde_json::json!({
                         "id": header.id,
                         "fileName": header.file_name,
                         "total": header.file_size,
                         "transferred": total_written
                     }));
                     last_emit = std::time::Instant::now();
                }
            }
            Err(e) => {
                tracing::error!("Stream Read Error: {}", e);
                break;
            }
        }
    }
    
    let total_time = start_time.elapsed();
    let mb = total_written as f64 / 1_000_000.0;
    let speed = mb / total_time.as_secs_f64();
    tracing::info!("File Stream Completed. Written {} chunks ({} bytes) in {:?}. Speed: {:.2} MB/s", chunk_count, total_written, total_time, speed);
    
    // Final Progress
    let _ = app.emit("file-progress", serde_json::json!({
         "id": header.id,
         "fileName": header.file_name,
         "total": header.file_size,
         "transferred": total_written
     }));
    
     // Emit received event
     let _ = app.emit("file-received", &header);
     
     // Notification
     let settings = state.settings.lock().unwrap();
     if settings.notify_large_files && header.file_size > settings.max_auto_download_size {
         let body = format!("Download complete: {}", header.file_name);
         send_notification(&app, "Download Complete", &body, false, None);
     }

    // 5. Verify Size
    if total_written == header.file_size {
        tracing::info!("File Transfer Verified OK");
        if let Some(path_str) = file_path.to_str() {
             crate::clipboard::set_clipboard_paths(&app, vec![path_str.to_string()]);
        }
    } else {
        tracing::warn!("File Transfer Incomplete! Expected {}, got {}", header.file_size, total_written);
    }
}

async fn handle_message(msg: Message, addr: std::net::SocketAddr, listener_state: AppState, listener_handle: tauri::AppHandle, transport_inside: Transport) {
    match msg {
        Message::Clipboard(ciphertext) => {
            // Decrypt
            tracing::debug!("Received Encrypted Clipboard from {}", addr);
            let key_opt = {
                listener_state.cluster_key.lock().unwrap().clone()
            };

            if let Some(key) = key_opt {
                let mut key_arr = [0u8; 32];
                if key.len() == 32 {
                    key_arr.copy_from_slice(&key);
                    match crypto::decrypt(&key_arr, &ciphertext).map_err(|e| e.to_string()) {
                        Ok(plaintext) => {
                            // Try to parse as ClipboardPayload
                            let (text, id, ts, sender, payload) = if let Ok(payload) = serde_json::from_slice::<crate::protocol::ClipboardPayload>(&plaintext) {
                                    (payload.text.clone(), payload.id.clone(), payload.timestamp, payload.sender.clone(), payload)
                            } else if let Ok(text) = String::from_utf8(plaintext.clone()) {
                                    // Backward compatibility / Fallback
                                    (
                                        text.clone(),
                                        uuid::Uuid::new_v4().to_string(),
                                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                                        "Unknown (Legacy)".to_string(),
                                        crate::protocol::ClipboardPayload {
                                            text: text.clone(),
                                            id: uuid::Uuid::new_v4().to_string(),
                                            timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                                            sender: "Unknown (Legacy)".to_string(),
                                            sender_id: "unknown".to_string(),
                                            files: None,
                                        }
                                    )
                            } else {
                                    tracing::error!("Failed to parse decrypted clipboard payload.");
                                    return;
                            };

                            // Self-sender check
                            {
                                let my_hostname = get_hostname_internal();
                                if sender == my_hostname {
                                    tracing::debug!("Ignoring clipboard message from self (sender={})", sender);
                                    return;
                                }
                            }

                            // Loop/Dedupe Check
                            let content_signature = if let Some(files) = &payload.files {
                                if !files.is_empty() {
                                    let mut sig = String::from("FILES:");
                                    for f in files {
                                        use std::fmt::Write;
                                        let _ = write!(sig, "{}:{};", f.name, f.size);
                                    }
                                    sig
                                } else {
                                    text.clone()
                                }
                            } else {
                                text.clone()
                            };

                            {
                                let mut last = listener_state.last_clipboard_content.lock().unwrap();
                                if *last == content_signature {
                                    tracing::debug!("Ignoring clipboard message - content matches last_clipboard_content");
                                    return;
                                }
                                *last = content_signature;
                            }

                            // Check Auto-Receive Setting
                            tracing::debug!("Decrypted Clipboard from {}: {}...", sender, if text.len() > 20 { &text[0..20] } else { &text }); 

                            if let Some(files) = &payload.files {
                                if !files.is_empty() {
                                    #[cfg(desktop)]
                                    {
                                        let should_badge = if let Some(window) = listener_handle.get_webview_window("main") {
                                            match window.is_focused() {
                                                Ok(focused) => !focused,
                                                Err(_) => true,
                                            }
                                        } else {
                                            true
                                        };
                                        
                                        if should_badge {
                                            crate::tray::set_badge(&listener_handle, true);
                                        }
                                    }
                                }
                            }
                            
                            // Create Payload Object (already created above as 'payload' or fallback)
                            // Use the one we constructed or parsed
                            let payload_obj = crate::protocol::ClipboardPayload {
                                id: id.clone(),
                                text: text.clone(),
                                files: payload.files.clone(),
                                timestamp: ts,
                                sender: sender.clone(),
                                sender_id: payload.sender_id.clone(),
                            };

                            // FILE HANDLING
                            if let Some(files) = &payload.files {
                                if !files.is_empty() {
                                    tracing::info!("Received File Metadata from {}: {} files", sender, files.len());
                                    let _ = listener_handle.emit("clipboard-change", &payload_obj);
                                    
                                    // Auto-Download Logic
                                    let (auto_recv, enable_ft, size_limit) = {
                                        let s = listener_state.settings.lock().unwrap();
                                        (s.auto_receive, s.enable_file_transfer, s.max_auto_download_size)
                                    };

                                    if !enable_ft {
                                        tracing::info!("File transfer disabled in settings. Ignoring auto-download.");
                                    } else {
                                        let mut total_size = 0u64;
                                        for f in files { total_size += f.size; }
                                        
                                        if auto_recv && total_size <= size_limit {
                                            tracing::info!("Auto-downloading {} files ({} bytes)", files.len(), total_size);
                                            // Request Each File
                                            for (idx, _file_meta) in files.iter().enumerate() {
                                                tracing::info!("Requesting file {}/{}", idx, files.len());
                                                let req_payload = crate::protocol::FileRequestPayload {
                                                    id: id.clone(),
                                                    file_index: idx,
                                                    offset: 0,
                                                };
                                                // Encrypt Request
                                                if let Ok(req_json) = serde_json::to_vec(&req_payload) {
                                                    if let Ok(req_cipher) = crypto::encrypt(&key_arr, &req_json) {
                                                        let msg = Message::FileRequest(req_cipher);
                                                        if let Ok(data) = serde_json::to_vec(&msg) {
                                                            let transport_clone = transport_inside.clone();
                                                            let addr_clone = addr;
                                                            tauri::async_runtime::spawn(async move {
                                                                let _ = transport_clone.send_message(addr_clone, &data).await;
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                        } else {
                                            let settings = listener_state.settings.lock().unwrap();
                                            if settings.notify_large_files {
                                                let body = format!("Received {} files from {}. Click to download.", files.len(), sender);
                                                send_notification(&listener_handle, "Files Available", &body, true, None);
                                            }
                                        }
                                    } // End if !enable_ft else
                                } // End if !files.is_empty()
                            } // End if let Some(files)

                            // TEXT HANDLING
                            if !text.is_empty() {
                                let auto_receiver = { listener_state.settings.lock().unwrap().auto_receive };
                                if auto_receiver {
                                    clipboard::set_clipboard(&listener_handle, text.clone());
                                    let _ = listener_handle.emit("clipboard-change", &payload_obj);
                                } else {
                                    // Manual Mode
                                    tracing::info!("[Clipboard] Auto-receive OFF. Storing pending clipboard from {}", sender);
                                    {
                                        let mut pending = listener_state.pending_clipboard.lock().unwrap();
                                        *pending = Some(payload_obj.clone());
                                    }
                                    let _ = listener_handle.emit("clipboard-pending", &payload_obj);
                                }
                                
                                let notifications = listener_state.settings.lock().unwrap().notifications.clone();
                                if notifications.data_received {
                                    send_notification(&listener_handle, "Clipboard Received", "Content copied to clipboard", false, Some(2));
                                }
                            }

                            // Relay Logic
                            let auto_send = { listener_state.settings.lock().unwrap().auto_send };
                            if !auto_send {
                                    return; 
                            }
                            
                            let state_relay = listener_state.clone();
                            let transport_relay = transport_inside.clone(); 
                            let sender_addr = addr;
                            let relay_key_arr = key_arr; 
                            
                            let payload_bytes = serde_json::to_vec(&payload_obj).unwrap_or(plaintext);
                            
                            if let Ok(relay_ciphertext) = crypto::encrypt(&relay_key_arr, &payload_bytes).map_err(|e| e.to_string()) {
                                let relay_data = serde_json::to_vec(&Message::Clipboard(relay_ciphertext)).unwrap_or_default();
                                let peers = state_relay.get_peers();
                                for p in peers.values() {
                                    let p_addr = std::net::SocketAddr::new(p.ip, p.port);
                                    if p_addr == sender_addr { continue; }
                                    let _ = transport_relay.send_message(p_addr, &relay_data).await;
                                }
                            }
                        }
                        Err(e) => tracing::error!("Decryption failed: {}", e),
                    }
                } else {
                    tracing::warn!("Received clipboard but no Cluster Key set!"); 
                }
            }
        }
        Message::HistoryDelete(id) => {
            tracing::info!("Received HistoryDelete for ID: {}", id);
            let _ = listener_handle.emit("history-delete", &id);
        }
        Message::PairRequest { msg, device_id } => {
            tracing::info!("Received PairRequest from {} ({}). Authenticating...", addr, device_id);
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
                                    tracing::info!("Authentication Success for {}!", device_id);
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
                                Err(e) => tracing::error!("Auth Failed: {}", e),
                            }
                        }
                    }
                }
                Err(e) => tracing::error!("SPAKE2 Error: {}", e),
            }
        }
        Message::PairResponse { msg, device_id } => {
            tracing::info!("Received PairResponse from {} ({})", addr, device_id);
            let spake_state = {
                let mut pending = listener_state.pending_handshakes.lock().unwrap();
                pending.remove(&addr.to_string())
            };
            if let Some(state) = spake_state {
                match crypto::finish_spake2(state, &msg).map_err(|e| e.to_string()) {
                    Ok(session_key) => {
                        tracing::info!("Auth Success (Initiator)! Waiting for Welcome...");
                        let mut sessions = listener_state.handshake_sessions.lock().unwrap();
                        sessions.insert(addr.to_string(), session_key);
                    }
                    Err(e) => {
                        tracing::error!("Auth Failed: {}", e);
                        let _ = listener_handle.emit("pairing-failed", "Authentication failed. Check the PIN and try again.");
                    }
                }
            } else {
                tracing::warn!("Received PairResponse but no pending handshake found for {}", addr);
                let _ = listener_handle.emit("pairing-failed", "Pairing session expired. Please try again.");
            }
        }
        Message::Welcome { encrypted_cluster_key, known_peers, network_name, network_pin } => {
             tracing::info!("Received WELCOME from {}", addr);
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
                             tracing::info!("Joined Network: {} (PIN: {})", network_name, network_pin);
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
                             let device_id = listener_state.local_device_id.lock().unwrap().clone();
                             let port = transport_inside.local_addr().map(|a| a.port()).unwrap_or(0);
                             if let Some(discovery) = listener_state.discovery.lock().unwrap().as_mut() {
                                  let _ = discovery.register(&device_id, &network_name, port);
                             }
                             let mut kp_lock = listener_state.known_peers.lock().unwrap();
                             let mut runtime_peers = listener_state.peers.lock().unwrap();
                             for peer in known_peers {
                                 kp_lock.insert(peer.id.clone(), peer.clone());
                                 runtime_peers.insert(peer.id.clone(), peer.clone());
                                 let _ = listener_handle.emit("peer-update", &peer);
                             }
                             save_known_peers(listener_handle.app_handle(), &kp_lock);
                             
                             for (id, peer) in runtime_peers.iter_mut() {
                                 if peer.ip == addr.ip() {
                                     peer.is_trusted = true;
                                     peer.network_name = Some(network_name.clone());
                                     let _ = listener_handle.emit("peer-update", &*peer);
                                     kp_lock.insert(id.clone(), peer.clone());
                                     break;
                                 }
                             }
                             save_known_peers(listener_handle.app_handle(), &kp_lock);
                         }
                         Err(e) => {
                             tracing::error!("Decryption Error: {}", e);
                             let _ = listener_handle.emit("pairing-failed", "Failed to join network. The PIN may be incorrect.");
                         }
                     }
                 } else {
                     tracing::error!("Decryption Error: No Cluster Key loaded.");
                     let _ = listener_handle.emit("pairing-failed", "Session key error. Please try again.");
                 }
             } else {
                 tracing::warn!("Received Welcome but no session key found for {}", addr);
                 let _ = listener_handle.emit("pairing-failed", "Pairing session expired. Please try again.");
             }
        }
        Message::PeerDiscovery(mut peer) => {
            tracing::debug!("Received PeerDiscovery for {}", peer.hostname);
            
            let local_id = listener_state.local_device_id.lock().unwrap().clone();
            if peer.id == local_id {
                return;
            }

            {
                let mut pending = listener_state.pending_removals.lock().unwrap();
                if pending.remove(&peer.id).is_some() {
                    tracing::info!("[Discovery] Cancelled pending removal for {} due to Heartbeat/Packet.", peer.id);
                }
            }

            peer.ip = addr.ip();
            peer.port = addr.port();
            peer.last_seen = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            
            {
                let kp = listener_state.known_peers.lock().unwrap();
                if let Some(existing) = kp.get(&peer.id) {
                     peer.is_manual = existing.is_manual;
                } else {
                     peer.is_manual = false; 
                }
            }
            
            let mut should_reply = false;
            {
                 let mut kp_lock = listener_state.known_peers.lock().unwrap();
                 let manual_id = format!("manual-{}", peer.ip);
                 if kp_lock.contains_key(&manual_id) {
                     tracing::info!("Replacing manual placeholder {} with real peer {}", manual_id, peer.id);
                     kp_lock.remove(&manual_id);
                     listener_state.peers.lock().unwrap().remove(&manual_id);
                     let _ = listener_handle.emit("peer-remove", &manual_id);
                     should_reply = true; 
                     peer.is_manual = true;
                 }
                 
                 let runtime_known = listener_state.peers.lock().unwrap().contains_key(&peer.id);
                 if !kp_lock.contains_key(&peer.id) && !runtime_known {
                     should_reply = true;
                 }

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
                     tracing::debug!("Verified Signature for {}! Trust maintained/granted.", peer.id);
                     peer.is_trusted = true;
                 } else {
                     if let Some(existing) = kp_lock.get(&peer.id) {
                         if existing.is_trusted {
                            tracing::warn!("Revoking Trust for {}: Invalid/Missing Signature.", peer.id);
                         }
                     }
                     peer.is_trusted = false;
                 }

                 listener_state.add_peer(peer.clone());
                 let _ = listener_handle.emit("peer-update", &peer);

                 if peer.is_trusted || peer.is_manual {
                     kp_lock.insert(peer.id.clone(), peer.clone());
                     save_known_peers(listener_handle.app_handle(), &kp_lock);
                 } else {
                     if kp_lock.contains_key(&peer.id) {
                         tracing::info!("Removing untrusted auto-peer {} from persistence.", peer.id);
                         kp_lock.remove(&peer.id);
                         save_known_peers(listener_handle.app_handle(), &kp_lock);
                     }
                 }
            }
            
            if should_reply {
                tracing::debug!("Sending Discovery Reply to {}", addr);
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
                
                let my_peer = crate::peer::Peer {
                    id: local_id,
                    ip: transport_inside.local_addr().unwrap().ip(),
                    port: transport_inside.local_addr().unwrap().port(),
                    hostname,
                    last_seen: 0,
                    is_trusted: false, 
                    is_manual: true,
                    network_name: Some(network_name),
                    signature,
                };
                
                let msg = Message::PeerDiscovery(my_peer);
                let data = serde_json::to_vec(&msg).unwrap_or_default();
                tauri::async_runtime::spawn(async move {
                    let _ = transport_inside.send_message(addr, &data).await;
                });
            }
        }
        Message::PeerRemoval(target_id) => {
            tracing::info!("Received PeerRemoval for {}", target_id);
            let local_id = listener_state.local_device_id.lock().unwrap().clone();
            
            if target_id == local_id {
                tracing::warn!("I have been removed from the network! resetting state...");
                perform_factory_reset(
                    &listener_handle,
                    &listener_state,
                    transport_inside.local_addr().map(|a| a.port()).unwrap_or(0)
                );
            } else {
                {
                    let mut kp = listener_state.known_peers.lock().unwrap();
                    if kp.remove(&target_id).is_some() {
                        save_known_peers(listener_handle.app_handle(), &kp);
                    }
                }
                {
                    let mut peers = listener_state.peers.lock().unwrap();
                    if let Some(peer) = peers.remove(&target_id) {
                        drop(peers);
                        check_and_notify_leave(&listener_handle, &listener_state, &peer);
                    }
                }
                let _ = listener_handle.emit("peer-remove", &target_id);
            }
        }
        
        Message::FileRequest(req_cipher) => {
             // HANDLE FILE REQUEST (Sender)
             // 1. Decrypt Request
             tracing::info!("Received File Request from {}", addr);
             let key_opt = { listener_state.cluster_key.lock().unwrap().clone() };
             if let Some(key) = key_opt {
                 let mut key_arr = [0u8; 32];
                 if key.len() == 32 {
                     key_arr.copy_from_slice(&key);
                     match crypto::decrypt(&key_arr, &req_cipher).map_err(|e| e.to_string()) {
                         Ok(plaintext) => {
                             if let Ok(req) = serde_json::from_slice::<crate::protocol::FileRequestPayload>(&plaintext) {
                                 tracing::info!("Processing File Request: ID={}, Index={}", req.id, req.file_index);
                                 
                                 // 2. Find File Path
                                 let path = {
                                     let map = listener_state.local_files.lock().unwrap();
                                     if let Some(paths) = map.get(&req.id) {
                                         if req.file_index < paths.len() {
                                             Some(paths[req.file_index].clone())
                                         } else { None }
                                     } else { None }
                                 };
                                 
                                 if let Some(p_str) = path {
                                      let file_path = PathBuf::from(p_str.clone());
                                      // 3. Open Stream & Send
                                      tauri::async_runtime::spawn(async move {
                                           // Open File
                                           let mut file = match File::open(&file_path).await {
                                               Ok(f) => f,
                                               Err(e) => { tracing::error!("Failed to open requested file: {}", e); return; }
                                           };
                                           let file_size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
                                           let file_name = file_path.file_name().unwrap_or_default().to_string_lossy().to_string();
                                           
                                           tracing::info!("Opening QUIC Stream to {} for file '{}' ({} bytes)", addr, file_name, file_size);
                                           // Open QUIC Stream
                                           match transport_inside.send_file_stream(addr).await {
                                               Ok((_connection, mut stream)) => {
                                                   // 4a. Generate Auth Token
                                                   let timestamp = std::time::SystemTime::now()
                                                       .duration_since(std::time::UNIX_EPOCH)
                                                       .unwrap_or_default()
                                                       .as_secs();
                                                   
                                                   let auth_payload = timestamp.to_le_bytes();
                                                   let auth_token = match crypto::encrypt(&key_arr, &auth_payload) {
                                                       Ok(c) => BASE64.encode(c),
                                                       Err(e) => {
                                                           tracing::error!("Failed to generate auth token: {}", e);
                                                           return;
                                                       }
                                                   };
                                                   
                                                   // 4b. Send Header
                                                   let header = crate::protocol::FileStreamHeader {
                                                       id: req.id,
                                                       file_index: req.file_index,
                                                       file_name,
                                                       file_size,
                                                       auth_token,
                                                   };
                                                   
                                                   if let Ok(h_json) = serde_json::to_string(&header) {
                                                       if let Err(e) = stream.write_all(h_json.as_bytes()).await { tracing::error!("Header Write Error: {}", e); return; }
                                                       if let Err(e) = stream.write_all(b"\n").await { tracing::error!("Header Newline Error: {}", e); return; }
                                                   }
                                                   
                                                   // 5. Send Raw File
                                                   let mut buf = vec![0u8; 1024 * 1024]; // 1MB chunks
                                                   let mut chunks_sent = 0;
                                                   let start_time = std::time::Instant::now();

                                                   tracing::info!("[Sender] Starting RAW loop. File size: {}", file_size);

                                                   loop {
                                                       match file.read(&mut buf).await {
                                                           Ok(0) => break, // EOF
                                                           Ok(n) => {
                                                               // Write Raw Data
                                                               if let Err(e) = stream.write_all(&buf[0..n]).await { tracing::error!("Stream Write Error: {}", e); break; }
                                                               chunks_sent += 1;
                                                           }
                                                           Err(e) => { tracing::error!("File Read Error: {}", e); break; }
                                                       }
                                                   }
                                                   let total_time = start_time.elapsed();
                                                   tracing::info!("[Sender] Loop finished in {:?}. Chunks: {}", total_time, chunks_sent);
                                                   // Finish Stream
                                                   let _ = stream.finish();
                                                   
                                                   // Ensure connection stays alive until data is flushed/acknowledged
                                                   tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                                                   _connection.close(0u32.into(), b"done");
                                                   
                                                   tracing::info!("File Sent Successfully: {}", p_str);
                                               }

                                               Err(e) => tracing::error!("Failed to open file stream: {}", e),
                                           }
                                      });
                                 } else {
                                     tracing::warn!("Requested file not found (ID: {}, Index: {})", req.id, req.file_index);
                                 }
                             }
                         }
                         Err(e) => tracing::error!("Failed to decrypt FileRequest: {}", e),
                     }
                 }
             }
        }
    }
}



#[tauri::command]
async fn request_file(
    _app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    file_id: String,
    file_index: usize,
    peer_id: String,
) -> Result<(), String> {
    tracing::info!("Manual File Request: ID={}, Index={}, Peer={}", file_id, file_index, peer_id);
    
    // 1. Find Peer Address
    let addr = {
        let peers = state.get_peers();
        if let Some(p) = peers.get(&peer_id) {
            std::net::SocketAddr::new(p.ip, p.port)
        } else {
             return Err(format!("Peer {} not found or offline", peer_id));
        }
    };
    
    // 2. Get Transport
    let transport = {
        let t_lock = state.transport.lock().unwrap();
        t_lock.clone().ok_or("Transport not initialized".to_string())?
    };
    
    // 3. Encrypt & Send Request
    let req_payload = crate::protocol::FileRequestPayload {
        id: file_id,
        file_index,
        offset: 0,
    };
    
    let key_opt = state.cluster_key.lock().unwrap().clone();
    if let Some(key) = key_opt {
        if key.len() == 32 {
            let mut key_arr = [0u8; 32];
            key_arr.copy_from_slice(&key);
             if let Ok(req_json) = serde_json::to_vec(&req_payload) {
                if let Ok(req_cipher) = crypto::encrypt(&key_arr, &req_json).map_err(|e| e.to_string()) {
                    let msg = Message::FileRequest(req_cipher);
                    if let Ok(data) = serde_json::to_vec(&msg) {
                        transport.send_message(addr, &data).await.map_err(|e| e.to_string())?;
                        tracing::info!("File Request sent to {}", addr);
                        return Ok(());
                    }
                }
             }
        }
    }
    
    Err("Failed to encrypt/send request".to_string())
}

fn register_shortcuts(app_handle: &tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    
    // Unregister all first to clear old ones
    if let Err(e) = app_handle.global_shortcut().unregister_all() {
        tracing::warn!("Failed to unregister shortcuts: {}", e);
    }
    
    // Register Send Shortcut
    if !settings.auto_send {
        if let Some(s) = &settings.shortcut_send {
            match Shortcut::from_str(s) {
                Ok(shortcut) => {
                    if let Err(e) = app_handle.global_shortcut().register(shortcut) {
                        tracing::error!("Failed to register Send shortcut '{}': {}", s, e);
                    } else {
                        tracing::debug!("Registered Send shortcut: {}", s);
                    }
                }
                Err(e) => tracing::error!("Invalid Send shortcut '{}': {}", s, e),
            }
        }
    }
    
    // Register Receive Shortcut
    if !settings.auto_receive {
        if let Some(s) = &settings.shortcut_receive {
            match Shortcut::from_str(s) {
                Ok(shortcut) => {
                    if let Err(e) = app_handle.global_shortcut().register(shortcut) {
                        tracing::error!("Failed to register Receive shortcut '{}': {}", s, e);
                    } else {
                        tracing::debug!("Registered Receive shortcut: {}", s);
                    }
                }
                Err(e) => tracing::error!("Invalid Receive shortcut '{}': {}", s, e),
            }
        }
    }
}

fn handle_shortcut(app_handle: &tauri::AppHandle, shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state == ShortcutState::Released {
        return;
    }
    let state = app_handle.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    
    // Check Send
    if let Some(s) = &settings.shortcut_send {
        if let Ok(parsed) = Shortcut::from_str(s) {
           if parsed == *shortcut {
               tracing::info!("Global Send Shortcut Triggered!");
               // Trigger Send Logic
               // Get local content
               match app_handle.state::<Clipboard>().read_text() {
                   Ok(text) => {
                        let hostname = hostname::get().map(|h| h.to_string_lossy().to_string()).unwrap_or("Unknown".to_string());
                        let msg_id = uuid::Uuid::new_v4().to_string();
                        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();

                            let local_id = state.local_device_id.lock().unwrap().clone();
                            let payload_obj = crate::protocol::ClipboardPayload {
                                id: msg_id.clone(),
                                text: text.clone(),
                                timestamp: ts,
                                sender: hostname,
                                sender_id: local_id,
                                files: None,
                            };
                        
                        // Emit local event
                        let _ = app_handle.emit("clipboard-change", &payload_obj);

                        // Encrypt & Send
                        let ck_lock = state.cluster_key.lock().unwrap();
                        if let Some(key) = ck_lock.as_ref() {
                            if key.len() == 32 {
                                let mut key_arr = [0u8; 32];
                                key_arr.copy_from_slice(key);
                                if let Ok(json_payload) = serde_json::to_vec(&payload_obj) {
                                    if let Ok(cipher) = crypto::encrypt(&key_arr, &json_payload) {
                                        let msg = Message::Clipboard(cipher);
                                        if let Ok(data) = serde_json::to_vec(&msg) {
                                            let transport = app_handle.state::<Transport>();
                                            let peers = state.get_peers();
                                            for p in peers.values() {
                                                let addr = std::net::SocketAddr::new(p.ip, p.port);
                                                let transport_clone = (*transport).clone();
                                                let data_vec = data.clone();
                                                tauri::async_runtime::spawn(async move {
                                                    let _ = transport_clone.send_message(addr, &data_vec).await;
                                                });
                                            }
                                            
                                            // Notification
                                            let notif_settings = settings.notifications.clone();
                                            if notif_settings.data_sent {
                                                send_notification(app_handle, "Clipboard Sent", "Manual broadcast successful.", false, Some(2));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                   },
                   Err(e) => tracing::error!("Failed to read clipboard for global send: {}", e),
               }
               return;
           }
        }
    }
    
    // Check Receive
    if let Some(s) = &settings.shortcut_receive {
        if let Ok(parsed) = Shortcut::from_str(s) {
           if parsed == *shortcut {
                tracing::info!("Global Receive Shortcut Triggered!");
                // Manual Receive Logic
                let mut guard = state.pending_clipboard.lock().unwrap();
                if let Some(payload) = guard.take() { // take() removes it from pending
                    // Apply to System Clipboard
                    // Using clipboard plugin
                    if let Err(e) = app_handle.state::<Clipboard>().write_text(payload.text) {
                        tracing::error!("Failed to write pending clipboard to system: {}", e);
                    } else {
                        tracing::info!("Confirmed pending clipboard content via shortcut.");
                        send_notification(app_handle, "Clipboard Received", "Pending content applied.", false, Some(2));
                    }
                } else {
                    tracing::info!("No pending clipboard content to receive.");
                     send_notification(app_handle, "Manual Receive", "No pending content.", false, Some(3));
                }
           }
        }
    }
}
#[derive(serde::Serialize)]
struct ExtensionStatus {
    is_gnome: bool,
    is_installed: bool,
}

#[tauri::command]
async fn check_gnome_extension_status() -> ExtensionStatus {
    let xdg_current_desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let is_gnome = xdg_current_desktop.contains("GNOME");

    if !is_gnome {
        return ExtensionStatus { is_gnome: false, is_installed: false };
    }

    // Try D-Bus first (works in Flatpak if permissions are set)
    if let Ok(connection) = zbus::Connection::session().await {
         let proxy_result: zbus::Result<zbus::Proxy> = zbus::Proxy::new(
             &connection,
             "org.gnome.Shell",
             "/org/gnome/Shell",
             "org.gnome.Shell.Extensions"
         ).await;

         if let Ok(proxy) = proxy_result {
              // Method: ListExtensions() -> a{sa{sv}}
              // Returns a map where key is UUID, value is properties
              // Use OwnedValue to avoid lifetime issues with DynamicDeserialize
              let call_result: zbus::Result<std::collections::HashMap<String, std::collections::HashMap<String, zbus::zvariant::OwnedValue>>> = proxy.call("ListExtensions", &()).await;
              
              if let Ok(extensions) = call_result {
                   let is_installed = extensions.contains_key("clustercut@keithvassallo.com");
                   return ExtensionStatus { is_gnome: true, is_installed };
              }
         }
    }

    // Fallback to File Check (for native builds)
    let home = std::env::var("HOME").unwrap_or_default();
    let local_path = format!("{}/.local/share/gnome-shell/extensions/clustercut@keithvassallo.com", home);
    let system_path = "/usr/share/gnome-shell/extensions/clustercut@keithvassallo.com";

    let is_installed = std::path::Path::new(&local_path).exists() || std::path::Path::new(system_path).exists();

    ExtensionStatus { is_gnome: true, is_installed }
}
