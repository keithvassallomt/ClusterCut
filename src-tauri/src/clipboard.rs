use arboard::Clipboard;
use std::{thread, time::Duration};
use tauri::{AppHandle, Emitter};
use crate::transport::Transport;
use crate::state::AppState;

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
                if text != last_text && !text.is_empty() {
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
    // Spawn a thread to set clipboard to avoid blocking network loop
    thread::spawn(move || {
         match Clipboard::new() {
            Ok(mut c) => {
                if let Err(e) = c.set_text(text) {
                    eprintln!("Failed to set clipboard: {}", e);
                } else {
                    println!("Successfully set local clipboard content.");
                }
            },
            Err(e) => eprintln!("Failed to init clipboard for write: {}", e),
         }
    });
}
