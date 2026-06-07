//! Clipboard send/receive/history commands.

use crate::state::AppState;
use crate::transport::Transport;
use crate::{NotificationPayload, send_notification, get_hostname_internal, report_send_failure};
use crate::protocol::Message;
use crate::{request_clipboard_blob_internal, request_file_internal};
use tauri::{Emitter, State};

#[tauri::command]
pub(crate) async fn send_clipboard(
    text: String,
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {

    // Manual Send Command
    crate::clipboard::set_clipboard(&app_handle, text.clone()); // Update local clipboard too? Yes, usually.

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
        blob: None,
        formats: None,
    };

    // Emit local event so history updates
    crate::clipboard::common::record_and_emit(&app_handle, &state, "clipboard-change", &payload_obj);

    // Send (mTLS provides confidentiality + sender auth; no app-layer
    // encryption needed since v0.3 dropped cluster_key).
    let msg = Message::Clipboard(payload_obj);
    let data = serde_json::to_vec(&msg).map_err(|e| e.to_string())?;

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
            } else {
                tracing::debug!("[Clipboard] Sent to {}", addr);
            }
        });
    }

    let notifications = state.settings.lock().unwrap().notifications.clone();
    if notifications.data_sent {
        send_notification(
            &app_handle,
            "Clipboard Sent",
            "Manual broadcast successful.",
            false,
            Some(2),
            "history",
            NotificationPayload::None,
        );
    }

    Ok(())
}

#[tauri::command]
pub(crate) async fn set_local_clipboard(app: tauri::AppHandle, text: String) -> Result<(), String> {
    crate::clipboard::set_clipboard(&app, text);
    Ok(())
}

#[tauri::command]
pub(crate) async fn set_local_clipboard_files(app: tauri::AppHandle, paths: Vec<String>) -> Result<(), String> {
    crate::clipboard::set_clipboard_paths(&app, paths);
    Ok(())
}

/// Re-copy a History item's retained content to the local OS clipboard,
/// keyed by id. No cluster broadcast.
#[tauri::command]
pub(crate) async fn recall_copy_history_item(
    id: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    use crate::clipboard::history_store::RecalledContent;
    let recalled = {
        let store = state.history_store.lock().unwrap();
        let entry = store
            .get(&id)
            .ok_or_else(|| "Content no longer available".to_string())?;
        entry.content.recall()?
    };
    match recalled {
        RecalledContent::Text(t) => crate::clipboard::set_clipboard(&app_handle, t),
        RecalledContent::Rich { text, formats } => {
            crate::clipboard::set_clipboard_rich(&app_handle, text, formats)
        }
        RecalledContent::Image {
            mime,
            bytes,
            width,
            height,
        } => {
            let blob = crate::protocol::ClipboardBlob::from_bytes(mime, &bytes, width, height);
            crate::clipboard::set_clipboard_image(&app_handle, blob);
        }
    }
    Ok(())
}

/// Re-broadcast a History item's retained content to the cluster, keyed by id.
/// Reconstructs the original clipboard content and runs it through the normal
/// broadcast path, so large items re-descriptor correctly.
#[tauri::command]
pub(crate) async fn recall_send_history_item(
    id: String,
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    use crate::clipboard::common::ClipboardContent;
    use crate::clipboard::history_store::RecalledContent;
    let recalled = {
        let store = state.history_store.lock().unwrap();
        let entry = store
            .get(&id)
            .ok_or_else(|| "Content no longer available".to_string())?;
        entry.content.recall()?
    };
    let content = match recalled {
        RecalledContent::Text(t) => ClipboardContent::Text(t),
        RecalledContent::Rich { text, formats } => ClipboardContent::Rich { text, formats },
        RecalledContent::Image {
            mime,
            bytes,
            width,
            height,
        } => {
            let blob = crate::protocol::ClipboardBlob::from_bytes(mime, &bytes, width, height);
            ClipboardContent::Image(blob)
        }
    };
    // Explicit user re-send: clear the echo/dedup guard so process_clipboard_change
    // does not suppress an image/rich item that still matches the last broadcast.
    *state.last_clipboard_content.lock().unwrap() = String::new();
    crate::clipboard::common::process_clipboard_change(content, &app_handle, &state, &transport);
    Ok(())
}

