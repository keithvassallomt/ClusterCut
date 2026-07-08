mod app;
mod clipboard;
mod cluster_name;
mod commands;
mod compression;
mod diagnostics;
#[cfg(target_os = "linux")]
mod dbus;
mod handlers;
mod net_util;
mod pairing;
mod discovery;
mod netmon;
mod peer;
mod presence;
mod protocol;
mod shortcuts;
mod state;
mod storage;
mod transport;
mod tray;

use crate::protocol::Message;
use peer::Peer;
use state::AppState;
use storage::{
    establish_network_pin, load_network_name,
    save_cluster_id, save_device_id,
    save_network_name_origin, save_network_name_version,
    reset_network_state,
};
use tauri::Emitter;
// `Manager` (for `get_webview_window`) is only used in the macOS/Linux
// notification-callback paths; on Windows those are cfg'd out, so the import
// would be unused there.
#[cfg(not(target_os = "windows"))]
use tauri::Manager;

// Track last notification time for macOS cleaner
#[cfg(target_os = "macos")]
static LAST_NOTIFICATION_TIME: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

pub(crate) fn get_hostname_internal() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string())
}

// Helper to broadcast a new peer to all known peers (Gossip)
pub(crate) fn send_notification(app_handle: &tauri::AppHandle, title: &str, body: &str, increment_badge: bool, _id: Option<i32>, target_view: &str, _payload: NotificationPayload) {
    // 1. Windows (Native windows-rs with XML Actions)
    #[cfg(target_os = "windows")]
    {
        let _ = app_handle;
        let _ = increment_badge;
        use windows::UI::Notifications::{ToastNotificationManager, ToastNotification};
        use windows::Data::Xml::Dom::XmlDocument;
        use windows::core::HSTRING;
        use windows::core::Interface;

        let aumid = "app.clustercut.clustercut";

        // Since this is a generic notification (clipboard update, peer found, etc.),
        // we might not want specific buttons like "Download".
        // But for consistency and "click to show app", the basic XML structure is good.
        // We'll mimic the simpler notification but use 'activationType="protocol"' to wake app.

        // Dynamic Actions
        let mut actions_xml = format!(r#"<action content="Open" arguments="clustercut://action/show?view={}" activationType="protocol"/>"#, target_view);

        if let NotificationPayload::DownloadAvailable { msg_id, file_count, peer_id } = &_payload {
             // Encode params if needed, but for now simple format
             let download_args = format!("clustercut://action/download?msg_id={}&peer_id={}&file_count={}", msg_id, peer_id, file_count);
             // Escape XML chars in URL? & in XML is &amp;
             // Rust format! doesn't auto-escape for XML.
             // We need to escape '&' to '&amp;' in the URL when putting it into XML attribute.
             let download_args_escaped = download_args.replace("&", "&amp;");

             let download_action = format!(r#"<action content="Download" arguments="{}" activationType="protocol"/>"#, download_args_escaped);
             actions_xml.push_str(&download_action);
        }

        let xml = format!(r#"
<toast activationType="protocol" launch="clustercut://action/show?view={}">
    <visual>
        <binding template="ToastGeneric">
            <text>{}</text>
            <text>{}</text>
        </binding>
    </visual>
    <actions>
        {}
    </actions>
</toast>
"#, target_view, title, body, actions_xml);

        if let Ok(doc) = XmlDocument::new() {
             match doc.LoadXml(&HSTRING::from(&xml)) {
                 Ok(_) => match ToastNotification::CreateToastNotification(&doc) {
                     Ok(toast) => {
                         // Set Expiration Time. This is when Windows removes the
                         // notification from Action Center *and* the cutoff for
                         // when the banner can still display — if the OS hasn't
                         // gotten around to rendering the toast by this time
                         // (e.g. because a big file transfer is occupying the
                         // system), Windows silently drops it.
                         //
                         // The previous 5 s window was catastrophic: any
                         // notification that fired during peer setup or a file
                         // receive (the times users most want to be told) hit
                         // the queue late, expired, and dropped. 10 minutes is
                         // well past any queueing delay, short enough that
                         // Action Center stays tidy.
                         let now = std::time::SystemTime::now();
                         if let Ok(duration) = now.duration_since(std::time::UNIX_EPOCH) {
                             // Windows Epoch (1601-01-01) is 11,644,473,600 s before Unix Epoch.
                             // Ticks are 100ns intervals.
                             let unix_secs = duration.as_secs();
                             let unix_nanos = duration.subsec_nanos() as u64;
                             let windows_ticks = (unix_secs + 11_644_473_600) * 10_000_000
                                 + (unix_nanos / 100);
                             let expire_ticks = windows_ticks + (10 * 60 * 10_000_000);

                             let expiry = windows::Foundation::DateTime {
                                 UniversalTime: expire_ticks as i64,
                             };
                             if let Ok(inspectable) =
                                 windows::Foundation::PropertyValue::CreateDateTime(expiry)
                             {
                                 if let Ok(expiry_ref) = inspectable.cast::<windows::Foundation::IReference<windows::Foundation::DateTime>>() {
                                     if let Err(e) = toast.SetExpirationTime(&expiry_ref) {
                                         tracing::warn!("[Notification] SetExpirationTime failed: {}", e);
                                     }
                                 }
                             }
                         }

                         match ToastNotificationManager::CreateToastNotifierWithId(
                             &HSTRING::from(aumid),
                         ) {
                             Ok(notifier) => {
                                 if let Err(e) = notifier.Show(&toast) {
                                     // Surface the failure instead of swallowing
                                     // it — if AUMID isn't registered (dev mode
                                     // without the MSI), this is the place we'll
                                     // see it.
                                     tracing::warn!(
                                         "[Notification] ToastNotifier.Show failed: {}",
                                         e
                                     );
                                 }
                             }
                             Err(e) => {
                                 tracing::warn!(
                                     "[Notification] CreateToastNotifierWithId(\"{}\") failed: {} — AUMID likely not registered (only registered when installed via MSI)",
                                     aumid,
                                     e
                                 );
                             }
                         }
                     }
                     Err(e) => {
                         tracing::warn!(
                             "[Notification] CreateToastNotification failed: {}",
                             e
                         );
                     }
                 },
                 Err(e) => {
                     tracing::warn!("[Notification] LoadXml failed: {}", e);
                 }
             }
        }
    }

    // 2. macOS (user-notify)
    #[cfg(target_os = "macos")]
    {
        let _ = increment_badge;

        // Dev-mode fallback: when the binary isn't running inside an
        // `.app` bundle, user-notify silently swallows everything via its
        // `NotificationManagerMock` (no UNUserNotificationCenter access
        // outside a bundle). Detect that and shell out to `osascript`
        // instead — actions aren't supported but the user at least sees
        // the notification banner. The bundle-check log fires at startup
        // (see `init_logging`).
        let in_bundle = std::env::current_exe()
            .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
            .unwrap_or(false);
        if !in_bundle {
            tracing::info!("[Notification] macOS dev mode — falling back to osascript (no bundle).");
            let escape = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!(
                "display notification \"{}\" with title \"{}\"",
                escape(body),
                escape(title)
            );
            std::thread::spawn(move || {
                if let Err(e) = std::process::Command::new("osascript")
                    .args(["-e", &script])
                    .output()
                {
                    tracing::warn!("[Notification] osascript fallback failed: {}", e);
                }
            });
            return;
        }

        tracing::info!("[Notification] macOS detected. Using user-notify...");

        // Update last notification time
        {
            let mut lock = LAST_NOTIFICATION_TIME.lock().unwrap();
            *lock = Some(std::time::Instant::now());
        }

        let title = title.to_string();
        let body = body.to_string();
        let view = target_view.to_string();
        let app = app_handle.clone();

        static NOTIFICATION_MANAGER: std::sync::OnceLock<std::sync::Arc<dyn user_notify::NotificationManager>> = std::sync::OnceLock::new();

        let manager = NOTIFICATION_MANAGER.get_or_init(|| {
            tracing::info!("[Notification] Initializing Singleton Manager on MAIN THREAD...");
            let (tx, rx) = std::sync::mpsc::channel();
            let app_handle_main = app.clone();

            // Dispatch creation AND registration to Main Thread to satisfy SendWrapper thread affinity
            let _ = app.run_on_main_thread(move || {
                tracing::info!("[Notification] Creating manager on Main Thread...");
                let m = user_notify::get_notification_manager("app.clustercut.clustercut".to_string(), None);

                // Dispatch REGISTER immediately on Main Thread
                let app_handle_callback = app_handle_main.clone();
                match m.register(
                    Box::new(move |response| {
                        tracing::info!("Notification Response: {:?}" , response);
                        match response.action {
                            user_notify::NotificationResponseAction::Default => {
                                let _ = app_handle_callback.get_webview_window("main").map(|w: tauri::WebviewWindow| {
                                    tracing::info!("[Notification] Emitting 'notification-clicked' to main window...");
                                    // Extract view from payload
                                    let mut view = "history".to_string(); // Default
                                    if let Some(v) = response.user_info.get("view") {
                                        view = v.clone();
                                    }

                                    #[derive(serde::Serialize, Clone)]
                                    struct Payload {
                                        view: String,
                                    }

                                    let _ = w.emit("notification-clicked", Payload { view });
                                    let _ = w.unminimize();
                                    let _ = w.show();
                                    let _ = w.set_focus();
                                });
                            }
                            _ => {}
                        }
                    }),
                    vec![]
                ) {
                    Ok(_) => tracing::info!("[Notification] Callback registered successfully."),
                    Err(e) => tracing::error!("[Notification] Callback registration failed: {:?}" , e),
                }

                // Send the manager back to the implementation thread
                if let Err(e) = tx.send(m) {
                    tracing::error!("[Notification] Failed to send manager back: {:?}", e);
                }
            });

            // Block until Main Thread creates and registers the manager
            let manager = match rx.recv() {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!("[Notification] Failed to receive manager from Main Thread: {:?}", e);
                    // Fallback to avoid panic, though this state is critical
                     user_notify::get_notification_manager("app.clustercut.clustercut".to_string(), None)
                }
            };

            // Spawn Cleaner Thread (runs once per app lifecycle essentially, or per manager init)
            let m_cleaner = manager.clone();
            std::thread::spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                         tracing::error!("[Notification Cleaner] Failed to build runtime: {:?}", e);
                         return;
                    }
                };

                rt.block_on(async move {
                    tracing::info!("[Notification Cleaner] Started timed notification cleaner.");
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                        let should_clear = {
                            let lock = LAST_NOTIFICATION_TIME.lock().unwrap();
                            if let Some(last) = *lock {
                                last.elapsed().as_secs() > 5
                            } else {
                                false
                            }
                        };

                        if should_clear {
                             tracing::info!("[Notification Cleaner] Inactivity > 5s. Clearing notifications.");
                             // Reset timer first
                             {
                                 let mut lock = LAST_NOTIFICATION_TIME.lock().unwrap();
                                 *lock = None;
                             }

                             // Clear notifications
                             if let Err(e) = m_cleaner.remove_all_delivered_notifications() {
                                  tracing::error!("[Notification Cleaner] Failed to remove delivered notifications: {:?}", e);
                             } else {
                                  tracing::info!("[Notification Cleaner] Notifications cleared.");
                             }
                        }
                    }
                });
            });

            manager
        });

        let manager = manager.clone();

        // Spawn thread to SEND payload
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("[Notification] Failed to build runtime: {:?}", e);
                    return;
                }
            };

            rt.block_on(async move {
                use user_notify::NotificationBuilder;

                // Ask permission (idempotent-ish check)
                // We ask every time just to be sure we have it, or rely on cached state
                match manager.first_time_ask_for_notification_permission().await {
                     Ok(granted) => tracing::info!("[Notification] Permission status: {}", granted),
                     Err(e) => tracing::error!("[Notification] Permission check failed: {:?}", e),
                }

                tracing::info!("[Notification] Sending notification...");
                let mut notification = NotificationBuilder::new()
                    .title(&title)
                    .body(&body);

                // Add Context
                let mut map = std::collections::HashMap::new();
                map.insert("view".to_string(), view);
                notification = notification.set_user_info(map);

                match manager.send_notification(notification).await {
                    Ok(_) => tracing::info!("[Notification] Sent successfully via user-notify"),
                    Err(e) => tracing::error!("[Notification] Failed to send notification: {:?}", e),
                }
            });
        });
    }

    // 2. Linux Notifications
    #[cfg(target_os = "linux")]
    {
      if std::env::var("FLATPAK_ID").is_ok() {
        // Flatpak: use Notification portal (avoids --talk-name=org.freedesktop.Notifications)
        tracing::debug!("[Notification] Flatpak detected. Using Notification portal...");

        let title = title.to_string();
        let body = body.to_string();
        let payload = _payload.clone();
        let view = target_view.to_string();
        let app = app_handle.clone();

        let app_state_opt = app.try_state::<crate::state::AppState>();
        let state = if let Some(s) = app_state_opt {
             (*s).clone()
        } else {
             tracing::error!("Failed to get AppState for notification callback!");
             return;
        };

        tauri::async_runtime::spawn(async move {
            let conn = match zbus::Connection::session().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("[Notification] D-Bus session connection failed: {}", e);
                    return;
                }
            };

            let proxy: zbus::Proxy<'_> = match zbus::proxy::Builder::new(&conn)
                .interface("org.freedesktop.portal.Notification").unwrap()
                .path("/org/freedesktop/portal/desktop").unwrap()
                .destination("org.freedesktop.portal.Desktop").unwrap()
                .build()
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("[Notification] Failed to create Notification portal proxy: {}", e);
                    return;
                }
            };

            let notif_id = format!("n{}", uuid::Uuid::new_v4().as_simple());

            // Build notification dict
            let mut notif = std::collections::HashMap::<&str, zbus::zvariant::Value<'_>>::new();
            notif.insert("title", zbus::zvariant::Value::from(title.as_str()));
            notif.insert("body", zbus::zvariant::Value::from(body.as_str()));
            notif.insert("priority", zbus::zvariant::Value::from("normal"));
            notif.insert("default-action", zbus::zvariant::Value::from("open"));

            // Buttons
            let mut open_btn = std::collections::HashMap::<&str, zbus::zvariant::Value<'_>>::new();
            open_btn.insert("label", zbus::zvariant::Value::from("Open"));
            open_btn.insert("action", zbus::zvariant::Value::from("open"));

            let buttons: Vec<std::collections::HashMap<&str, zbus::zvariant::Value<'_>>> =
                if matches!(&payload, NotificationPayload::DownloadAvailable { .. }) {
                    let mut dl_btn = std::collections::HashMap::<&str, zbus::zvariant::Value<'_>>::new();
                    dl_btn.insert("label", zbus::zvariant::Value::from("Download"));
                    dl_btn.insert("action", zbus::zvariant::Value::from("download"));
                    vec![open_btn, dl_btn]
                } else if matches!(&payload, NotificationPayload::PromoteRichClipboard) {
                    let mut promote_btn = std::collections::HashMap::<&str, zbus::zvariant::Value<'_>>::new();
                    promote_btn.insert("label", zbus::zvariant::Value::from("Switch to Rich"));
                    promote_btn.insert("action", zbus::zvariant::Value::from("promote_rich"));
                    vec![open_btn, promote_btn]
                } else {
                    vec![open_btn]
                };

            notif.insert("buttons", zbus::zvariant::Value::from(buttons));

            // Subscribe to ActionInvoked before sending
            let mut action_stream: zbus::proxy::SignalStream<'_> = match proxy
                .receive_signal("ActionInvoked")
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("[Notification] Failed to subscribe to ActionInvoked: {}", e);
                    return;
                }
            };

            // Send notification
            if let Err(e) = proxy.call_method("AddNotification", &(notif_id.as_str(), notif)).await {
                tracing::error!("[Notification] AddNotification failed: {}", e);
                return;
            }
            tracing::info!("[Notification] Sent via portal: {}", notif_id);

            // Wait for action with timeout (loop to filter by our notification ID)
            use futures::StreamExt;
            let timeout_duration = std::time::Duration::from_secs(60);
            let start = std::time::Instant::now();

            loop {
                let remaining = timeout_duration.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    break;
                }

                match tokio::time::timeout(remaining, action_stream.next()).await {
                    Ok(Some(signal)) => {
                        let body: zbus::message::Body = signal.body();
                        match body.deserialize::<(String, String, Vec<zbus::zvariant::OwnedValue>)>() {
                            Ok((id, action, _params)) => {
                                if id != notif_id {
                                    continue; // Not our notification
                                }
                                tracing::info!("[Notification] Portal action '{}' for {}", action, id);

                                if action == "open" {
                                    let _ = app.emit("notification-clicked", serde_json::json!({ "view": &view }));
                                    let _ = app.get_webview_window("main").map(|w: tauri::WebviewWindow| {
                                        let _ = w.unminimize();
                                        let _ = w.show();
                                        let _ = w.set_focus();
                                    });
                                } else if action == "download" {
                                    if let NotificationPayload::DownloadAvailable { file_count, peer_id, .. } = &payload {
                                        let msg_id = if let NotificationPayload::DownloadAvailable { msg_id, .. } = &payload {
                                            msg_id.clone()
                                        } else {
                                            String::new()
                                        };

                                        let _ = app.emit("notification-clicked", serde_json::json!({ "view": "history" }));
                                        let _ = app.get_webview_window("main").map(|w: tauri::WebviewWindow| {
                                            let _ = w.unminimize();
                                            let _ = w.show();
                                            let _ = w.set_focus();
                                        });

                                        let state_clone = state.clone();
                                        let peer_id_clone = peer_id.clone();
                                        let count = *file_count;
                                        for i in 0..count {
                                            if let Err(e) = crate::request_file_internal(&state_clone, msg_id.clone(), i, peer_id_clone.clone()).await {
                                                tracing::error!("Failed to auto-download file {}/{}: {}", i, count, e);
                                            } else {
                                                tracing::info!("Successfully requested file {}/{}", i, count);
                                            }
                                        }
                                    }
                                } else if action == "promote_rich" {
                                    let promoted = {
                                        let mut slot = state.pending_rich_promotion.lock().unwrap();
                                        slot.take()
                                    };
                                    if let Some(payload_inner) = promoted {
                                        if let Some(formats) = payload_inner
                                            .formats
                                            .as_ref()
                                            .filter(|fs| !fs.is_empty())
                                            .cloned()
                                        {
                                            tracing::info!(
                                                "[Notification/portal] User clicked Switch to Rich. Promoting from {}: text={} chars, formats=[{}]",
                                                payload_inner.sender,
                                                payload_inner.text.len(),
                                                formats.iter().map(|f| f.mime_type.as_str()).collect::<Vec<_>>().join(", ")
                                            );
                                            clipboard::set_clipboard_rich(&app, payload_inner.text.clone(), formats);
                                            let _ = app.emit("clipboard-change", &payload_inner);
                                        }
                                    } else {
                                        tracing::debug!(
                                            "[Notification/portal] Switch to Rich clicked but nothing stashed"
                                        );
                                    }
                                }
                                break;
                            }
                            Err(e) => {
                                tracing::warn!("[Notification] Failed to deserialize ActionInvoked: {}", e);
                                continue;
                            }
                        }
                    }
                    _ => break, // Timeout or stream ended
                }
            }
        });
      } else {
        // Non-Flatpak: use notify-rust via D-Bus
        use notify_rust::Notification;
        tracing::debug!("[Notification] Linux detected. Using notify-rust via DBus...");

        let title = title.to_string();
        let body = body.to_string();
        // Move payload into the closure
        let payload = _payload.clone();
        let view = target_view.to_string();
        let app = app_handle.clone();

        let app_state_opt = app.try_state::<crate::state::AppState>();
        // We need state for request_file_internal, but try_state might fail?
        // Actually, AppHandle usually has state. But it returns Option in current tauri v2?
        // Wait, app.state() panics if missing. app.try_state() returns Option.
        // We'll capture the specific needed state (AppState) to avoid Send issues with AppHandle?
        // AppHandle is Send. AppState is Send (Arc<Mutex>).
        // We'll clone state here to be safe.
        // explicitly deref to clone the AppState, not the State wrapper (which has lifetime)
        let state = if let Some(s) = app_state_opt {
             (*s).clone()
        } else {
             tracing::error!("Failed to get AppState for notification callback!");
             return;
        };

        // Spawn to avoid blocking
        tauri::async_runtime::spawn(async move {
            let mut notification = Notification::new();
            notification
                .summary(&title)
                .body(&body)
                .appname("ClusterCut")
                .timeout(notify_rust::Timeout::Milliseconds(5000));

            // Ubuntu/Dock Badge Logic:
            if !increment_badge {
                notification.hint(notify_rust::Hint::Transient(true));
            } else {
                notification.hint(notify_rust::Hint::Transient(false));
            }

            // Actions
            notification.action("default", "Open");
            notification.action("open_btn", "Open");

            if let NotificationPayload::DownloadAvailable { .. } = &payload {
                 notification.action("download", "Download");
            }
            if matches!(&payload, NotificationPayload::PromoteRichClipboard) {
                notification.action("promote_rich", "Switch to Rich");
            }

            if let Ok(id) = std::env::var("FLATPAK_ID") {
                notification.hint(notify_rust::Hint::DesktopEntry(id));
            }

            let handle = match notification.show() {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("Failed to show notification: {}", e);
                    return;
                }
            };

            // Wait for action (Blocking call, hence spawn)
            handle.wait_for_action(move |action| {
                tracing::info!("Notification Action Clicked: {}", action);
                if action == "default" || action == "Open" || action == "open_btn" {
                    tracing::info!("Emitting 'notification-clicked' event");

                    #[derive(serde::Serialize, Clone)]
                    struct Payload {
                        view: String,
                    }

                    let _ = app.emit("notification-clicked", Payload { view: view.clone() });

                    let _ = app.get_webview_window("main").map(|w: tauri::WebviewWindow| {
                        let _ = w.unminimize();
                        let _ = w.show();
                        let _ = w.set_focus();
                    });
                } else if action == "download" || action == "Download" {
                     if let NotificationPayload::DownloadAvailable { msg_id: _, file_count, peer_id } = &payload {
                         tracing::info!("User clicked Download. Triggering download for {} files...", file_count);
                         // Trigger download for all files.
                         // Note: We need msg_id to look up the files locally in state.local_files map?
                         // Wait, request_file_internal takes (state, file_id, index, peer_id).
                         // The msg_id IS the file_id used for storage?
                         // In handle_incoming_file_stream/metadata logic:
                         // `files_lock.insert(msg_id.clone(), valid_paths.clone());`
                         // But `request_file` uses `file_id` which maps to `msg_id` in our context.

                         let msg_id = if let NotificationPayload::DownloadAvailable { msg_id, .. } = &payload { msg_id.clone() } else { String::new() };

                         let state_clone = state.clone();
                         let peer_id_clone = peer_id.clone();
                         let count = *file_count;

                         tauri::async_runtime::spawn(async move {
                             let _ = app.emit("notification-clicked", serde_json::json!({ "view": "history" }));
                             let _ = app.get_webview_window("main").map(|w: tauri::WebviewWindow| {
                                 let _ = w.unminimize();
                                 let _ = w.show();
                                 let _ = w.set_focus();
                             });

                             tracing::info!("Starting background download sequence override...");
                             for i in 0..count {
                                  if let Err(e) = crate::request_file_internal(&state_clone, msg_id.clone(), i, peer_id_clone.clone()).await {
                                      tracing::error!("Failed to auto-download file {}/{}: {}", i, count, e);
                                  } else {
                                      tracing::info!("Successfully requested file {}/{}", i, count);
                                  }
                             }
                         });
                     }
                } else if action == "promote_rich" || action == "Switch to Rich" {
                    // Pop the stashed Rich payload and overwrite the clipboard
                    // with it. The IGNORED guard + lenient `rich_eq_stable`
                    // suppresses the resulting truncated read-back so this
                    // doesn't echo back to the sender.
                    let promoted = {
                        let mut slot = state.pending_rich_promotion.lock().unwrap();
                        slot.take()
                    };
                    if let Some(payload_inner) = promoted {
                        if let Some(formats) = payload_inner
                            .formats
                            .as_ref()
                            .filter(|fs| !fs.is_empty())
                            .cloned()
                        {
                            tracing::info!(
                                "[Notification] User clicked Switch to Rich. Promoting from {}: text={} chars, formats=[{}]",
                                payload_inner.sender,
                                payload_inner.text.len(),
                                formats.iter().map(|f| f.mime_type.as_str()).collect::<Vec<_>>().join(", ")
                            );
                            clipboard::set_clipboard_rich(&app, payload_inner.text.clone(), formats);
                            let _ = app.emit("clipboard-change", &payload_inner);
                        }
                    } else {
                        tracing::debug!(
                            "[Notification] Switch to Rich clicked but nothing stashed (already promoted or superseded)"
                        );
                    }
                }
            });
        });
      }
    }


}

