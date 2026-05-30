//! Inbound QUIC message and stream handlers.

use crate::protocol::Message;
use crate::state::AppState;
use crate::transport::Transport;
use crate::{net_util, storage};
use crate::{NotificationPayload, send_notification, get_hostname_internal, check_and_notify_leave, perform_factory_reset};
use tauri::{Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncBufReadExt, BufReader};
use std::path::PathBuf;
use tokio::fs::File;

/// Read a §3.3 clipboard-blob stream into memory and land it on the OS
/// clipboard. The header has already been parsed and confirmed to carry
/// `DeliveryTarget::Clipboard{…}`. Auth-token verification mirrors the file
/// path. Race protection: if `state.in_flight_clipboard_fetch` no longer
/// holds this id by the time bytes finish arriving, a newer clipboard event
/// has superseded this one — we still drain the stream to keep QUIC happy
/// but skip writing to the OS clipboard.
async fn handle_incoming_clipboard_blob_stream(
    mut reader: BufReader<quinn::RecvStream>,
    header: crate::protocol::FileStreamHeader,
    mime_type: String,
    width: Option<u32>,
    height: Option<u32>,
    addr: std::net::SocketAddr,
    state: AppState,
    app: tauri::AppHandle,
) {
    tracing::info!(
        "Receiving Clipboard Blob: mime={}, {} bytes, id={}, from={}",
        mime_type, header.file_size, header.id, addr
    );

    // No app-layer auth token to verify — the QUIC connection itself is
    // mTLS-pinned to the sending peer (see issue #9 follow-up).
    //
    // Drain the stream into memory. Cap defensively at MAX_CLIPBOARD_IMAGE_BYTES
    // so a malformed sender can't OOM the receiver.
    let cap = crate::clipboard::common::MAX_CLIPBOARD_IMAGE_BYTES;
    let mut accum: Vec<u8> = Vec::with_capacity(header.file_size.min(cap as u64) as usize);
    let mut buf = vec![0u8; 1024 * 1024];
    let mut last_emit = std::time::Instant::now();
    let start_time = std::time::Instant::now();
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if accum.len() + n > cap {
                    tracing::error!(
                        "Clipboard-blob stream exceeds {} byte cap (got {}); dropping.",
                        cap,
                        accum.len() + n
                    );
                    // Drain remainder of stream to keep QUIC happy, but stop accumulating.
                    let mut sink = vec![0u8; 1024 * 1024];
                    while let Ok(n2) = reader.read(&mut sink).await {
                        if n2 == 0 { break; }
                    }
                    return;
                }
                accum.extend_from_slice(&buf[..n]);
                if last_emit.elapsed().as_millis() > 200 {
                    let _ = app.emit("file-progress", serde_json::json!({
                        "id": header.id,
                        "fileName": format!("Clipboard image ({})", mime_type),
                        "total": header.file_size,
                        "transferred": accum.len() as u64,
                    }));
                    last_emit = std::time::Instant::now();
                }
            }
            Err(e) => {
                tracing::error!("Clipboard-blob stream read error: {}", e);
                return;
            }
        }
    }
    let total_time = start_time.elapsed();
    tracing::info!(
        "Clipboard-blob stream complete: {} bytes in {:?} (mime={})",
        accum.len(),
        total_time,
        mime_type
    );

    if accum.len() as u64 != header.file_size {
        tracing::warn!(
            "Clipboard-blob size mismatch: header says {} bytes, got {} bytes — dropping.",
            header.file_size,
            accum.len()
        );
        return;
    }

    // Race protection: only land on clipboard if this id is still the in-flight one.
    let still_current = {
        let mut slot = state.in_flight_clipboard_fetch.lock().unwrap();
        match slot.as_ref() {
            Some(s) if *s == header.id => {
                *slot = None;
                true
            }
            _ => false,
        }
    };
    if !still_current {
        tracing::info!(
            "[ClipboardBlob] Discarding fetched bytes for id={} — superseded by a newer clipboard event",
            header.id
        );
        return;
    }

    // Reconstruct a ClipboardBlob and drive it onto the OS clipboard via the
    // same `set_clipboard_image` that the inline path uses.
    let blob = crate::protocol::ClipboardBlob::from_bytes(
        mime_type.clone(),
        &accum,
        width,
        height,
    );

    let auto_recv = { state.settings.lock().unwrap().auto_receive };
    if auto_recv {
        crate::clipboard::set_clipboard_image(&app, blob.clone());
    } else {
        // Manual mode — stash the now-fully-fetched blob in pending_clipboard
        // so the user can confirm via the existing UI.
        let payload = crate::protocol::ClipboardPayload {
            id: header.id.clone(),
            text: String::new(),
            files: None,
            blob: Some(blob.clone()),
            formats: None,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: format!("{}", addr),
            sender_id: String::new(),
        };
        let mut pending = state.pending_clipboard.lock().unwrap();
        *pending = Some(payload);
    }

    // Surface to the history view as a normal clipboard-change event (so
    // the entry shows up in history with a thumbnail / size).
    let payload_event = crate::protocol::ClipboardPayload {
        id: header.id.clone(),
        text: String::new(),
        files: None,
        blob: Some(blob),
        formats: None,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        sender: format!("{}", addr),
        sender_id: String::new(),
    };
    let _ = app.emit("clipboard-change", &payload_event);

    let notifications = state.settings.lock().unwrap().notifications.clone();
    if notifications.data_received {
        let mb = accum.len() as f64 / (1024.0 * 1024.0);
        send_notification(
            &app,
            "Image Available to Paste",
            &format!("{:.1} MB image is now on the clipboard.", mb),
            false,
            Some(3),
            "history",
            NotificationPayload::None,
        );
    }
}

