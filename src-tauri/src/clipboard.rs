use crate::crypto;
use crate::protocol::Message;
use crate::state::AppState;
use crate::transport::Transport;
use arboard::Clipboard;
use std::{thread, time::Duration};
use tauri::{AppHandle, Emitter};

// Use a shared cache to avoid feedback loops
use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex};

static IGNORED_TEXT: Lazy<Arc<Mutex<Option<String>>>> = Lazy::new(|| Arc::new(Mutex::new(None)));

pub fn start_monitor(app_handle: AppHandle, state: AppState, transport: Transport) {
    thread::spawn(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to initialize clipboard: {}", e);
                return;
            }
        };

        let mut last_text = clipboard.get_text().unwrap_or_default();

        // Polling loop
        loop {
            if let Ok(text) = clipboard.get_text() {
                // Check if this text should be ignored (because we just set it)
                let should_ignore = {
                    let mut ignored = IGNORED_TEXT.lock().unwrap();
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
                    // (actually, better to keep it until it changes? No, once seen is enough usually)
                    {
                        let mut ignored = IGNORED_TEXT.lock().unwrap();
                        *ignored = None;
                    }
                } else if text != last_text && !text.is_empty() {
                    println!("Clipboard Changed detected (len={})", text.len());
                    last_text = text.clone();

                    // Emit to frontend (local notification)
                    let _ = app_handle.emit("clipboard-change", &text);

                    // Encrypt Payload using Cluster Key
                    let payload: Option<Vec<u8>> = {
                        let ck_lock = state.cluster_key.lock().unwrap();
                        if let Some(key) = ck_lock.as_ref() {
                            let mut key_arr = [0u8; 32];
                            if key.len() == 32 {
                                key_arr.copy_from_slice(key);
                                // Encrypt
                                match crypto::encrypt(&key_arr, text.as_bytes()) {
                                    Ok(cipher) => Some(cipher),
                                    Err(e) => {
                                        eprintln!("Failed to encrypt clipboard: {}", e);
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
                                if let Err(e) = transport_clone.send_message(addr, &data_vec).await
                                {
                                    eprintln!("Failed to send to {}: {}", addr, e);
                                } else {
                                    println!("Sent clipboard to {}", addr);
                                }
                            });
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

pub fn set_clipboard(text: String) {
    let text_clone = text.clone();
    // Spawn a thread to set clipboard to avoid blocking network loop
    thread::spawn(move || {
        match Clipboard::new() {
            Ok(mut c) => {
                // Mark this content as "to be ignored" by the monitor
                {
                    let mut ignored = IGNORED_TEXT.lock().unwrap();
                    *ignored = Some(text_clone);
                }

                if let Err(e) = c.set_text(text) {
                    eprintln!("Failed to set clipboard: {}", e);
                } else {
                    println!("Successfully set local clipboard content.");
                }
            }
            Err(e) => eprintln!("Failed to init clipboard for write: {}", e),
        }
    });
}
