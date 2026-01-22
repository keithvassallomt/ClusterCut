use crate::crypto;
use crate::protocol::{ClipboardPayload, FileMetadata, Message};
use crate::state::AppState;
use crate::transport::Transport;
use std::{thread, time::Duration};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_clipboard::Clipboard;

// Use a shared cache to avoid feedback loops
use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
enum ClipboardContent {
    Text(String),
    Files(Vec<String>),
    None,
}

static IGNORED_CONTENT: Lazy<Arc<Mutex<ClipboardContent>>> =
    Lazy::new(|| Arc::new(Mutex::new(ClipboardContent::None)));

/// Read clipboard content (Files or Text) using the Tauri clipboard plugin
fn read_clipboard(app: &AppHandle) -> ClipboardContent {
    let clip = app.state::<Clipboard>();

    // Priority: Files > Text
    // Note: Check API availability. Assuming `read_files()` exists in CrossCopy plugin.
    match clip.read_files() {
        Ok(files) => {
            if !files.is_empty() {
                // Sanitize?
                // CrossCopy on Linux might return file:// URIs.
                // We should probably normalize to paths if needed, or keep as URIs?
                // For 'std::fs', we need paths.
                // Let's assume they are paths or URIs we can parse.
                // For now, store as is.
                return ClipboardContent::Files(files);
            }
        }
        Err(_) => {} // Ignore error, check text
    }

    match clip.read_text() {
        Ok(text) => {
            if !text.is_empty() {
                return ClipboardContent::Text(text);
            }
        }
        Err(_) => {}
    }

    ClipboardContent::None
}

/// Write clipboard text
pub fn set_system_clipboard(app: &AppHandle, text: String) -> Result<(), String> {
    app.state::<Clipboard>()
        .write_text(text)
        .map_err(|e| e.to_string())
}

/// Write clipboard files (paths)
pub fn set_clipboard_files(app: &AppHandle, files: Vec<String>) -> Result<(), String> {
    // Convert paths to file:// URIs
    let uris: Vec<String> = files
        .into_iter()
        .filter_map(|p| {
            let path = std::path::Path::new(&p);
            // Ensure absolute path
            let abs_path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().ok()?.join(path)
            };

            url::Url::from_file_path(abs_path)
                .ok()
                .map(|u| u.to_string())
        })
        .collect();

    if uris.is_empty() {
        return Err("No valid file paths convertible to URIs".to_string());
    }

    app.state::<Clipboard>()
        .write_files_uris(uris)
        .map_err(|e| e.to_string())
}

// Helper for lib.rs legacy call (also used for text)

pub fn set_clipboard(app: &AppHandle, text: String) {
    let app_handle = app.clone();
    let text_clone = text.clone();

    thread::spawn(move || {
        // Ignored check
        {
            let mut ignored = IGNORED_CONTENT.lock().unwrap();
            *ignored = ClipboardContent::Text(text_clone.clone());
        }

        if let Err(e) = set_system_clipboard(&app_handle, text_clone) {
            tracing::error!("Failed to set clipboard text: {}", e);
        } else {
            tracing::debug!("Successfully set local clipboard text.");
        }
    });
}

// New helper for files
pub fn set_clipboard_paths(app: &AppHandle, paths: Vec<String>) {
    let app_handle = app.clone();
    let paths_clone = paths.clone();

    thread::spawn(move || {
        {
            let mut ignored = IGNORED_CONTENT.lock().unwrap();
            *ignored = ClipboardContent::Files(paths_clone.clone());
        }

        if let Err(e) = set_clipboard_files(&app_handle, paths_clone) {
            tracing::error!("Failed to set clipboard files: {}", e);
        } else {
            tracing::debug!("Successfully set local clipboard files.");
        }
    });
}

