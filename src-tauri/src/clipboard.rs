use crate::crypto;
use crate::protocol::Message;
use crate::state::AppState;
use crate::transport::Transport;
// use arboard::Clipboard;
use std::{thread, time::Duration};
use tauri::{AppHandle, Emitter};
use tauri_plugin_clipboard_manager::ClipboardExt;

// Use a shared cache to avoid feedback loops
use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex};

static IGNORED_TEXT: Lazy<Arc<Mutex<Option<String>>>> = Lazy::new(|| Arc::new(Mutex::new(None)));

pub fn start_monitor(app_handle: AppHandle, state: AppState, transport: Transport) {
    thread::spawn(move || {
        let mut last_text = match app_handle.clipboard().read_text() {
            Ok(t) => t,
            _ => String::new(),
        };

        // Polling loop
        loop {
            // Use Tauri Plugin for reading (Main Thread Safe via Dispatch)
            if let Ok(text) = app_handle.clipboard().read_text() {
                // Check if this text should be ignored (because we just set it)
                let should_ignore = {
                    let ignored = IGNORED_TEXT.lock().unwrap();
                    if let Some(ref ign) = *ignored {
                        if ign == &text {
                            // Match found, clear ignored and skip processing
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };

                if should_ignore {
                    // Start tracking this as the new "baseline" so we don't trigger on it later
                    // But don't broadcast it.
                    last_text = text;
                    // Clear the ignored text now that we've seen it
                    {
                        let mut ignored = IGNORED_TEXT.lock().unwrap();
                        *ignored = None;
                    }
                } else if text != last_text && !text.is_empty() {
                    tracing::debug!("Clipboard Changed detected (len={})", text.len());
                    last_text = text.clone();

                    // Update global deduplication cache to prevent echo loops
                    {
                        let mut last_global = state.last_clipboard_content.lock().unwrap();
                        *last_global = text.clone();
                    }

                    // Construct Payload Object
                    // let local_id = { state.local_device_id.lock().unwrap().clone() };
                    let hostname = crate::get_hostname_internal();
                    let msg_id = uuid::Uuid::new_v4().to_string();
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    let payload_obj = crate::protocol::ClipboardPayload {
                        id: msg_id.clone(),
                        text: text.clone(),
                        timestamp: ts,
                        sender: hostname,
                    };

                    // Emit to frontend (local notification)
                    let _ = app_handle.emit("clipboard-change", &payload_obj);

                    // Check Auto-Send Setting
                    let auto_send = { state.settings.lock().unwrap().auto_send };
                    if !auto_send {
                        tracing::debug!("Auto-send disabled. Skipping broadcast.");
                        // We continue the loop, sleeping at the end
                    } else {
                        // Encrypt Payload using Cluster Key
                        let payload: Option<Vec<u8>> = {
                            let ck_lock = state.cluster_key.lock().unwrap();
                            if let Some(key) = ck_lock.as_ref() {
                                let mut key_arr = [0u8; 32];
                                if key.len() == 32 {
                                    key_arr.copy_from_slice(key);
                                    // Serialize Payload
                                    let json_payload =
                                        serde_json::to_vec(&payload_obj).unwrap_or_default();
                                    // Encrypt
                                    match crypto::encrypt(&key_arr, &json_payload) {
                                        Ok(cipher) => Some(cipher),
                                        Err(e) => {
                                            tracing::error!("Failed to encrypt clipboard: {}", e);
                                            None
                                        }
                                    }
                                } else {
                                    None
                                }
                            } else {
                                // No key set, cannot broadcast securely
                                // println!("Skipping broadcast: No Cluster Key");
                                None
                            }
                        };

                        if let Some(encrypted_data) = payload {
                            // Broadcast to peers
                            let peers = state.get_peers();

                            if !peers.is_empty() {
                                let notifications =
                                    state.settings.lock().unwrap().notifications.clone();
                                if notifications.data_sent {
                                    crate::send_notification(
                                        &app_handle,
                                        "Clipboard Sent",
                                        "Clipboard content broadcasted to cluster.",
                                    );
                                }
                            }
                            // Wrap in protocol message
                            let msg = Message::Clipboard(encrypted_data);
                            let data = serde_json::to_vec(&msg).unwrap_or_default();

                            // Only send to Trusted Peers? Or Known Peers?
                            // If we used the cluster key, only those who have it can read it.
                            // So we can send to all known peers.
                            for peer in peers.values() {
                                // Only send to trusted peers? Or all known?
                                // If they are in `peers` map (from Discovery), we can try.
                                // Ideally we only send to `is_trusted` if we want to save bandwidth,
                                // but for now let's send to all found peers.
                                let target = peer.ip;
                                let port = peer.port;
                                let transport_clone = transport.clone();
                                let data_vec = data.to_vec();

                                // Spawn send so we don't block polling
                                tauri::async_runtime::spawn(async move {
                                    let addr = std::net::SocketAddr::new(target, port);
                                    if let Err(e) =
                                        transport_clone.send_message(addr, &data_vec).await
                                    {
                                        tracing::error!("Failed to send to {}: {}", addr, e);
                                    } else {
                                        tracing::info!("Sent clipboard to {}", addr);
                                    }
                                });
                            }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

pub fn set_clipboard(app: &AppHandle, text: String) {
    // First, check if the clipboard already contains this exact text.
    // This prevents a race condition on macOS where:
    // 1. User copies text -> ClusterCut broadcasts it
    // 2. Remote peer receives and relays back
    // 3. We receive our own content and call set_clipboard
    // 4. arboard's clearContents() + writeObjects() is NOT atomic
    // 5. Between clear and write, the pasteboard is empty
    // 6. User tries to paste -> nothing there!
    //
    // By checking BEFORE writing, we avoid the unnecessary clear+write cycle.
    use tauri_plugin_clipboard_manager::ClipboardExt;

    match app.clipboard().read_text() {
        Ok(current) if current == text => {
            tracing::debug!("Clipboard already contains this text, skipping write to avoid race condition");
            return;
        }
        _ => {}
    }

    let text_clone = text.clone();

    // Mark this content as "to be ignored" by the monitor
    {
        let mut ignored = IGNORED_TEXT.lock().unwrap();
        *ignored = Some(text_clone);
    }

    // Use Tauri Clipboard Plugin (Main Thread Safe)
    if let Err(e) = app.clipboard().write_text(text.clone()) {
        tracing::error!("Failed to set clipboard via Tauri Plugin: {}", e);
    } else {
        tracing::debug!("Successfully set local clipboard content via Tauri Plugin.");
    }
}
