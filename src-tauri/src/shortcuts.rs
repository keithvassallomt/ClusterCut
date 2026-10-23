//! Global keyboard-shortcut registration and handling.

use std::str::FromStr;

use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::clipboard::common::{Broadcast, ClipboardContent};
use crate::protocol::ClipboardPayload;
use crate::state::AppState;
use crate::transport::Transport;
use crate::{clipboard, request_clipboard_blob_internal, send_notification, NotificationPayload};

pub(crate) fn register_shortcuts(app_handle: &tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();

    // Unregister all first to clear old ones
    if let Err(e) = app_handle.global_shortcut().unregister_all() {
        tracing::warn!("Failed to unregister shortcuts: {}", e);
    }

    // The plugin is X11-only; on Wayland the GlobalShortcuts portal binds them.
    #[cfg(target_os = "linux")]
    if clipboard::is_wayland() {
        crate::portal_shortcuts::spawn_sync(app_handle.clone());
        return;
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
    let matches = |s: &Option<String>| {
        s.as_deref()
            .and_then(|s| Shortcut::from_str(s).ok())
            .is_some_and(|parsed| parsed == *shortcut)
    };

    if matches(&settings.shortcut_send) {
        tracing::info!("Global Send Shortcut Triggered!");
        manual_send(app_handle);
    } else if matches(&settings.shortcut_receive) {
        tracing::info!("Global Receive Shortcut Triggered!");
        manual_receive(app_handle);
    }
}

/// Manual-mode Send: broadcast the clipboard to every peer in full (text, rich
/// text, images, files), as Auto-Send would. Shared by the global shortcut, the
/// D-Bus `ManualSend` method, the Wayland shortcut portal and the in-app button.
pub(crate) fn manual_send(app_handle: &tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let transport = app_handle.state::<Transport>();

    // With Auto-Send off, the monitor keeps the payload it built on the last
    // copy, with large content already staged for peers to fetch.
    if let Some(payload) = clipboard::common::pending_manual_send() {
        tracing::info!("Manual send of the last copy (id={}, {})", payload.id, payload_kind(&payload));
        clipboard::common::broadcast_clipboard(app_handle, &state, &transport, payload, Broadcast::Always);
        return;
    }

    // Nothing kept (copied before ClusterCut started, Auto-Send was on, or the
    // clipboard holds something ClusterCut wrote): send its plain text.
    match clipboard::read_text(app_handle) {
        Ok(text) if !text.trim().is_empty() => {
            tracing::info!("Manual send of the clipboard's plain text (no captured copy)");
            clipboard::common::process_clipboard_change_with(ClipboardContent::Text(text), app_handle, &state, &transport, Broadcast::Always);
        }
        result => {
            match result {
                Err(e) => tracing::info!("Nothing to send: {}", e),
                Ok(_) => tracing::info!("Nothing to send: clipboard text is only whitespace"),
            }
            // Otherwise the shortcut just appears to do nothing.
            send_notification(app_handle, "Manual Send", "Nothing to send. Copy something first.", false, Some(3), "history", NotificationPayload::None);
        }
    }
}

fn payload_kind(payload: &ClipboardPayload) -> &'static str {
    if payload.files.is_some() {
        "files"
    } else if let Some(blob) = &payload.blob {
        if blob.mime_type.starts_with("image/") { "image" } else { "large text" }
    } else if payload.formats.as_ref().is_some_and(|f| !f.is_empty()) {
        "rich text"
    } else {
        "text"
    }
}

/// Manual-mode Receive: apply the pending clipboard from a peer to the OS
/// clipboard. Shared by the global shortcut, the D-Bus `ManualReceive` method
/// and the Wayland shortcut portal.
pub(crate) fn manual_receive(app_handle: &tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let payload_opt = {
        let mut guard = state.pending_clipboard.lock().unwrap();
        guard.take()
    };
    let Some(payload) = payload_opt else {
        tracing::info!("No pending clipboard content to receive.");
        send_notification(app_handle, "Manual Receive", "No pending content.", false, Some(3), "history", NotificationPayload::None);
        return;
    };

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
}