pub(crate) fn check_and_notify_leave(app_handle: &tauri::AppHandle, state: &AppState, peer: &Peer) {
    // Suppress leave notifications on startup too (though less likely to happen immediately)
    if !state.should_notify() {
        tracing::debug!("[Notification] Device leave notification suppressed by startup timer for peer: {}", peer.hostname);
        return;
    }

    let notifications = state.settings.lock().unwrap().notifications.clone();
    if notifications.device_leave {
        let local_net = state.network_name.lock().unwrap().clone();
        if let Some(remote_net) = &peer.network_name {
            if *remote_net == local_net {
                tracing::info!("[Notification] Device Left: {}", peer.hostname);
                send_notification(app_handle, "Device Left", &format!("{} has left the cluster", peer.hostname), false, Some(1), "devices", NotificationPayload::None);
            }
        }
    }
}

/// Log a send failure and, if the destination peer is on an incompatible
/// protocol version, emit a `peer-incompatible` event for the UI to surface
/// as a modal. Only fires for user-triggered sends (clipboard, file
/// requests) — background pings/heartbeats use plain logging instead, so a
/// transient unreachable peer doesn't spam the user.
pub fn report_send_failure(
    app: &tauri::AppHandle,
    peer_id: &str,
    peer_hostname: &str,
    peer_version: Option<&str>,
    addr: std::net::SocketAddr,
    err: &str,
) {
    tracing::error!("Failed to send to {} ({}, {}): {}", peer_hostname, peer_id, addr, err);
    if !net_util::is_protocol_compatible(peer_version) {
        let _ = app.emit(
            "peer-incompatible",
            serde_json::json!({
                "id": peer_id,
                "hostname": peer_hostname,
            }),
        );
    }
}

