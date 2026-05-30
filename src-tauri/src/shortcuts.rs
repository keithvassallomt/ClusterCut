//! Global keyboard-shortcut registration and handling.

use std::str::FromStr;

use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::protocol::Message;
use crate::state::AppState;
use crate::transport::Transport;
use crate::{clipboard, report_send_failure, request_clipboard_blob_internal, send_notification, NotificationPayload};

pub(crate) fn register_shortcuts(app_handle: &tauri::AppHandle) {
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

pub(crate) fn handle_shortcut(app_handle: &tauri::AppHandle, shortcut: &Shortcut, event: ShortcutEvent) {
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
               match clipboard::read_text(app_handle) {
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
                                blob: None,
                                formats: None,
                            };

                        // Emit local event
                        let _ = app_handle.emit("clipboard-change", &payload_obj);

                        // Send (mTLS handles confidentiality + sender auth).
                        let msg = Message::Clipboard(payload_obj);
                        if let Ok(data) = serde_json::to_vec(&msg) {
                            let transport = app_handle.state::<Transport>();
                            let peers = state.get_peers();
                            for p in peers.values() {
                                let addr = std::net::SocketAddr::new(p.ip, p.port);
                                let transport_clone = (*transport).clone();
                                let data_vec = data.clone();
                                let app_clone = app_handle.clone();
                                let peer_id = p.id.clone();
                                let peer_hostname = p.hostname.clone();
                                let peer_version = p.protocol_version.clone();
                                tauri::async_runtime::spawn(async move {
                                    if let Err(e) = transport_clone.send_message(addr, &data_vec).await {
                                        report_send_failure(
                                            &app_clone,
                                            &peer_id,
                                            &peer_hostname,
                                            peer_version.as_deref(),
                                            addr,
                                            &e.to_string(),
                                        );
                                    }
                                });
                            }

                            let notif_settings = settings.notifications.clone();
                            if notif_settings.data_sent {
                                send_notification(app_handle, "Clipboard Sent", "Manual broadcast successful.", false, Some(2), "history", NotificationPayload::None);
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
                let payload_opt = {
                    let mut guard = state.pending_clipboard.lock().unwrap();
                    guard.take()
                };
                if let Some(payload) = payload_opt {
                    // §3.3 descriptor: trigger an async fetch instead of
                    // pushing empty bytes onto the OS clipboard.
                    let is_descriptor = payload
                        .blob
                        .as_ref()
                        .map(|b| b.is_descriptor())
                        .unwrap_or(false);
                    if is_descriptor {
                        let app = app_handle.clone();
                        let state_clone = (*state).clone();
                        let id = payload.id.clone();
                        let peer_id = payload.sender_id.clone();
                        {
                            let mut slot = state_clone.in_flight_clipboard_fetch.lock().unwrap();
                            *slot = Some(id.clone());
                        }
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = request_clipboard_blob_internal(&state_clone, id, peer_id).await {
                                tracing::error!("Failed to fetch clipboard blob via shortcut: {}", e);
                            }
                        });
                        send_notification(&app, "Receiving Clipboard Image", "Fetching pending image…", false, Some(2), "history", NotificationPayload::None);
                    } else if let Some(blob) = payload.blob.clone() {
                        clipboard::set_clipboard_image(app_handle, blob);
                        tracing::info!("Confirmed pending clipboard image via shortcut.");
                        send_notification(app_handle, "Image Received", "Pending image applied.", false, Some(2), "history", NotificationPayload::None);
                    } else if let Err(e) = clipboard::write_text_direct(app_handle, payload.text) {
                        tracing::error!("Failed to write pending clipboard to system: {}", e);
                    } else {
                        tracing::info!("Confirmed pending clipboard content via shortcut.");
                        send_notification(app_handle, "Clipboard Received", "Pending content applied.", false, Some(2), "history", NotificationPayload::None);
                    }
                } else {
                    tracing::info!("No pending clipboard content to receive.");
                     send_notification(app_handle, "Manual Receive", "No pending content.", false, Some(3), "history", NotificationPayload::None);
                }
           }
        }
    }
}
