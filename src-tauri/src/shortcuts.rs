//! Global keyboard-shortcut registration and handling.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::clipboard::change_events;
use crate::clipboard::common::{Broadcast, ClipboardContent};
use crate::keys::{self, Action, KeysError};
use crate::protocol::ClipboardPayload;
use crate::state::AppState;
use crate::storage::AppSettings;
use crate::transport::Transport;
use crate::{clipboard, request_clipboard_blob_internal, send_notification, NotificationPayload};

/// The manual-mode shortcuts. Issue #21 added the one-step ones, which press
/// Copy, Cut or Paste in the focused app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManualAction {
    CopySend,
    CutSend,
    SendClipboard,
    ReceivePaste,
    ReceiveOnly,
}

impl ManualAction {
    pub(crate) const ALL: [Self; 5] = [Self::CopySend, Self::CutSend, Self::SendClipboard, Self::ReceivePaste, Self::ReceiveOnly];

    /// Stable ID: KDE and GNOME remember the key per app ID + shortcut ID, and
    /// Hyprland binds reference it as `app.clustercut.clustercut:<id>`.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::CopySend => "copy-send",
            Self::CutSend => "cut-send",
            Self::SendClipboard => "send-clipboard",
            Self::ReceivePaste => "receive-paste",
            Self::ReceiveOnly => "receive-only",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    /// Shown in the desktop's shortcut settings.
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::CopySend => "Copy and send to cluster",
            Self::CutSend => "Cut and send to cluster",
            Self::SendClipboard => "Send clipboard to cluster",
            Self::ReceivePaste => "Paste received clipboard",
            Self::ReceiveOnly => "Apply received clipboard",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::CopySend => "Copy & Send",
            Self::CutSend => "Cut & Send",
            Self::SendClipboard => "Send Clipboard",
            Self::ReceivePaste => "Receive & Paste",
            Self::ReceiveOnly => "Receive Only",
        }
    }

    /// A shortcut only acts while its direction is in manual mode.
    pub(crate) fn is_manual(self, settings: &AppSettings) -> bool {
        match self {
            Self::CopySend | Self::CutSend | Self::SendClipboard => !settings.auto_send,
            Self::ReceivePaste | Self::ReceiveOnly => !settings.auto_receive,
        }
    }

    pub(crate) fn accelerator(self, settings: &AppSettings) -> Option<&str> {
        match self {
            Self::CopySend => settings.shortcut_copy_send.as_deref(),
            Self::CutSend => settings.shortcut_cut_send.as_deref(),
            Self::SendClipboard => settings.shortcut_send.as_deref(),
            Self::ReceivePaste => settings.shortcut_receive_paste.as_deref(),
            Self::ReceiveOnly => settings.shortcut_receive.as_deref(),
        }
    }

    /// Blocks while the one-step actions wait on the clipboard: run it off the
    /// main thread and the async executor.
    pub(crate) fn run(self, app_handle: &tauri::AppHandle) {
        match self {
            Self::CopySend => copy_send(app_handle, Action::Copy, self.title()),
            Self::CutSend => copy_send(app_handle, Action::Cut, self.title()),
            Self::SendClipboard => manual_send(app_handle),
            Self::ReceivePaste => receive_paste(app_handle, self.title()),
            Self::ReceiveOnly => manual_receive(app_handle),
        }
    }
}

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

    for action in ManualAction::ALL {
        let Some(s) = action.accelerator(&settings).filter(|_| action.is_manual(&settings)) else {
            continue;
        };
        match Shortcut::from_str(s) {
            Ok(shortcut) => {
                if let Err(e) = app_handle.global_shortcut().register(shortcut) {
                    tracing::error!("Failed to register {} shortcut '{}': {}", action.title(), s, e);
                } else {
                    tracing::debug!("Registered {} shortcut: {}", action.title(), s);
                }
            }
            Err(e) => tracing::error!("Invalid {} shortcut '{}': {}", action.title(), s, e),
        }
    }
}

pub(crate) fn handle_shortcut(app_handle: &tauri::AppHandle, shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state == ShortcutState::Released {
        return;
    }
    let state = app_handle.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    let action = ManualAction::ALL.into_iter().find(|action| {
        action.is_manual(&settings)
            && action
                .accelerator(&settings)
                .and_then(|s| Shortcut::from_str(s).ok())
                .is_some_and(|parsed| parsed == *shortcut)
    });
    if let Some(action) = action {
        tracing::info!("Global {} shortcut triggered", action.title());
        let app = app_handle.clone();
        tauri::async_runtime::spawn_blocking(move || action.run(&app));
    }
}