pub fn start_monitor(app_handle: AppHandle, state: AppState, transport: Transport) {
    thread::spawn(move || {
        let mut last_content = read_clipboard(&app_handle);

        // Polling loop
        loop {
            if state.is_shutdown() {
                tracing::info!("Clipboard monitor received shutdown signal, exiting.");
                break;
            }

            let current_content = read_clipboard(&app_handle);

            // Check Ignored (Feedback Loop)
            let mut should_process = false;
            {
                let mut ignored = IGNORED_CONTENT.lock().unwrap();
                match &*ignored {
                    ClipboardContent::None => {
                        if current_content != last_content
                            && current_content != ClipboardContent::None
                        {
                            should_process = true;
                        }
                    }
                    ClipboardContent::Text(ign_text) => {
                        if let ClipboardContent::Text(curr_text) = &current_content {
                            if curr_text == ign_text {
                                // Match! This is our echo.
                                // Reset ignored, update last_content
                                last_content = current_content.clone();
                                *ignored = ClipboardContent::None;
                            } else {
                                // Different text?
                                // If it's different, it might be a user copy.
                                // But maybe we haven't seen the echo yet?
                                // Optimized: If current != ignored, and current != last, then it's new.
                                if current_content != last_content {
                                    should_process = true;
                                    // But if we are expecting Ignored, and we see something else,
                                    // maybe we should keep Ignored set?
                                    // Or maybe the user overwrote it immediately.
                                    // Let's assume if it's different, we process it.
                                    // We only clear Ignored if we match it.
                                    // (Or timeout? todo)
                                }
                            }
                        } else {
                            // Type mismatch (ignoring text, got files). Process files.
                            if current_content != last_content
                                && current_content != ClipboardContent::None
                            {
                                should_process = true;
                            }
                        }
                    }
                    ClipboardContent::Files(ign_files) => {
                        if let ClipboardContent::Files(curr_files) = &current_content {
                            if curr_files == ign_files {
                                // distinct paths check
                                last_content = current_content.clone();
                                *ignored = ClipboardContent::None;
                            } else {
                                if current_content != last_content {
                                    should_process = true;
                                }
                            }
                        } else {
                            if current_content != last_content
                                && current_content != ClipboardContent::None
                            {
                                should_process = true;
                            }
                        }
                    }
                }
            }

            if should_process {
                last_content = current_content.clone();

                // Process Change
                match current_content {
                    ClipboardContent::Text(text) => {
                        tracing::debug!("Clipboard Text Change Detected (len={})", text.len());

                        // Dedupe Global
                        {
                            let mut last_global = state.last_clipboard_content.lock().unwrap();
                            if *last_global == text {
                                // Double check?
                            } else {
                                *last_global = text.clone();
                            }
                        }

                        let hostname = crate::get_hostname_internal();
                        let msg_id = uuid::Uuid::new_v4().to_string();
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        let local_id = state.local_device_id.lock().unwrap().clone();
                        let payload_obj = ClipboardPayload {
                            id: msg_id.clone(),
                            text: text.clone(),
                            files: None,
                            timestamp: ts,
                            sender: hostname,
                            sender_id: local_id,
                        };

                        broadcast_clipboard(&app_handle, &state, &transport, payload_obj);
                    }
                    ClipboardContent::Files(raw_paths) => {
                        tracing::debug!(
                            "Clipboard File Change Detected. Raw paths: {:?}",
                            raw_paths
                        );
                        // Dedupe logic for files?
                        // For now rely on last_content local dedupe.

                        let hostname = crate::get_hostname_internal();
                        let msg_id = uuid::Uuid::new_v4().to_string();
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        // Process Metadata & Validate Paths
                        let mut file_metas = Vec::new();
                        let mut valid_paths = Vec::new();

                        for path_str in &raw_paths {
                            // Try to parse as URL first
                            let path_buf = if let Ok(u) = url::Url::parse(path_str) {
                                if u.scheme() == "file" {
                                    if let Ok(p) = u.to_file_path() {
                                        p
                                    } else {
                                        // Metadata decoding failed or not a local file
                                        std::path::PathBuf::from(path_str)
                                    }
                                } else {
                                    std::path::PathBuf::from(path_str)
                                }
                            } else {
                                std::path::PathBuf::from(path_str)
                            };

                            let path = path_buf.as_path();
                            if path.exists() {
                                let name = path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string();
                                let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                                file_metas.push(FileMetadata { name, size });
                                valid_paths.push(path.to_string_lossy().to_string());
                            } else {
                                tracing::warn!("Path does not exist or invalid: {:?}", path);
                            }
                        }

                        if !file_metas.is_empty() {
                            // Store files mapping for serving requests (Use VALID paths)
                            {
                                let mut files_lock = state.local_files.lock().unwrap();
                                files_lock.insert(msg_id.clone(), valid_paths.clone());
                            }

                            let local_id = state.local_device_id.lock().unwrap().clone();
                            let payload_obj = ClipboardPayload {
                                id: msg_id.clone(),
                                text: String::new(), // Empty text for files
                                files: Some(file_metas),
                                timestamp: ts,
                                sender: hostname,
                                sender_id: local_id,
                            };
                            broadcast_clipboard(&app_handle, &state, &transport, payload_obj);
                        } else {
                            tracing::warn!("No valid files found in clipboard content.");
                        }
                    }
                    ClipboardContent::None => {}
                }
            }

            thread::sleep(Duration::from_millis(500));
        }
    }); // end spawn
}

fn broadcast_clipboard(
    app_handle: &AppHandle,
    state: &AppState,
    transport: &Transport,
    payload_obj: ClipboardPayload,
) {
    // Emit Local Event
    let _ = app_handle.emit("clipboard-change", &payload_obj);

    // Check Auto-Send
    let auto_send = { state.settings.lock().unwrap().auto_send };
    if !auto_send {
        tracing::debug!("Auto-send disabled. Skipping broadcast.");
        return;
    }

    // Encrypt
    let payload_bytes = match serde_json::to_vec(&payload_obj) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("Failed to serialize clipboard payload: {}", e);
            return;
        }
    };

    let ck_lock = state.cluster_key.lock().unwrap();
    if let Some(key) = ck_lock.as_ref() {
        if key.len() == 32 {
            let mut key_arr = [0u8; 32];
            key_arr.copy_from_slice(key);

            match crypto::encrypt(&key_arr, &payload_bytes) {
                Ok(cipher) => {
                    // Send
                    let msg = Message::Clipboard(cipher);
                    let data = serde_json::to_vec(&msg).unwrap_or_default(); // 1MB+ if strictly JSON.
                                                                             // IMPORTANT: Files are NOT sent here. Only Metadata.
                                                                             // The payload only contains file paths/sizes.

                    let peers = state.get_peers();
                    if !peers.is_empty() {
                        // Notification for "Sending..."?
                        // Maybe only if files?
                        let notifications = state.settings.lock().unwrap().notifications.clone();
                        if notifications.data_sent {
                            let body = if payload_obj.files.is_some() {
                                "File info broadcasted to cluster."
                            } else {
                                "Clipboard content broadcasted to cluster."
                            };
                            crate::send_notification(app_handle, "Clipboard Sent", body);
                        }
                    }

                    for peer in peers.values() {
                        let addr = std::net::SocketAddr::new(peer.ip, peer.port);
                        let transport_clone = transport.clone();
                        let data_vec = data.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = transport_clone.send_message(addr, &data_vec).await {
                                tracing::error!("Failed to send to {}: {}", addr, e);
                            } else {
                                tracing::info!("Sent clipboard to {}", addr);
                            }
                        });
                    }
                }
                Err(e) => tracing::error!("Encryption failed: {}", e),
            }
        }
    }
}