#[tauri::command]
pub(crate) async fn delete_history_item(
    app_handle: tauri::AppHandle,
    id: String,
    state: State<'_, AppState>,
    transport: State<'_, Transport>,
) -> Result<(), String> {
    // 1. Emit Local Event (to update UI immediately)
    tracing::info!("Deleting history item locally: {}", id);
    let _ = app_handle.emit("history-delete", &id);

    // Drop retained content + its disk file. Take the evicted entry out of the
    // history_store lock scope first, so we never hold history_store while
    // acquiring local_clipboard_blobs.
    let evicted = state.history_store.lock().unwrap().remove(&id);
    if let Some(evicted) = evicted {
        if let Some(path) = evicted.disk_path {
            let _ = std::fs::remove_file(&path);
        }
        state.local_clipboard_blobs.lock().unwrap().remove(&id);
    }

    // 2. Broadcast to Peers
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
pub(crate) async fn confirm_pending_clipboard(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let pending_opt = {
        let mut lock = state.pending_clipboard.lock().unwrap();
        lock.take() // Take it (clearing it)
    };

    if let Some(payload) = pending_opt {
        tracing::info!("Confirming pending clipboard from {}", payload.sender);

        // §3.3 descriptor: bytes weren't carried inline. Trigger a
        // FileRequest fetch over the `clustercut-file` ALPN; the stream
        // listener will land them on the OS clipboard once they arrive.
        if let Some(blob) = payload.blob.as_ref() {
            if blob.is_descriptor() {
                tracing::info!(
                    "[ClipboardBlob] Confirming descriptor fetch (id={}, total={:?})",
                    payload.id,
                    blob.total_size
                );
                {
                    let mut slot = state.in_flight_clipboard_fetch.lock().unwrap();
                    *slot = Some(payload.id.clone());
                }
                let mb = blob.total_size.unwrap_or(0) as f64 / (1024.0 * 1024.0);
                let notifications = state.settings.lock().unwrap().notifications.clone();
                if notifications.data_received {
                    send_notification(
                        &app_handle,
                        "Receiving Clipboard Image",
                        &format!("Receiving {:.1} MB image from {}…", mb, payload.sender),
                        false,
                        Some(2),
                        "history",
                        NotificationPayload::None,
                    );
                }
                return request_clipboard_blob_internal(&state, payload.id.clone(), payload.sender_id.clone()).await;
            }
        }

        if let Some(blob) = payload.blob.clone() {
            crate::clipboard::set_clipboard_image(&app_handle, blob);
        } else if let Some(formats) = payload
            .formats
            .as_ref()
            .filter(|fs| !fs.is_empty())
            .cloned()
        {
            crate::clipboard::set_clipboard_rich(&app_handle, payload.text.clone(), formats);
        } else {
            crate::clipboard::set_clipboard(&app_handle, payload.text.clone());
        }

        // Emit change event so history updates
        crate::clipboard::common::record_and_emit(&app_handle, &state, "clipboard-change", &payload);

        Ok(())
    } else {
        Err("No pending clipboard content".to_string())
    }
}

/// User-triggered "switch to rich format" promotion (issue #17 follow-up,
/// GNOME only). On receive, a Rich payload landed plain text on the clipboard
/// and stashed the full payload in `pending_rich_promotion`. This command
/// pops the stash and writes the rich formats — last-write-wins on the GNOME
/// extension, so what survives is the final rich MIME (`text/rtf` in our
/// current priority order). The `IGNORED_CONTENT` guard set by
/// `set_clipboard_rich_with_ignore` combined with `rich_eq_stable`'s lenient
/// subset rule catches the resulting truncated read-back, so the promotion
/// doesn't echo back to the sender. Idempotent — a second call with nothing
/// stashed returns Ok.
#[tauri::command]
pub(crate) async fn promote_pending_rich(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let promoted = {
        let mut slot = state.pending_rich_promotion.lock().unwrap();
        slot.take()
    };

    let Some(payload) = promoted else {
        tracing::debug!("promote_pending_rich called with nothing stashed");
        return Ok(());
    };

    let Some(formats) = payload
        .formats
        .as_ref()
        .filter(|fs| !fs.is_empty())
        .cloned()
    else {
        tracing::warn!(
            "promote_pending_rich: stashed payload had no rich formats; nothing to promote"
        );
        return Ok(());
    };

    tracing::info!(
        "Promoting rich clipboard from {}: text={} chars, formats=[{}]",
        payload.sender,
        payload.text.len(),
        formats.iter().map(|f| f.mime_type.as_str()).collect::<Vec<_>>().join(", ")
    );

    crate::clipboard::set_clipboard_rich(&app_handle, payload.text.clone(), formats);
    crate::clipboard::common::record_and_emit(&app_handle, &state, "clipboard-change", &payload);
    Ok(())
}

#[tauri::command]
pub(crate) async fn request_file(
    _app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    file_id: String,
    file_index: usize,
    peer_id: String,
) -> Result<(), String> {
    request_file_internal(&state, file_id, file_index, peer_id).await
}