pub(crate) async fn handle_incoming_file_stream(recv: quinn::RecvStream, addr: std::net::SocketAddr, state: AppState, app: tauri::AppHandle) {
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

    // §3.3 routing: clipboard-blob streams accumulate bytes in memory and
    // land on the OS clipboard. File streams keep the existing temp-download
    // path. The two share auth-token verification and the QUIC drain dance,
    // but everything past the header is structurally different.
    if let crate::protocol::DeliveryTarget::Clipboard { mime_type, width, height } = header.delivery_target.clone() {
        handle_incoming_clipboard_blob_stream(reader, header, mime_type, width, height, addr, state, app).await;
        return;
    }

    tracing::info!("Receiving File: {} ({} bytes) [ID: {}]", header.file_name, header.file_size, header.id);

    // 2. Prepare Output File
    // Use Cache Directory -> temp_downloads
    let root_cache_dir = match app.path().app_cache_dir() {
        Ok(p) => p,
        Err(e) => {
             tracing::error!("Failed to get cache dir: {}", e);
             return;
        }
    };

    let cache_dir = root_cache_dir.join("temp_downloads");

    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        tracing::error!("Failed to create cache dir: {}", e);
        return;
    }

    // Handle name collision (append (n))
    let mut file_path = cache_dir.join(&header.file_name);

    if file_path.exists() {
        tracing::info!("File collision detected for {}, renaming...", header.file_name);
        let path_obj = std::path::Path::new(&header.file_name);
        let file_stem = path_obj.file_stem().map(|s| s.to_string_lossy()).unwrap_or_else(|| std::borrow::Cow::from(&header.file_name));
        let extension = path_obj.extension().map(|s| s.to_string_lossy());

        let mut counter = 1;
        while file_path.exists() {
            let new_name = match &extension {
                Some(ext) => format!("{} ({}).{}", file_stem, counter, ext),
                None => format!("{} ({})", file_stem, counter),
            };
            file_path = cache_dir.join(new_name);
            counter += 1;
        }
        tracing::info!("Renamed to {:?}", file_path.file_name());
    }

    let mut file = match File::create(&file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Failed to create file {:?}: {}", file_path, e);
            return;
        }
    };

    // 3. No app-layer auth token to verify — sender identity is already
    //    authenticated by the QUIC mTLS handshake (see issue #9 follow-up).
    tracing::info!("Starting Download...");

    // 4. Stream Data (Zero-Copy-ish)
    let start_time = std::time::Instant::now();

    // reader is BufReader<RecvStream>. We loop manually so we can emit progress.
    // total_written counts bytes written to disk (post-decompression on the compressed
    // path), so the progress percentage matches header.file_size — the *uncompressed*
    // size — regardless of whether the wire payload was compressed.

    let mut buf = vec![0u8; 1024 * 1024]; // 1MB Buffer
    let mut total_written = 0u64;
    let mut last_emit = std::time::Instant::now();
    let mut chunk_count = 0;

    if header.compressed {
        tracing::info!("[Receiver] Starting ZSTD Stream. Expecting {} bytes (decompressed).", header.file_size);
        let mut decoder = async_compression::tokio::bufread::ZstdDecoder::new(reader);
        loop {
            match decoder.read(&mut buf).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    if let Err(e) = file.write_all(&buf[0..n]).await {
                        tracing::error!("File Write Error: {}", e);
                        break;
                    }
                    total_written += n as u64;
                    chunk_count += 1;

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
                    tracing::error!("Decompressed Stream Read Error: {}", e);
                    break;
                }
            }
        }
    } else {
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
     let _ = app.emit("file-received", serde_json::json!({
         "id": header.id,
         "file_name": header.file_name,
         "file_size": header.file_size,
         "file_index": header.file_index,
         "path": file_path.to_string_lossy()
     }));

     // Notification
     let settings = state.settings.lock().unwrap();
     if settings.notify_large_files && header.file_size > settings.max_auto_download_size {
         let body = format!("Download complete: {}", header.file_name);
         send_notification(&app, "Download Complete", &body, false, None, "history", NotificationPayload::None);
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

pub(crate) async fn handle_message(msg: Message, addr: std::net::SocketAddr, listener_state: AppState, listener_handle: tauri::AppHandle, transport_inside: Transport) {
    match msg {
        Message::Clipboard(payload) => {
            tracing::debug!("Received Clipboard from {}", addr);
            let text = payload.text.clone();
            let id = payload.id.clone();
            let ts = payload.timestamp;
            let sender = payload.sender.clone();
            {
                            // Verify Timestamp Freshness (120s threshold)
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();

                            let diff = if now > ts {
                                now - ts
                            } else {
                                ts - now // Future timestamp (clock skew)
                            };

                            if diff > 120 {
                                tracing::warn!("Ignored stale clipboard message from {} (Timestamp: {}, Now: {}, Diff: {}s)", sender, ts, now, diff);
                                return;
                            }

                            // Self-sender check
                            {
                                let my_hostname = get_hostname_internal();
                                if sender == my_hostname {
                                    tracing::debug!("Ignoring clipboard message from self (sender={})", sender);
                                    return;
                                }
                            }

                            // Loop/Dedupe Check — must match the sender-side
                            // signature in clipboard::common::payload_signature
                            // so a blob received from a peer correctly suppresses
                            // an immediate re-broadcast back to the cluster.
                            let content_signature =
                                crate::clipboard::common::payload_signature(&payload);

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
                                blob: payload.blob.clone(),
                                formats: payload.formats.clone(),
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
                                    let (auto_recv, enable_ft, size_limit, notify_large) = {
                                        let s = listener_state.settings.lock().unwrap();
                                        (s.auto_receive, s.enable_file_transfer, s.max_auto_download_size, s.notify_large_files)
                                    };

                                    if !enable_ft {
                                        tracing::info!("File transfer disabled in settings. Ignoring auto-download.");
                                    } else {
                                        let mut total_size = 0u64;
                                        for f in files { total_size += f.size; }

                                        tracing::info!("File Transfer Logic: AutoRecv={}, TotalSize={}, Limit={}, NotifyLarge={}", auto_recv, total_size, size_limit, notify_large);

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
                                                let msg = Message::FileRequest(req_payload);
                                                if let Ok(data) = serde_json::to_vec(&msg) {
                                                    let transport_clone = transport_inside.clone();
                                                    let addr_clone = addr;
                                                    tauri::async_runtime::spawn(async move {
                                                        let _ = transport_clone.send_message(addr_clone, &data).await;
                                                    });
                                                }
                                            }
                                        } else {
                                            // Too large or auto-recv off
                                            if notify_large {
                                                tracing::info!("Large file or manual mode. Sending notification.");
                                                let body = format!("Received {} files from {}. Click to download.", files.len(), sender);
                                                let _body = format!("Received {} files from {}. Click to download.", files.len(), sender);
                                                // Create Payload for Download Button
                                                let payload = NotificationPayload::DownloadAvailable {
                                                    msg_id: id.clone(),
                                                    file_count: files.len(),
                                                    peer_id: payload.sender_id.clone(),
                                                };
                                                send_notification(&listener_handle, "Files Available", &body, true, None, "history", payload);
                                            } else {
                                                tracing::warn!("Large file received but 'notify_large_files' is FALSE. No notification sent.");
                                            }
                                        }
                                    } // End if !enable_ft else
                                } // End if !files.is_empty()
                            } // End if let Some(files)

                            // BLOB HANDLING (image clipboard data)
                            // Race protection: any fresh clipboard event from
                            // a peer supersedes an older in-flight clipboard-
                            // blob fetch. Cleared here unconditionally; the
                            // descriptor-fetch branch below overwrites with
                            // its own id immediately. Bytes from the older
                            // fetch still drain off the wire (so QUIC stays
                            // happy) but are discarded by the file-stream
                            // listener's id check.
                            {
                                let mut slot = listener_state.in_flight_clipboard_fetch.lock().unwrap();
                                *slot = None;
                            }
                            if let Some(blob) = payload_obj.blob.clone() {
                                if blob.is_descriptor() {
                                    // §3.3 large-blob descriptor path. Bytes
                                    // ride the `clustercut-file` ALPN, not
                                    // inline. Decide auto-fetch vs. user-
                                    // confirm based on `max_auto_download_size`.
                                    let total_size = blob.total_size.unwrap_or(0);
                                    let mb = total_size as f64 / (1024.0 * 1024.0);
                                    let (auto_recv, enable_ft, size_limit) = {
                                        let s = listener_state.settings.lock().unwrap();
                                        (s.auto_receive, s.enable_file_transfer, s.max_auto_download_size)
                                    };
                                    tracing::info!(
                                        "Received clipboard image descriptor from {}: mime={}, total={} bytes{} fetch_id={}",
                                        sender,
                                        blob.mime_type,
                                        total_size,
                                        match (blob.width, blob.height) {
                                            (Some(w), Some(h)) => format!(", {}x{},", w, h),
                                            _ => String::new(),
                                        },
                                        blob.fetch_id.as_deref().unwrap_or("?")
                                    );

                                    if !enable_ft {
                                        tracing::info!("File transfer disabled in settings. Ignoring large clipboard descriptor.");
                                    } else if !auto_recv {
                                        // Manual mode — stash for confirm-via-UI.
                                        tracing::info!("[Clipboard] Auto-receive OFF. Storing pending clipboard descriptor from {}", sender);
                                        {
                                            let mut pending = listener_state.pending_clipboard.lock().unwrap();
                                            *pending = Some(payload_obj.clone());
                                        }
                                        let _ = listener_handle.emit("clipboard-pending", &payload_obj);

                                        // Notification is the primary cue that an
                                        // accept is waiting — gate on
                                        // `notify_large_files` (defaults true) so
                                        // it fires even when `data_received` is
                                        // off, mirroring the file-transfer accept
                                        // notification.
                                        let notify_large = listener_state.settings.lock().unwrap().notify_large_files;
                                        if notify_large {
                                            send_notification(
                                                &listener_handle,
                                                "Large Clipboard Image",
                                                &format!("{:.1} MB image from {} — accept to receive.", mb, sender),
                                                true,
                                                Some(3),
                                                "history",
                                                NotificationPayload::None,
                                            );
                                        }
                                    } else if total_size > size_limit {
                                        // Tier B2 — over auto-download threshold. Stash and notify with Accept.
                                        tracing::info!(
                                            "[ClipboardBlob] Descriptor {} bytes exceeds auto-download limit {} bytes — awaiting accept",
                                            total_size,
                                            size_limit
                                        );
                                        {
                                            let mut pending = listener_state.pending_clipboard.lock().unwrap();
                                            *pending = Some(payload_obj.clone());
                                        }
                                        let _ = listener_handle.emit("clipboard-pending", &payload_obj);

                                        let notify_large = listener_state.settings.lock().unwrap().notify_large_files;
                                        if notify_large {
                                            send_notification(
                                                &listener_handle,
                                                "Large Clipboard Image",
                                                &format!("{:.1} MB image from {} — accept to receive.", mb, sender),
                                                true,
                                                Some(3),
                                                "history",
                                                NotificationPayload::None,
                                            );
                                        }
                                    } else {
                                        // Tier B1 — auto-fetch via file-transfer ALPN.
                                        tracing::info!(
                                            "[ClipboardBlob] Auto-fetching descriptor ({} bytes, mime={})",
                                            total_size,
                                            blob.mime_type
                                        );
                                        // Race protection: mark this fetch as the in-flight one.
                                        // A newer event arriving mid-stream will overwrite the slot
                                        // and the older payload's bytes will still drain off the
                                        // wire but won't land on the OS clipboard.
                                        {
                                            let mut slot = listener_state.in_flight_clipboard_fetch.lock().unwrap();
                                            *slot = Some(id.clone());
                                        }

                                        let _ = listener_handle.emit("clipboard-blob-fetching", &payload_obj);

                                        let notifications = listener_state.settings.lock().unwrap().notifications.clone();
                                        if notifications.data_received {
                                            send_notification(
                                                &listener_handle,
                                                "Receiving Clipboard Image",
                                                &format!("Receiving {:.1} MB image from {}…", mb, sender),
                                                false,
                                                Some(2),
                                                "history",
                                                NotificationPayload::None,
                                            );
                                        }

                                        let req_payload = crate::protocol::FileRequestPayload {
                                            id: id.clone(),
                                            file_index: 0,
                                            offset: 0,
                                        };
                                        let msg = Message::FileRequest(req_payload);
                                        if let Ok(data) = serde_json::to_vec(&msg) {
                                            let transport_clone = transport_inside.clone();
                                            let sender_addr = addr;
                                            tauri::async_runtime::spawn(async move {
                                                if let Err(e) = transport_clone.send_message(sender_addr, &data).await {
                                                    tracing::error!("Failed to send clipboard FileRequest to {}: {}", sender_addr, e);
                                                }
                                            });
                                        }
                                    }
                                } else {
                                    let blob_size = blob.decoded_len();
                                    tracing::info!(
                                        "Received clipboard image from {}: mime={}, decoded={} bytes{}",
                                        sender,
                                        blob.mime_type,
                                        blob_size,
                                        match (blob.width, blob.height) {
                                            (Some(w), Some(h)) => format!(", {}x{}", w, h),
                                            _ => String::new(),
                                        }
                                    );
                                    let auto_receiver = { listener_state.settings.lock().unwrap().auto_receive };
                                    if auto_receiver {
                                        crate::clipboard::set_clipboard_image(&listener_handle, blob);
                                        let _ = listener_handle.emit("clipboard-change", &payload_obj);
                                    } else {
                                        tracing::info!("[Clipboard] Auto-receive OFF. Storing pending blob from {}", sender);
                                        {
                                            let mut pending = listener_state.pending_clipboard.lock().unwrap();
                                            *pending = Some(payload_obj.clone());
                                        }
                                        let _ = listener_handle.emit("clipboard-pending", &payload_obj);
                                    }

                                    let notifications = listener_state.settings.lock().unwrap().notifications.clone();
                                    if notifications.data_received {
                                        // Large blobs (§3.3 v1) get a more specific
                                        // notification with the size, so users know
                                        // the (potentially many MB) image is now
                                        // available to paste even if there was a
                                        // perceptible transfer delay.
                                        if blob_size > crate::clipboard::common::LARGE_CLIPBOARD_BLOB_NOTIFY_THRESHOLD {
                                            let mb = blob_size as f64 / (1024.0 * 1024.0);
                                            send_notification(
                                                &listener_handle,
                                                "Large Image Received",
                                                &format!("{:.1} MB image from {} is now on the clipboard.", mb, sender),
                                                false,
                                                Some(3),
                                                "history",
                                                NotificationPayload::None,
                                            );
                                        } else {
                                            send_notification(&listener_handle, "Image Received", "Image copied to clipboard", false, Some(2), "history", NotificationPayload::None);
                                        }
                                    }
                                }
                            }

                            // RICH HANDLING (text + alternate formats like text/html, text/rtf).
                            // Takes precedence over plain TEXT HANDLING so destination apps see
                            // the multi-MIME buffet the source had. Backends that can't yet write
                            // multi-format fall back to plain text inside set_clipboard_rich.
                            let rich_formats = payload_obj
                                .formats
                                .as_ref()
                                .filter(|fs| !fs.is_empty())
                                .cloned();

                            if let Some(formats) = rich_formats {
                                tracing::info!(
                                    "Received clipboard rich from {}: text={} chars, formats=[{}]",
                                    sender,
                                    text.len(),
                                    formats.iter().map(|f| f.mime_type.as_str()).collect::<Vec<_>>().join(", ")
                                );
                                // GNOME-only two-stage promotion (issue #17 follow-up).
                                // mutter's `Meta.SelectionSource` is single-MIME and
                                // can't be subclassed for multi-MIME from GJS (GJS #255),
                                // so the extension's `_writeFormats` is last-write-wins.
                                // Writing the rich payload directly leaves *only* the
                                // final rich MIME advertised — plain-text consumers
                                // (gedit, GNOME Text Editor, OnlyOffice, browser inputs)
                                // then get nothing on paste. Apply plain text by default
                                // so the broad case works, stash the full payload, and
                                // emit `rich-promotion-available` so the UI can offer a
                                // one-click "switch to rich format" promotion. Other
                                // backends (Windows, macOS, wlroots) write all MIMEs
                                // atomically and don't need this path.
                                let needs_promotion_dance: bool = {
                                    #[cfg(target_os = "linux")]
                                    {
                                        matches!(
                                            crate::clipboard::get_backend(),
                                            crate::clipboard::ClipboardBackend::GnomeExtension
                                        ) && !text.trim().is_empty()
                                    }
                                    #[cfg(not(target_os = "linux"))]
                                    {
                                        false
                                    }
                                };

                                let auto_receiver = { listener_state.settings.lock().unwrap().auto_receive };
                                if auto_receiver {
                                    if needs_promotion_dance {
                                        {
                                            let mut stash = listener_state.pending_rich_promotion.lock().unwrap();
                                            *stash = Some(payload_obj.clone());
                                        }
                                        crate::clipboard::set_clipboard(&listener_handle, text.clone());
                                        let _ = listener_handle.emit("clipboard-change", &payload_obj);
                                    } else {
                                        crate::clipboard::set_clipboard_rich(&listener_handle, text.clone(), formats);
                                        let _ = listener_handle.emit("clipboard-change", &payload_obj);
                                    }
                                } else {
                                    tracing::info!("[Clipboard] Auto-receive OFF. Storing pending rich clipboard from {}", sender);
                                    {
                                        let mut pending = listener_state.pending_clipboard.lock().unwrap();
                                        *pending = Some(payload_obj.clone());
                                    }
                                    let _ = listener_handle.emit("clipboard-pending", &payload_obj);
                                }

                                if needs_promotion_dance {
                                    // The promotion notification is the *only* path
                                    // to the rich format on a GNOME receiver — without
                                    // it the user has no way to upgrade past the
                                    // plain-text fallback. Surface unconditionally,
                                    // not gated on the generic `data_received`
                                    // toggle (which is off by default and used for
                                    // purely informational pings).
                                    send_notification(
                                        &listener_handle,
                                        "Pasted as plain text",
                                        &format!(
                                            "From {}. Click \"Switch to Rich\" to upgrade.",
                                            sender
                                        ),
                                        false,
                                        Some(2),
                                        "history",
                                        NotificationPayload::PromoteRichClipboard,
                                    );
                                } else {
                                    let notifications = listener_state.settings.lock().unwrap().notifications.clone();
                                    if notifications.data_received {
                                        send_notification(
                                            &listener_handle,
                                            "Clipboard Received",
                                            "Formatted content copied to clipboard",
                                            false,
                                            Some(2),
                                            "history",
                                            NotificationPayload::None,
                                        );
                                    }
                                }
                            } else if !text.trim().is_empty() {
                                // TEXT HANDLING — plain text only, no rich formats present.
                                // `trim().is_empty()` (not just `is_empty()`) drops
                                // whitespace-only payloads — e.g. a single newline or
                                // space bouncing around the cluster, which would
                                // otherwise overwrite a useful clipboard on every peer.
                                // Symmetric with the broadcast-side guard in
                                // `clipboard::common::process_clipboard_change`.
                                tracing::info!(
                                    "Received clipboard text from {}: {} chars",
                                    sender,
                                    text.len()
                                );
                                let auto_receiver = { listener_state.settings.lock().unwrap().auto_receive };
                                if auto_receiver {
                                    crate::clipboard::set_clipboard(&listener_handle, text.clone());
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
                                    send_notification(&listener_handle, "Clipboard Received", "Content copied to clipboard", false, Some(2), "history", NotificationPayload::None);
                                }
                            }

                            // Relay Logic — re-broadcast to other cluster
                            // members (mTLS authenticates each hop; no
                            // app-layer encryption needed).
                            let auto_send = { listener_state.settings.lock().unwrap().auto_send };
                            if !auto_send {
                                return;
                            }

                            let sender_addr = addr;
                            let relay_data = serde_json::to_vec(&Message::Clipboard(payload_obj.clone())).unwrap_or_default();
                            let peers = listener_state.get_peers();
                            for p in peers.values() {
                                let p_addr = std::net::SocketAddr::new(p.ip, p.port);
                                if p_addr == sender_addr { continue; }
                                let _ = transport_inside.send_message(p_addr, &relay_data).await;
                            }
            }
        }
        Message::HistoryDelete(id) => {
            tracing::info!("Received HistoryDelete for ID: {}", id);
            let _ = listener_handle.emit("history-delete", &id);
        }
        Message::PeerDiscovery(mut peer) => {
            tracing::debug!("Received PeerDiscovery for {}", peer.hostname);

            let local_id = listener_state.local_device_id.lock().unwrap().clone();
            if peer.id == local_id {
                // Collision Detection:
                // If the sender IP is NOT one of our local IPs, then it's a remote device with the same ID.
                // This shouldn't happen unless the device was cloned (e.g. VM clone).
                let sender_ip = addr.ip();
                if !net_util::is_local_ip(sender_ip) {
                     tracing::warn!("Device ID Collision Detected! Remote peer at {} has the same ID as me ({}).", sender_ip, local_id);
                     send_notification(&listener_handle,
                         "Configuration Error",
                         &format!("Device ID Collision! Another device at {} shares your ID. Please reset one device.", sender_ip),
                         true,
                         None,
                         "settings",
                         NotificationPayload::None
                     );
                }
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
                     // Don't let a gossip update without a fingerprint clobber an
                     // already-pinned one. Sticky pinning until re-pair.
                     if peer.fingerprint.is_none() {
                         peer.fingerprint = existing.fingerprint.clone();
                     }
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

                 // Under v0.3 mTLS, the gossip arrived over an authenticated
                 // QUIC connection — the sender's cert had to match a paired
                 // peer's pinned fingerprint or it would not have been accepted.
                 // Transitive trust: a paired peer's gossip about any peer is
                 // taken as cluster membership.
                 peer.is_trusted = true;

                 listener_state.add_peer(peer.clone());
                 let _ = listener_handle.emit("peer-update", crate::peer::PeerView::from_peer(&peer));

                 // Fire deferred join notification if this peer was pending verification
                 {
                     let mut pending_joins = listener_state.pending_join_notifications.lock().unwrap();
                     if pending_joins.remove(&peer.id) {
                         if listener_state.should_notify()
                             && listener_state.settings.lock().unwrap().notifications.device_join
                         {
                             tracing::info!("[Notification] Deferred 'Device Joined' fired for {} (confirmed by heartbeat)", peer.hostname);
                             send_notification(&listener_handle, "Device Joined", &format!("{} has joined your cluster", peer.hostname), false, Some(1), "devices", NotificationPayload::None);
                         }
                     }
                 }

                 if peer.is_trusted || peer.is_manual {
                     kp_lock.insert(peer.id.clone(), peer.clone());
                     storage::save_known_peers(listener_handle.app_handle(), &kp_lock);
                 } else {
                     if kp_lock.contains_key(&peer.id) {
                         tracing::info!("Removing untrusted auto-peer {} from persistence.", peer.id);
                         kp_lock.remove(&peer.id);
                         storage::save_known_peers(listener_handle.app_handle(), &kp_lock);
                     }
                 }
            }

            if should_reply {
                tracing::debug!("Sending Discovery Reply to {}", addr);
                let local_id = listener_state.local_device_id.lock().unwrap().clone();
                let hostname = hostname::get().map(|h| h.to_string_lossy().to_string()).unwrap_or("Unknown".to_string());
                let network_name = listener_state.network_name.lock().unwrap().clone();

                let my_peer = crate::peer::Peer {
                    id: local_id,
                    ip: transport_inside.local_addr().unwrap().ip(),
                    port: transport_inside.local_addr().unwrap().port(),
                    hostname,
                    last_seen: 0,
                    is_trusted: false,
                    is_manual: true,
                    network_name: Some(network_name),
                    signature: None,
                    fingerprint: Some(transport_inside.local_fingerprint()),
                    protocol_version: Some(crate::discovery::CLUSTERCUT_PROTOCOL_VERSION.to_string()),
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
                        storage::save_known_peers(listener_handle.app_handle(), &kp);
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

        Message::FileRequest(req) => {
             // HANDLE FILE REQUEST (Sender). The connection is mTLS-pinned
             // to a paired peer, so we trust the request without an
             // app-layer auth token (issue #9 follow-up).
             tracing::info!("Received File Request from {}: ID={}, Index={}", addr, req.id, req.file_index);

             // 2a. Clipboard-blob serve (§3.3): if `req.id` matches a
             // registered large clipboard blob, serve it with
             // `delivery_target = Clipboard{…}` so the receiver lands
             // the bytes on its OS clipboard. The temp file lives in
             // `temp_downloads/<id>.<ext>` (cleaned by the existing
             // startup `clear_cache`).
             let clipboard_blob_meta = {
                 let map = listener_state.local_clipboard_blobs.lock().unwrap();
                 map.get(&req.id).cloned()
             };

             if let Some(meta) = clipboard_blob_meta {
                                      let file_path = meta.path.clone();
                                      let mime_type = meta.mime_type.clone();
                                      let width = meta.width;
                                      let height = meta.height;
                                      let req_id = req.id.clone();
                                      let req_file_index = req.file_index;
                                      tauri::async_runtime::spawn(async move {
                                          let mut file = match File::open(&file_path).await {
                                              Ok(f) => f,
                                              Err(e) => {
                                                  tracing::error!(
                                                      "Failed to open clipboard-blob temp file {:?}: {}",
                                                      file_path, e
                                                  );
                                                  return;
                                              }
                                          };
                                          let file_size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
                                          let file_name = file_path
                                              .file_name()
                                              .unwrap_or_default()
                                              .to_string_lossy()
                                              .to_string();
                                          tracing::info!(
                                              "Opening QUIC Stream to {} for clipboard-blob '{}' ({} bytes, mime={})",
                                              addr, file_name, file_size, mime_type
                                          );
                                          match transport_inside.send_file_stream(addr).await {
                                              Ok((_connection, mut stream)) => {
                                                  let header = crate::protocol::FileStreamHeader {
                                                      id: req_id,
                                                      file_index: req_file_index,
                                                      file_name,
                                                      file_size,
                                                      compressed: false, // never compress already-compressed image bytes
                                                      delivery_target: crate::protocol::DeliveryTarget::Clipboard {
                                                          mime_type,
                                                          width,
                                                          height,
                                                      },
                                                  };
                                                  if let Ok(h_json) = serde_json::to_string(&header) {
                                                      if let Err(e) = stream.write_all(h_json.as_bytes()).await {
                                                          tracing::error!("Header Write Error: {}", e);
                                                          return;
                                                      }
                                                      if let Err(e) = stream.write_all(b"\n").await {
                                                          tracing::error!("Header Newline Error: {}", e);
                                                          return;
                                                      }
                                                  }
                                                  let mut buf = vec![0u8; 1024 * 1024];
                                                  let start_time = std::time::Instant::now();
                                                  let mut chunks_sent = 0;
                                                  loop {
                                                      match file.read(&mut buf).await {
                                                          Ok(0) => break,
                                                          Ok(n) => {
                                                              if let Err(e) = stream.write_all(&buf[0..n]).await {
                                                                  tracing::error!("Clipboard-blob stream write error: {}", e);
                                                                  break;
                                                              }
                                                              chunks_sent += 1;
                                                          }
                                                          Err(e) => { tracing::error!("Clipboard-blob file read error: {}", e); break; }
                                                      }
                                                  }
                                                  let total_time = start_time.elapsed();
                                                  tracing::info!(
                                                      "[Sender] Clipboard-blob stream finished in {:?}. Chunks: {}",
                                                      total_time, chunks_sent
                                                  );
                                                  let _ = stream.finish();
                                                  drop(stream);
                                                  let _ = tokio::time::timeout(
                                                      std::time::Duration::from_secs(300),
                                                      _connection.closed(),
                                                  ).await;
                                                  tracing::info!("Clipboard-blob sent successfully: {:?}", file_path);
                                              }
                                              Err(e) => tracing::error!("Failed to open clipboard-blob stream: {}", e),
                                          }
                                      });
                                      return;
                                 }

                                 // 2b. Find File Path (existing files path)
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
                                      let compress_enabled = listener_state.settings.lock().unwrap().compress_file_transfers;
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
                                                   // Decide whether to compress this file (deterministic rules).
                                                   let compressed = compress_enabled
                                                       && crate::compression::should_compress(&file_name, file_size);

                                                   // Send Header (no auth_token; mTLS authenticates the sender).
                                                   let header = crate::protocol::FileStreamHeader {
                                                       id: req.id,
                                                       file_index: req.file_index,
                                                       file_name,
                                                       file_size,
                                                       compressed,
                                                       delivery_target: crate::protocol::DeliveryTarget::Disk,
                                                   };

                                                   if let Ok(h_json) = serde_json::to_string(&header) {
                                                       if let Err(e) = stream.write_all(h_json.as_bytes()).await { tracing::error!("Header Write Error: {}", e); return; }
                                                       if let Err(e) = stream.write_all(b"\n").await { tracing::error!("Header Newline Error: {}", e); return; }
                                                   }

                                                   // 5. Send File (raw or zstd-compressed depending on flag)
                                                   let mut buf = vec![0u8; 1024 * 1024]; // 1MB chunks
                                                   let mut chunks_sent = 0;
                                                   let start_time = std::time::Instant::now();

                                                   if compressed {
                                                       tracing::info!("[Sender] Starting ZSTD loop. File size: {}", file_size);
                                                       let mut encoder = async_compression::tokio::write::ZstdEncoder::with_quality(
                                                           stream,
                                                           async_compression::Level::Precise(crate::compression::ZSTD_LEVEL),
                                                       );
                                                       loop {
                                                           match file.read(&mut buf).await {
                                                               Ok(0) => break, // EOF
                                                               Ok(n) => {
                                                                   if let Err(e) = encoder.write_all(&buf[0..n]).await {
                                                                       tracing::error!("Compressed Stream Write Error: {}", e);
                                                                       break;
                                                                   }
                                                                   chunks_sent += 1;
                                                               }
                                                               Err(e) => { tracing::error!("File Read Error: {}", e); break; }
                                                           }
                                                       }
                                                       // Flush trailing zstd block before finishing the QUIC stream.
                                                       if let Err(e) = encoder.shutdown().await {
                                                           tracing::error!("Encoder Shutdown Error: {}", e);
                                                       }
                                                       let mut stream = encoder.into_inner();
                                                       let total_time = start_time.elapsed();
                                                       tracing::info!("[Sender] ZSTD loop finished in {:?}. Chunks: {}", total_time, chunks_sent);
                                                       let _ = stream.finish();
                                                       drop(stream);
                                                   } else {
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
                                                       // Finish Stream (signals no more data will be written)
                                                       let _ = stream.finish();
                                                       drop(stream);
                                                   }

                                                   // Wait for the connection to close naturally.
                                                   // After all data is delivered and ACKed, both sides go idle,
                                                   // and the 30s idle timeout closes the connection.
                                                   // This is critical over high-latency links (e.g. VPN) where
                                                   // QUIC needs time to retransmit/deliver buffered data.
                                                   let _ = tokio::time::timeout(
                                                       std::time::Duration::from_secs(300),
                                                       _connection.closed()
                                                   ).await;

                                                   tracing::info!("File Sent Successfully: {}", p_str);
                                               }

                                               Err(e) => tracing::error!("Failed to open file stream: {}", e),
                                           }
                                      });
                                 } else {
                                     tracing::warn!("Requested file not found (ID: {}, Index: {})", req.id, req.file_index);
                                 }
        }
        Message::Ping => {
            tracing::debug!("Received Ping from {}. Sending Pong.", addr);
            if let Ok(pong_data) = serde_json::to_vec(&Message::Pong) {
                let _ = transport_inside.send_message(addr, &pong_data).await;
            }
        }
        Message::ClusterInfoRequest => {
            // Post-pairing bootstrap reply (T6 → T7). The sender has already
            // passed our mTLS client-cert verifier (we just pinned its cert
            // in `handle_pairing_connection`), so the request is authenticated
            // and we can hand over our cluster state without further checks.
            let cluster_id = listener_state.cluster_id.lock().unwrap().clone();
            if cluster_id.is_empty() {
                tracing::warn!("ClusterInfoRequest from {} but we have no cluster_id", addr);
                return;
            }
            let known_peers_vec: Vec<_> = listener_state
                .known_peers
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect();
            let network_name = listener_state.network_name.lock().unwrap().clone();
            let info = crate::protocol::ClusterInfo {
                cluster_id,
                known_peers: known_peers_vec,
                network_name,
            };
            tracing::debug!("Replying to ClusterInfoRequest from {}", addr);
            match serde_json::to_vec(&Message::ClusterInfo(info)) {
                Ok(bytes) => {
                    if let Err(e) = transport_inside.send_message(addr, &bytes).await {
                        tracing::warn!("Failed to send ClusterInfo to {}: {}", addr, e);
                    }
                }
                Err(e) => tracing::error!("Failed to serialise ClusterInfo: {}", e),
            }
        }
        Message::ClusterInfo(info) => {
            // T7 reply to an in-progress `start_pairing`. mTLS already
            // authenticated the responder; we just hand off into the
            // pending oneshot. A stray ClusterInfo with no waiter is a
            // protocol-level no-op (logged + dropped).
            let waiter = listener_state.pending_cluster_info.lock().unwrap().take();
            match waiter {
                Some(tx) => {
                    let _ = tx.send(info);
                }
                None => {
                    tracing::warn!("Received unsolicited ClusterInfo from {}; ignoring", addr);
                }
            }
        }
        Message::Pong => {
             tracing::debug!("Received Pong from {}. Connection Verified.", addr);
             // Fire deferred join notification if the responding peer was pending
             let peer_id_opt = {
                 let peers = listener_state.peers.lock().unwrap();
                 peers.values().find(|p| p.ip == addr.ip() && p.port == addr.port()).map(|p| (p.id.clone(), p.hostname.clone()))
             };
             if let Some((peer_id, hostname)) = peer_id_opt {
                 let mut pending_joins = listener_state.pending_join_notifications.lock().unwrap();
                 if pending_joins.remove(&peer_id) {
                     if listener_state.should_notify()
                         && listener_state.settings.lock().unwrap().notifications.device_join
                     {
                         tracing::info!("[Notification] Deferred 'Device Joined' fired for {} (confirmed by Pong)", hostname);
                         send_notification(&listener_handle, "Device Joined", &format!("{} has joined your cluster", hostname), false, Some(1), "devices", NotificationPayload::None);
                     }
                 }
             }
        }
    }
}