// Helper to wipe state and restart network identity
pub(crate) fn perform_factory_reset(app_handle: &tauri::AppHandle, state: &AppState, port: u16) {
    // 1. Reset Config on Disk
    reset_network_state(app_handle);

    // Fresh identity, fresh cluster: tombstones from the previous cluster
    // life must not suppress membership sync in the next one.
    state.removed_peer_tombstones.lock().unwrap().clear();

    // 2. Update Runtime State
    {
        let mut kp = state.known_peers.lock().unwrap();
        let mut peers = state.peers.lock().unwrap();
        let mut cid = state.cluster_id.lock().unwrap();
        let mut nn = state.network_name.lock().unwrap();
        let mut np = state.network_pin.lock().unwrap();

        kp.clear();
        // Mark peers untrusted
        for p in peers.values_mut() {
            p.is_trusted = false;
        }

        // Generate a fresh cluster_id (UUID) — new cluster, new handle.
        let new_cluster_id = uuid::Uuid::new_v4().to_string();
        *cid = new_cluster_id.clone();
        save_cluster_id(app_handle, &new_cluster_id);

        // Load new identity (generated by accessors if missing)
        let new_name_val = load_network_name(app_handle);
        // Factory reset returns the cluster to auto mode (set below), so the PIN
        // is ephemeral — not persisted (issue 4).
        let new_pin_val = establish_network_pin(app_handle, "auto");

        *nn = new_name_val.clone();
        *np = new_pin_val.clone();

        tracing::info!(
            "Reset to New Network: {} (PIN: <redacted>, cluster {})",
            new_name_val,
            new_cluster_id
        );
    }

    // Reset cluster mode to auto
    {
        let mut settings = state.settings.lock().unwrap();
        settings.cluster_mode = "auto".to_string();
        crate::storage::save_settings(app_handle, &settings);
    }

    // 2b. Regenerate this device's ID so peers see a genuinely NEW mDNS service
    // instance. The mDNS instance name is the device_id; when a leaving device
    // re-registers the *same* instance with only its TXT cluster-name changed,
    // browsers that already have it cached do NOT re-emit ServiceResolved (and
    // the goodbye for a same-instance re-register doesn't reliably reach/clear
    // their cache), so the leaver's new cluster stayed invisible to
    // already-running peers until they restarted (confirmed via device logs: a
    // directly-paired peer received no mDNS event at all for the re-register). A
    // fresh device_id is a brand-new instance every browser resolves cleanly.
    // Peers were already told to drop the old id via PeerRemoval, and the TLS
    // cert/fingerprint is unchanged, so this is a clean re-introduction.
    {
        let new_device_id = format!("clustercut-{}", uuid::Uuid::new_v4());
        save_device_id(app_handle, &new_device_id);
        *state.local_device_id.lock().unwrap() = new_device_id.clone();
        tracing::info!("Regenerated device ID on reset: {}", new_device_id);

        // This is a brand-new cluster with a freshly-generated name, so reset
        // the cluster-name convergence register too: origin = this (new) device,
        // version = 0. Otherwise it kept the OLD origin (a now-dead device id)
        // and version from the previous cluster, which could skew name
        // convergence once the fresh cluster gains peers.
        *state.network_name_version.lock().unwrap() = 0;
        *state.network_name_origin.lock().unwrap() = new_device_id.clone();
        save_network_name_version(app_handle, 0);
        save_network_name_origin(app_handle, &new_device_id);
    }

    // 3. Re-register mDNS (under the new device_id → new instance name).
    {
        let local_id = state.local_device_id.lock().unwrap().clone();
        let new_name = state.network_name.lock().unwrap().clone();
        if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
             let _ = discovery.register(&local_id, &new_name, port);
        }
    }

    // 4. Notify Frontend
    let _ = app_handle.emit("network-reset", ());
}

