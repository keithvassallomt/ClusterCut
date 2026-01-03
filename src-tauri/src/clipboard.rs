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

                    // Emit to frontend
                    let _ = app_handle.emit("clipboard-change", &text);

                    // Broadcast to peers
                    let peers = state.get_peers();
                    let data = text.as_bytes();

                    for peer in peers.values() {
                        let target = peer.ip;
                        let port = peer.port;
                        let transport_clone = transport.clone();
                        let data_vec = data.to_vec();

                        // Spawn send so we don't block polling
                        tauri::async_runtime::spawn(async move {
                            let addr = std::net::SocketAddr::new(target, port);
                            if let Err(e) = transport_clone.send_message(addr, &data_vec).await {
                                eprintln!("Failed to send to {}: {}", addr, e);
                            } else {
                                println!("Sent clipboard to {}", addr);
                            }
                        });
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