/// How long an app gets to put its selection on the clipboard after Copy.
const COPY_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long the clipboard monitor gets to read and stage a copy (an image takes a moment).
const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
/// How long received content gets to land on the clipboard before Paste.
const APPLY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Large content is fetched from the sender first; past this, the user pastes it themselves.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// Keep My Clipboard: how long the app gets to read the pasted content before
/// the old clipboard goes back. Pastes read asynchronously; too soon and the
/// app pastes the old content.
const RESTORE_DELAY: Duration = Duration::from_millis(500);

/// One key-pressing action at a time: a second press while the first still
/// waits on the clipboard would press keys into the middle of it.
static PRESSING: AtomicBool = AtomicBool::new(false);

struct Pressing;

impl Pressing {
    fn start() -> Option<Self> {
        PRESSING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok().map(|_| Pressing)
    }
}

impl Drop for Pressing {
    fn drop(&mut self) {
        PRESSING.store(false, Ordering::Release);
    }
}

/// The clipboard as it was, for Keep My Clipboard to put back.
struct KeptClipboard {
    content: ClipboardContent,
    /// The kept manual-send copy that goes with it.
    pending: Option<ClipboardPayload>,
}

/// `None` unless Keep My Clipboard is on and there's something to put back.
fn keep_clipboard(app_handle: &tauri::AppHandle) -> Option<KeptClipboard> {
    if !app_handle.state::<AppState>().settings.lock().unwrap().keep_local_clipboard {
        return None;
    }
    let content = clipboard::common::last_seen_content()?;
    Some(KeptClipboard { content, pending: clipboard::common::pending_manual_send() })
}

fn restore_clipboard(app_handle: &tauri::AppHandle, kept: KeptClipboard) {
    tracing::info!("Keep My Clipboard: putting back {}", clipboard::common::describe_content(&kept.content));
    let before = change_events::change_count();
    clipboard::common::restore_content(app_handle, kept.content);
    // The write's echo guard drops the kept copy (on its own thread), so put it
    // back once the write has landed.
    if let Some(changed) = change_events::wait_for_change_after(before, APPLY_TIMEOUT) {
        change_events::wait_until_processed(changed, PROCESS_TIMEOUT);
    }
    clipboard::common::set_pending_manual_send(kept.pending);
}

/// Copy & Send and Cut & Send: press Copy (or Cut) in the focused app, then
/// send what it put on the clipboard. Sends nothing if the clipboard doesn't
/// change, rather than an older copy.
fn copy_send(app_handle: &tauri::AppHandle, action: Action, title: &str) {
    let Some(_pressing) = Pressing::start() else {
        tracing::info!("{} ignored: the previous one is still running", title);
        return;
    };
    if let Err(e) = keys::check() {
        report_keys_error(app_handle, title, e);
        return;
    }
    let kept = keep_clipboard(app_handle);
    let before = change_events::change_count();
    if let Err(e) = keys::perform(app_handle, action) {
        report_keys_error(app_handle, title, e);
        return;
    }
    let Some(changed) = change_events::wait_for_change_after(before, COPY_TIMEOUT) else {
        tracing::info!("{}: the clipboard didn't change", title);
        send_notification(app_handle, title, "Nothing copied. Select something first.", false, Some(3), "history", NotificationPayload::None);
        return;
    };
    // In manual mode the monitor keeps the copy's full payload for manual_send.
    if !change_events::wait_until_processed(changed, PROCESS_TIMEOUT) {
        tracing::warn!("{}: the clipboard monitor hasn't read the copy yet; sending what it has", title);
    }
    let auto_send = app_handle.state::<AppState>().settings.lock().unwrap().auto_send;
    if !auto_send {
        manual_send(app_handle);
    }
    if let Some(kept) = kept {
        restore_clipboard(app_handle, kept);
    }
}