/// Send a `FileRequest` to the descriptor's source peer to begin fetching a
/// large clipboard blob. Used by the manual-confirm path (§3.3 Tier B2 and
/// auto-receive=off Tier B1).
pub async fn request_clipboard_blob_internal(
    state: &AppState,
    msg_id: String,
    peer_id: String,
) -> Result<(), String> {
    request_file_internal(state, msg_id, 0, peer_id).await
}

pub async fn request_file_internal(
    state: &AppState,
    file_id: String,
    file_index: usize,
    peer_id: String,
) -> Result<(), String> {
    tracing::info!("File Request Internal: ID={}, Index={}, Peer={}", file_id, file_index, peer_id);

    // 1. Find Peer Address
    let addr = {
        let peers = state.get_peers();
        if let Some(p) = peers.get(&peer_id) {
            std::net::SocketAddr::new(p.ip, p.port)
        } else {
             return Err(format!("Peer {} not found or offline", peer_id));
        }
    };

    // 2. Get Transport
    let transport = {
        let t_lock = state.transport.lock().unwrap();
        t_lock.clone().ok_or("Transport not initialized".to_string())?
    };

    // 3. Send Request (mTLS provides confidentiality + sender auth).
    let req_payload = crate::protocol::FileRequestPayload {
        id: file_id,
        file_index,
        offset: 0,
    };
    let msg = Message::FileRequest(req_payload);
    let data = serde_json::to_vec(&msg).map_err(|e| e.to_string())?;
    transport.send_message(addr, &data).await.map_err(|e| e.to_string())?;
    tracing::info!("File Request sent to {}", addr);
    Ok(())
}

#[derive(Clone, Debug)]
pub enum NotificationPayload {
    None,
    DownloadAvailable { msg_id: String, file_count: usize, peer_id: String },
    /// GNOME-only: a Rich payload landed plain-text on the OS clipboard and the
    /// full rich payload is stashed in `pending_rich_promotion`. The system
    /// notification carries a "Switch to Rich" action that invokes
    /// `promote_pending_rich` so the user can upgrade without opening the app.
    PromoteRichClipboard,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    app::run();
}