/// Receive & Paste: apply the pending content from a peer, then press Paste in
/// the focused app.
fn receive_paste(app_handle: &tauri::AppHandle, title: &str) {
    let Some(_pressing) = Pressing::start() else {
        tracing::info!("{} ignored: the previous one is still running", title);
        return;
    };
    if let Err(e) = keys::check() {
        report_keys_error(app_handle, title, e);
        return;
    }
    let kept = keep_clipboard(app_handle);
    let before = change_events::change_count();
    let Some(fetching) = receive_pending(app_handle, title) else {
        return;
    };
    if change_events::wait_for_change_after(before, if fetching { FETCH_TIMEOUT } else { APPLY_TIMEOUT }).is_none() {
        // Nothing to put back yet: the content lands later and the user pastes it.
        if fetching {
            tracing::info!("{}: still fetching; not pasting", title);
            send_notification(app_handle, title, "Still receiving. Paste it once it arrives.", false, Some(3), "history", NotificationPayload::None);
        } else {
            tracing::warn!("{}: the received content didn't reach the clipboard; not pasting", title);
        }
        return;
    }
    if let Err(e) = keys::perform(app_handle, Action::Paste) {
        report_keys_error(app_handle, title, e);
    }
    if let Some(kept) = kept {
        std::thread::sleep(RESTORE_DELAY);
        restore_clipboard(app_handle, kept);
    }
}

fn report_keys_error(app_handle: &tauri::AppHandle, title: &str, e: KeysError) {
    tracing::warn!("{}: couldn't press keys: {}", title, e);
    let body = match e {
        KeysError::Unsupported => "Can't press keys on this desktop yet. Use Send Clipboard or Receive Only instead.",
        KeysError::Declined => "ClusterCut needs your permission to press keys. Allow it when your desktop asks.",
        KeysError::UpdateExtension => "Needs version 4.2 or newer of the ClusterCut GNOME extension.",
        KeysError::NeedsAccessibility => "Allow ClusterCut in System Settings > Privacy & Security > Accessibility, then try again.",
        KeysError::Failed(_) => "Couldn't press the keys. The log has details.",
    };
    send_notification(app_handle, title, body, false, Some(4), "history", NotificationPayload::None);
}

/// Send Clipboard: broadcast the clipboard to every peer in full (text, rich
/// text, images, files), as Auto-Send would. Shared by the shortcut, the D-Bus
/// `ManualSend` method, the Wayland shortcut portal, the in-app button and
/// Copy & Send.
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

/// Receive Only: apply the pending clipboard from a peer to the OS clipboard.
/// Shared by the shortcut, the D-Bus `ManualReceive` method and the Wayland
/// shortcut portal.
pub(crate) fn manual_receive(app_handle: &tauri::AppHandle) {
    match receive_pending(app_handle, "Manual Receive") {
        Some(true) => send_notification(app_handle, "Receiving Clipboard", "Fetching pending content…", false, Some(2), "history", NotificationPayload::None),
        Some(false) => send_notification(app_handle, "Clipboard Received", "Pending content applied.", false, Some(2), "history", NotificationPayload::None),
        None => {}
    }
}

/// Take the pending content from a peer and put it on the OS clipboard, as
/// Auto-Receive would. Large content arrives as a descriptor: its fetch is
/// started here and the stream handler applies the bytes when they land.
/// Returns whether a fetch was started, or `None` (after saying so) when
/// nothing is pending.
fn receive_pending(app_handle: &tauri::AppHandle, title: &str) -> Option<bool> {
    let state = app_handle.state::<AppState>();
    let payload_opt = state.pending_clipboard.lock().unwrap().take();
    let Some(payload) = payload_opt else {
        tracing::info!("No pending clipboard content to receive.");
        send_notification(app_handle, title, "No pending content.", false, Some(3), "history", NotificationPayload::None);
        return None;
    };

    // §3.3 descriptor: trigger an async fetch instead of
    // pushing empty bytes onto the OS clipboard.
    if payload.blob.as_ref().is_some_and(|b| b.is_descriptor()) {
        let state_clone = (*state).clone();
        let id = payload.id.clone();
        let peer_id = payload.sender_id.clone();
        *state_clone.in_flight_clipboard_fetch.lock().unwrap() = Some(id.clone());
        tauri::async_runtime::spawn(async move {
            if let Err(e) = request_clipboard_blob_internal(&state_clone, id, peer_id).await {
                tracing::error!("Failed to fetch pending clipboard content: {}", e);
            }
        });
        return Some(true);
    }

    clipboard::common::apply_received(app_handle, &state, &payload);
    tracing::info!("Applied pending clipboard content ({})", payload_kind(&payload));
    Some(false)
}
