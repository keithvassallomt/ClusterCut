mod clipboard;
mod commands;
mod compression;
#[cfg(target_os = "linux")]
mod dbus;
mod handlers;
mod net_util;
mod pairing;
mod discovery;
mod netmon;
mod peer;
mod protocol;
mod shortcuts;
mod state;
mod storage;
mod transport;
mod tray;

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
#[cfg(target_os = "linux")]
use tauri::Listener;
use crate::protocol::Message;


#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, default_value = "info")]
    log_level: String,
    
    #[arg(short, long, default_value_t = false)]
    debug: bool,

    #[arg(long, default_value_t = false)]
    minimized: bool,

    #[arg(long)]
    theme: Option<String>,
}

fn init_logging() -> Args {
    // 1. Parse CLI Args (ignoring unknown args that Tauri might use)
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(_) => {
            // Keep default if parsing fails (e.g. extra args)
            Args { log_level: "info".to_string(), debug: false, minimized: false, theme: None }
        }
    };

    if let Some(theme) = &args.theme {
        std::env::set_var("CLUSTERCUT_THEME", theme);
    }

    let level = if args.debug {
        tracing::Level::DEBUG
    } else {
        match args.log_level.to_lowercase().as_str() {
            "error" => tracing::Level::ERROR,
            "warn" => tracing::Level::WARN,
            "info" => tracing::Level::INFO,
            "debug" => tracing::Level::DEBUG,
            "trace" => tracing::Level::TRACE,
            _ => tracing::Level::INFO,
        }
    };

    // 2. Setup Stdout Layer (Colored)
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false) // Don't show target (module path) for cleaner output? Or maybe show it.
        .with_ansi(true)
        .compact(); // Compact format

    // 3. Setup File Layer (Rolling Daily)
    // We need a path. Since we are before AppHandle, we can't easily get AppDataDir.
    // We'll trust XDG or standard paths or just current dir for now?
    // User requested "sinks".
    // Better to use `tauri::api::path::app_log_dir`? No, we don't have app handle yet.
    // Let's us `directories` crate? Or just `.logs` in CWD for development as requested?
    // "We need each log line to be timestamped, and include hostname."
    
    // Use temp_dir for logs to ensure we can write even if CWD is / (macOS Bundle)
    let log_dir = std::env::temp_dir().join("ClusterCutLogs");
    let file_appender = tracing_appender::rolling::daily(&log_dir, "clustercut.log");
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_appender)
        .with_ansi(false)
        .with_target(true);

    // 4. Init Registry
    // Base Level: INFO (for external crates) + User Level for US
    let filter_level = if args.debug {
        "debug"
    } else {
        &args.log_level.to_lowercase()
    };
    
    let filter = tracing_subscriber::EnvFilter::new("info")
        .add_directive(format!("tauri_app={}", filter_level).parse().unwrap())
        .add_directive(format!("clustercut_lib={}", filter_level).parse().unwrap())
        // Silence noisy networking crates
        .add_directive("rustls=warn".parse().unwrap())
        .add_directive("quinn=warn".parse().unwrap())
        .add_directive("zbus=warn".parse().unwrap()); 

    tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();
        
    tracing::info!("Logging initialized. Level: {}, Hostname: {}", level, get_hostname_internal());

    if let Some(theme) = &args.theme {
        tracing::info!("Theme Override Active: {}", theme);
    }

    if cfg!(target_os = "macos") {
        if let Ok(exe) = std::env::current_exe() {
            let path_str = exe.to_string_lossy();
            tracing::info!("[Bundle Check] Executable Path: {}", path_str);
            if path_str.contains(".app/Contents/MacOS/") {
                tracing::info!("[Bundle Check] Running inside an App Bundle. Native Notifications SHOULD work.");
            } else {
                tracing::warn!("[Bundle Check] Running as raw binary. Notifications will likely use Mock.");
            }
        } else {
             tracing::error!("[Bundle Check] Failed to get current executable path.");
        }
    }
    
    args
}

pub(crate) fn get_hostname_internal() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string())
}
use discovery::Discovery;
use peer::Peer;
use rand::Rng;
use state::{AppState, LegacyPeerInfo};
use storage::{
    load_cluster_id, load_device_id, load_known_peers, load_network_name, load_network_pin,
    save_cluster_id, save_device_id, save_known_peers,
    wipe_legacy_cluster_key,
    reset_network_state, load_settings,
};
use tauri::{Emitter, Manager};
use transport::Transport;
// use tauri_plugin_notification::NotificationExt;

// Track last notification time for macOS cleaner
#[cfg(target_os = "macos")]
static LAST_NOTIFICATION_TIME: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

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

const MAX_REMOVAL_RETRIES: u32 = 3;

async fn removal_debounce_task(
    state: AppState,
    handle: tauri::AppHandle,
    peer_id: String,
    nonce: u64,
    retry_count: u32,
) {
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;

    let should_probe = {
        let pending = state.pending_removals.lock().unwrap();
        pending.get(&peer_id).map_or(false, |n| *n == nonce)
    };

    if !should_probe {
        tracing::debug!("[Discovery] Removal debounce cancelled (nonce changed) for {}", peer_id);
        return;
    }

    // Layer 2: If network is down, retry instead of falsely confirming removal
    if !state.network_available.load(std::sync::atomic::Ordering::Relaxed) {
        if retry_count < MAX_REMOVAL_RETRIES {
            tracing::info!("[Discovery] Network down — re-queuing removal for {} (retry {}/{})", peer_id, retry_count + 1, MAX_REMOVAL_RETRIES);
            let new_nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_micros() as u64;
            {
                let mut pending = state.pending_removals.lock().unwrap();
                pending.insert(peer_id.clone(), new_nonce);
            }
            Box::pin(removal_debounce_task(state, handle, peer_id, new_nonce, retry_count + 1)).await;
            return;
        }
        // Max retries reached — remove silently (can't verify either way)
        tracing::warn!("[Discovery] Max retries reached for {} with network down — removing silently", peer_id);
        let mut pending = state.pending_removals.lock().unwrap();
        pending.remove(&peer_id);
        drop(pending);
        let mut peers = state.peers.lock().unwrap();
        peers.remove(&peer_id);
        drop(peers);
        let _ = handle.emit("peer-remove", &peer_id);
        return;
    }

    // Network is up — perform active probe
    let peer_addr = {
        let peers = state.peers.lock().unwrap();
        peers.get(&peer_id).map(|p| std::net::SocketAddr::new(p.ip, p.port))
    };

    let mut is_alive = false;

    if let Some(addr) = peer_addr {
        tracing::info!("[Discovery] Debounce expired for {}. Probing...", peer_id);
        let transport_opt = { state.transport.lock().unwrap().clone() };

        if let Some(transport) = transport_opt {
            if let Ok(ping_data) = serde_json::to_vec(&Message::Ping) {
                let send_fut = async {
                    match transport.send_message(addr, &ping_data).await {
                        Ok(_) => true,
                        Err(e) => {
                            tracing::warn!("[Discovery] Active probe to {} failed: {}", addr, e);
                            false
                        }
                    }
                };
                if let Ok(result) = tokio::time::timeout(std::time::Duration::from_secs(2), send_fut).await {
                    is_alive = result;
                } else {
                    tracing::warn!("[Discovery] Active probe to {} timed out.", addr);
                }
            }
        }
    }

    // Finalize
    let mut pending = state.pending_removals.lock().unwrap();
    if let Some(current_n) = pending.get(&peer_id) {
        if *current_n == nonce {
            pending.remove(&peer_id);
            drop(pending);

            if is_alive {
                tracing::info!("[Discovery] Active probe SUCCESS for {}. Cancelling removal.", peer_id);
                return;
            }

            tracing::info!("[Discovery] Active probe FAILED/TIMEOUT. Removing peer {}", peer_id);
            let mut peers = state.peers.lock().unwrap();
            if let Some(peer) = peers.remove(&peer_id) {
                drop(peers);
                check_and_notify_leave(&handle, &state, &peer);
            }
            let _ = handle.emit("peer-remove", &peer_id);
        } else {
            tracing::debug!("[Discovery] Removal debounce cancelled (nonce updated during probe) for {}", peer_id);
        }
    } else {
        tracing::debug!("[Discovery] Removal debounce cancelled (entry removed during probe) for {}", peer_id);
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
        let new_pin_val = load_network_pin(app_handle);

        *nn = new_name_val.clone();
        *np = new_pin_val.clone();

        tracing::info!(
            "Reset to New Network: {} (PIN: {}, cluster {})",
            new_name_val,
            new_pin_val,
            new_cluster_id
        );
    }

    // Reset cluster mode to auto
    {
        let mut settings = state.settings.lock().unwrap();
        settings.cluster_mode = "auto".to_string();
        crate::storage::save_settings(app_handle, &settings);
    }

    // 3. Re-register mDNS
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

#[cfg(target_os = "linux")]
fn spawn_linux_theme_watcher(app: tauri::AppHandle) {
    use futures::StreamExt;
    use zbus::zvariant::{OwnedValue, Value};

    /// Recursively unwrap D-Bus variants to extract a u32.
    /// The portal returns color-scheme as v(v(u32)).
    fn extract_u32(v: &Value<'_>) -> Option<u32> {
        match v {
            Value::U32(n) => Some(*n),
            Value::Value(inner) => extract_u32(inner),
            _ => None,
        }
    }

    /// Convert a portal color-scheme value to a theme string.
    /// 0 = No preference, 1 = Prefer dark, 2 = Prefer light.
    fn color_scheme_to_theme(value: OwnedValue) -> &'static str {
        let v: Value<'static> = value.into();
        match extract_u32(&v) {
            Some(1) => "prefer-dark",
            _ => "default",
        }
    }

    let app_handle = app.clone();

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        tracing::info!("Starting Linux Theme Watcher (XDG Settings Portal)...");

        let conn = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to connect to session bus for theme watching: {}", e);
                return;
            }
        };

        let proxy: zbus::Proxy<'_> = match zbus::proxy::Builder::new(&conn)
            .interface("org.freedesktop.portal.Settings").unwrap()
            .path("/org/freedesktop/portal/desktop").unwrap()
            .destination("org.freedesktop.portal.Desktop").unwrap()
            .build()
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Failed to create Settings portal proxy: {}", e);
                return;
            }
        };

        // Read initial color-scheme value
        match proxy.call_method("Read", &("org.freedesktop.appearance", "color-scheme")).await {
            Ok(reply) => {
                let body: zbus::message::Body = reply.body();
                match body.deserialize::<(OwnedValue,)>() {
                    Ok((value,)) => {
                        let theme = color_scheme_to_theme(value);
                        apply_theme_change(&app_handle, theme);
                    },
                    Err(e) => tracing::warn!("Failed to deserialize color-scheme: {}", e),
                }
            },
            Err(e) => tracing::warn!("Failed to read initial color-scheme from portal: {}", e),
        }

        // Subscribe to SettingChanged signal for real-time updates
        let mut stream: zbus::proxy::SignalStream<'_> = match proxy.receive_signal("SettingChanged").await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to subscribe to SettingChanged signal: {}", e);
                return;
            }
        };

        while let Some(signal) = stream.next().await {
            let body: zbus::message::Body = signal.body();
            if let Ok((namespace, key, value)) = body.deserialize::<(String, String, OwnedValue)>() {
                if namespace == "org.freedesktop.appearance" && key == "color-scheme" {
                    let theme = color_scheme_to_theme(value);
                    apply_theme_change(&app_handle, theme);
                }
            }
        }
    });
}

#[cfg(target_os = "linux")]
fn apply_theme_change(app_handle: &tauri::AppHandle, theme: &str) {
    tracing::info!("Linux theme change detected: {}", theme);

    if let Some(state) = app_handle.try_state::<AppState>() {
        *state.current_theme.lock().unwrap() = Some(theme.to_string());
    }

    crate::tray::update_tray_icon(app_handle);

    let simple_theme = if theme == "prefer-dark" { "dark" } else { "light" };
    let _ = app_handle.emit("tauri://theme-changed", simple_theme);
}


#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    
    // Initialize Logging and get Args
    let args = init_logging();
    let minimized_arg = args.minimized;
    
    // Detect clipboard backend on Linux before building
    #[cfg(target_os = "linux")]
    let _clipboard_backend = clipboard::detect_backend();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init());

    // Only init clipboard plugin when needed (X11, or non-Linux)
    #[cfg(not(target_os = "linux"))]
    {
        builder = builder.plugin(tauri_plugin_clipboard::init());
    }
    #[cfg(target_os = "linux")]
    {
        if clipboard::should_init_plugin() {
            builder = builder.plugin(tauri_plugin_clipboard::init());
        }
    }
        
    #[cfg(not(target_os = "linux"))]
    {
        builder = builder.plugin(tauri_plugin_deep_link::init());
    }

    builder
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // Handle deep link activation from Toast
            let _ = app.emit("deep-link", args);
            // Always bring to front on activation
             if let Some(win) = app.get_webview_window("main") {
                 let _ = win.unminimize();
                 let _ = win.show();
                 let _ = win.set_focus();
             }
        }))
        // Pass --minimized to autostart args
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().with_handler(shortcuts::handle_shortcut).build())
        .manage(AppState::new())
        .setup(move |app| {
            #[cfg(not(target_os = "linux"))]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                // Explicitly register the scheme to avoid config parsing issues
                if let Err(e) = app.deep_link().register("clustercut") {
                     tracing::warn!("Failed to register deep link scheme 'clustercut': {}", e);
                }
            }

            // Handle Minimized Startup
            if let Some(window) = app.get_webview_window("main") {
                // Workaround: Always show the window to force WM to apply constraints
                tracing::info!("Startup: Force showing window to prime size calculations.");
                let _ = window.show();
                let _ = window.set_focus();

                if minimized_arg {
                    tracing::info!("Starting in minimized mode. Hiding window after brief delay.");
                    let win = window.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        let _ = win.hide();
                    });
                } else {
                    tracing::info!("Starting in normal mode.");
                }
            }

            // Clear Cache on Startup
            clear_cache(app.handle());

            // Load (or generate and persist) the device's TLS cert. Stable across
            // restarts so peers can pin our fingerprint (see issue #9).
            let (cert_der, key_der) = match storage::load_device_cert(app.handle()) {
                Some((c, k)) => (c, k),
                None => {
                    let (c, k) = transport::generate_self_signed_cert()
                        .expect("Failed to generate device cert");
                    storage::save_device_cert(app.handle(), &c, &k);
                    tracing::info!("Generated new device TLS cert and persisted to disk.");
                    (c, k)
                }
            };

            // Initialize QUIC Transport (Fixed Port 4654 for Discovery, or random fallback)
            let transport = tauri::async_runtime::block_on(async {
                match Transport::new(4654, cert_der.clone(), key_der.clone()) {
                    Ok(t) => Ok(t),
                    Err(e) => {
                        tracing::warn!("Failed to bind port 4654 ({}). Falling back to random port.", e);
                        Transport::new(0, cert_der, key_der)
                    }
                }
            }).expect("Failed to create transport");


            let port = transport.local_addr().expect("Failed to get port").port();
            tracing::info!("QUIC Transport listening on port {}", port);

            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            #[cfg(target_os = "linux")]
            {
                // Workaround for Flatpak/GTK unresponsive titlebar buttons.
                // Toggling resizable property on focus seems to "wake up" the window manager.
                if let Some(window) = app.get_webview_window("main") {
                    let win_clone = window.clone();
                    window.listen("tauri://focus", move |_event| {
                         // Fix: Explicitly unmaximize in case the WM forced it
                         if let Ok(true) = win_clone.is_maximized() {
                             let _ = win_clone.unmaximize();
                         }

                         // We want the window to be non-resizable (to hide maximize button).
                         // So we toggle True -> False. This forces a frame update on some WMs.
                         let _ = win_clone.set_resizable(true);
                         let _ = win_clone.set_resizable(false);
                    });
                }
            }

            let app_handle = app.handle();
            
            #[cfg(desktop)]
            {
                let _ = crate::tray::create_tray(&app_handle);
            }

            #[cfg(target_os = "linux")]
            {
                let dbus_handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                     if let Err(e) = crate::dbus::start_dbus_server(dbus_handle).await {
                         tracing::error!("Failed to start D-Bus service: {}", e);
                     }
                });
            }

            #[cfg(target_os = "windows")]
            {
                // Ensure firewall rule exists; checks first and only prompts UAC if needed.
                std::thread::spawn(|| {
                    net_util::configure_windows_firewall();
                });
            }

            #[cfg(target_os = "linux")]
            {
                spawn_linux_theme_watcher(app_handle.clone());
            }

            // Start network state monitor (cross-platform)
            {
                let netmon_handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    crate::netmon::start_network_monitor(netmon_handle).await;
                });
            }

            // Load State
            {
                let state = app.state::<AppState>();

                // 1. Load (or generate) cluster_id, and wipe the legacy
                //    cluster_key.bin secret if it's still on disk from v0.2.
                wipe_legacy_cluster_key(app_handle);
                let mut cid_lock = state.cluster_id.lock().unwrap();
                if let Some(id) = load_cluster_id(app_handle) {
                    *cid_lock = id;
                } else {
                    let new_id = uuid::Uuid::new_v4().to_string();
                    tracing::info!("No cluster_id found. Generated {}.", new_id);
                    save_cluster_id(app_handle, &new_id);
                    *cid_lock = new_id;
                }

                // 2. Load Known Peers, then sweep for legacy entries that
                //    pre-date mTLS — peers without a stored fingerprint can
                //    no longer talk to us under the v0.3 strict-pinning model
                //    and need to be re-paired. Their hostnames are stashed
                //    in `state.legacy_peers` so the UI can surface a banner.
                {
                    let mut kp_lock = state.known_peers.lock().unwrap();
                    *kp_lock = load_known_peers(app_handle);

                    let mut legacy = Vec::new();
                    for (id, peer) in kp_lock.iter_mut() {
                        if peer.fingerprint.is_none() {
                            peer.is_trusted = false;
                            legacy.push(LegacyPeerInfo {
                                id: id.clone(),
                                hostname: peer.hostname.clone(),
                            });
                        }
                    }
                    if !legacy.is_empty() {
                        tracing::warn!(
                            "Detected {} pre-mTLS peer(s) needing re-pair: {:?}",
                            legacy.len(),
                            legacy.iter().map(|p| &p.hostname).collect::<Vec<_>>()
                        );
                        *state.legacy_peers.lock().unwrap() = legacy;
                    }
                }


                // 4. Load Settings
                let mut settings_lock = state.settings.lock().unwrap();
                *settings_lock = load_settings(app_handle);
                drop(settings_lock); // Unlock to allow registration to access it if needed (though register_shortcuts locks it again)
                
                // Register Shortcuts on Startup
                shortcuts::register_shortcuts(app_handle);
                let mut device_id = load_device_id(app_handle);
                if device_id.is_empty() {
                    let run_id: u32 = rand::thread_rng().gen();
                    device_id = format!("clustercut-{}", run_id);
                    save_device_id(app_handle, &device_id);
                    tracing::info!("Generated new Device ID: {}", device_id);
                } else {
                    tracing::info!("Loaded Device ID: {}", device_id);
                }
                *state.local_device_id.lock().unwrap() = device_id.clone();
                
                // 3b. Load Network Name (for mDNS)
                let network_name = load_network_name(app_handle);
                *state.network_name.lock().unwrap() = network_name.clone();

                // 3c. Load Network PIN
                let network_pin = load_network_pin(app_handle);
                *state.network_pin.lock().unwrap() = network_pin.clone();
                tracing::info!("Network PIN: {}", network_pin);

                // 3e. Load Settings
                let settings = load_settings(app_handle);
                *state.settings.lock().unwrap() = settings;
                tracing::info!("Loaded Settings");

                // --- NEW: Startup Reconnection Probe ---
                // We want to try reconnecting to manual peers or trusted peers.
                let state_owned = (*state).clone();
                let transport_clone = transport.clone();
                let app_handle_clone = app_handle.clone();
                
                tauri::async_runtime::spawn(async move {
                     // Wait a moment for transport/discovery to settle
                     tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                     
                     // Retroactive Fix: If a peer is on a different subnet, mark it as manual.
                     let mut known_peers = state_owned.known_peers.lock().unwrap();
                     let local_ip_obj = local_ip_address::local_ip().unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
                     let mut changed = false;
                     
                     for peer in known_peers.values_mut() {
                         if !peer.is_manual {
                              // If peer.ip is remote relative to local_ip_obj
                              let is_remote = match (local_ip_obj, peer.ip) {
                                 (std::net::IpAddr::V4(l), std::net::IpAddr::V4(r)) => {
                                     // Compare first 3 octets
                                     l.octets()[0..3] != r.octets()[0..3]
                                 },
                                 (std::net::IpAddr::V6(l), std::net::IpAddr::V6(r)) => {
                                      // Compare first 4 segments
                                      l.segments()[0..4] != r.segments()[0..4]
                                 },
                                 _ => true,
                             };
                             
                             if is_remote && !peer.ip.is_loopback() {
                                 tracing::info!("Startup: Auto-correcting peer {} to is_manual=true (Remote IP: {})", peer.id, peer.ip);
                                 peer.is_manual = true;
                                 changed = true;
                             }
                         }
                     }
                     if changed {
                         save_known_peers(&app_handle_clone, &known_peers);
                     }
                     
                     // Clone to vector for iteration (drop lock)
                     let peers_to_probe: Vec<(String, Peer)> = known_peers.clone().into_iter().collect();
                     drop(known_peers);

                     if !peers_to_probe.is_empty() {
                         tracing::info!("Startup: Probing {} known peers for reconnection...", peers_to_probe.len());
                         for (id, peer) in peers_to_probe {
                             tracing::info!("Startup: Peer {} (Manual: {}) - {}", id, peer.is_manual, peer.ip);
                             
                             let s = state_owned.clone();
                             let t = transport_clone.clone();
                             let a = app_handle_clone.clone();
                             
                             tauri::async_runtime::spawn(async move {
                                 // We use the last known IP/Port
                                 net_util::probe_ip(peer.ip, peer.port, s, t, a).await;
                             });
                         }
                     }
                });

                // 4. Register Discovery
                let mut discovery = Discovery::new().expect("Failed to initialize discovery");
                discovery
                    .register(&device_id, &network_name, port)
                    .expect("Failed to register service");
                let receiver = discovery.browse().expect("Failed to browse");
                *state.discovery.lock().unwrap() = Some(discovery);

                // Spawn Discovery Loop
                let d_handle = app_handle.clone();
                let d_state = (*state).clone();

                tauri::async_runtime::spawn(async move {
                    while let Ok(event) = receiver.recv_async().await {
                        match event {
                            mdns_sd::ServiceEvent::ServiceResolved(info) => {
                                if let Some(ip) = info.get_addresses().iter().next() {
                                    let id = info
                                        .get_property_val_str("id")
                                        .unwrap_or("unknown")
                                        .to_string();

                                    let local_id =
                                        { d_state.local_device_id.lock().unwrap().clone() };
                                    if id == local_id {
                                        continue;
                                    }

                                    // DEBOUNCE: Cancel any pending removal for this peer
                                    {
                                        let mut pending = d_state.pending_removals.lock().unwrap();
                                        if pending.remove(&id).is_some() {
                                            tracing::debug!("[Discovery] Debounce: Cancelled pending removal for reappearing peer {}", id);
                                        }
                                    }

                                    let network_name_prop = info
                                        .get_property_val_str("n")
                                        .map(|s| s.to_string());
                                    let proto_prop = info
                                        .get_property_val_str("proto")
                                        .map(|s| s.to_string());
                                    
                                    if let Some(n) = &network_name_prop {
                                        tracing::debug!("Discovered peer {} with network name: {}", id, n);
                                    } else {
                                        tracing::warn!("Discovered peer {} WITHOUT network name (properties: {:?})", id, info.get_properties());
                                    }

                                    // Lock known_peers to prevent race with PairRequest.
                                    // Trust requires a stored fingerprint under the v0.3
                                    // strict-mTLS model — a known-but-unfingerprinted entry
                                    // is a legacy peer and must re-pair.
                                    let kp = d_state.known_peers.lock().unwrap();
                                    let known_entry = kp.get(&id).cloned();
                                    let is_trusted = known_entry
                                        .as_ref()
                                        .map(|p| p.fingerprint.is_some())
                                        .unwrap_or(false);
                                    let stored_fingerprint = known_entry
                                        .and_then(|p| p.fingerprint);

                                    // Extract hostname from property or fallback to mDNS hostname
                                    let h_prop = info.get_property_val_str("h");
                                    let hostname_prop = h_prop
                                        .or_else(|| info.get_property_val_str("hostname"))
                                        .map(|s| s.to_string())
                                        .unwrap_or_else(|| info.get_hostname().to_string());

                                    tracing::info!("[Discovery] Peer {} resolved. 'h' prop: {:?}, Final hostname: {}", id, h_prop, hostname_prop);

                                    let peer = Peer {
                                        id: id.clone(),
                                        ip: ip.to_string().parse().unwrap_or(std::net::IpAddr::V4(
                                            std::net::Ipv4Addr::new(127, 0, 0, 1),
                                        )),
                                        port: info.get_port(),
                                        hostname: hostname_prop,
                                        last_seen: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs(),
                                        is_trusted,
                                        is_manual: false, // Discovered via mDNS
                                        network_name: network_name_prop,
                                        signature: None,
                                        // Carry the stored fingerprint forward into the
                                        // runtime peer record so it shows up in the UI.
                                        fingerprint: stored_fingerprint,
                                        protocol_version: proto_prop,
                                    };

                                    // Check if peer is already active to prevent duplicate notifications
                                    let is_new_peer = {
                                        let peers = d_state.peers.lock().unwrap();
                                        !peers.contains_key(&id)
                                    };

                                    d_state.add_peer(peer.clone());
                                    let _ = d_handle.emit("peer-update", &peer);

                                    // Trigger Notification (with Layer 2 ping verification)
                                    {
                                        let same_network = {
                                            let local_net = d_state.network_name.lock().unwrap();
                                            peer.network_name.as_ref().map_or(false, |rn| *rn == *local_net)
                                        };

                                        if same_network && is_new_peer
                                            && d_state.settings.lock().unwrap().notifications.device_join
                                            && d_state.should_notify()
                                        {
                                            // Ping-verify before notifying
                                            let verify_state = d_state.clone();
                                            let verify_handle = d_handle.clone();
                                            let verify_peer = peer.clone();
                                            tauri::async_runtime::spawn(async move {
                                                let addr = std::net::SocketAddr::new(verify_peer.ip, verify_peer.port);
                                                let transport_opt = { verify_state.transport.lock().unwrap().clone() };
                                                let mut verified = false;

                                                if let Some(transport) = transport_opt {
                                                    if let Ok(ping_data) = serde_json::to_vec(&Message::Ping) {
                                                        if let Ok(Ok(_)) = tokio::time::timeout(
                                                            std::time::Duration::from_secs(3),
                                                            transport.send_message(addr, &ping_data),
                                                        ).await {
                                                            verified = true;
                                                        }
                                                    }
                                                }

                                                if verified {
                                                    tracing::info!("[Notification] Ping-verified 'Device Joined' for: {}", verify_peer.hostname);
                                                    send_notification(&verify_handle, "Device Joined", &format!("{} has joined your cluster", verify_peer.hostname), false, Some(1), "devices", NotificationPayload::None);
                                                } else {
                                                    tracing::info!("[Notification] Deferring 'Device Joined' for {} (ping failed, will fire on heartbeat)", verify_peer.hostname);
                                                    verify_state.pending_join_notifications.lock().unwrap().insert(verify_peer.id.clone());
                                                }
                                            });
                                        }
                                    }
                                }

                            }
                            mdns_sd::ServiceEvent::ServiceRemoved(_ty, fullname) => {
                                let id =
                                    fullname.split('.').next().unwrap_or("unknown").to_string();
                                tracing::info!("[Discovery] Service Removed: {} -> ID: {}", fullname, id);
                                
                                // Safety Check: If we effectively just saw this peer (in the last 2 seconds),
                                // ignore this removal as a "phantom" or out-of-order packet.
                                // This happens often when devices re-announce themselves.
                                {
                                    let peers = d_state.peers.lock().unwrap();
                                    if let Some(peer) = peers.get(&id) {
                                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                                        if now.saturating_sub(peer.last_seen) < 2 {
                                             tracing::warn!("[Discovery] Ignoring ServiceRemoved for {} (seen {}s ago) - likely phantom.", id, now.saturating_sub(peer.last_seen));
                                             return;
                                        }
                                    }
                                }

                                // DEBOUNCE: Don't remove immediately. Wait 8 seconds.
                                let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_micros() as u64;
                                {
                                    let mut pending = d_state.pending_removals.lock().unwrap();
                                    pending.insert(id.clone(), nonce);
                                }
                                
                                let r_state = d_state.clone();
                                let r_handle = d_handle.clone();
                                let r_id = id.clone();
                                
                                tauri::async_runtime::spawn(async move {
                                    removal_debounce_task(r_state, r_handle, r_id, nonce, 0).await;
                                });
                            }
                            _ => {}
                        }
                    }
                });
            }

            // Clones for transport listener
            let listener_handle = app.handle().clone();
            let listener_state = (*app.state::<AppState>()).clone();

            // Wire both fingerprint resolvers now that known_peers is loaded.
            //   - fingerprint_resolver: client side, address → expected fp.
            //   - known_fingerprints_resolver: server side, fp → "is paired?".
            // Pairing itself runs over plain TCP and bypasses both, so the
            // chicken-and-egg of "first contact" is handled separately.
            {
                let resolver_state = listener_state.clone();
                transport.set_fingerprint_resolver(std::sync::Arc::new(move |addr| {
                    resolver_state.fingerprint_for(addr)
                }));
            }
            {
                let resolver_state = listener_state.clone();
                transport.set_known_fingerprints_resolver(std::sync::Arc::new(move |fp| {
                    resolver_state.knows_fingerprint(fp)
                }));
            }

            {
                let mut t_lock = listener_state.transport.lock().unwrap();
                *t_lock = Some(transport.clone());
            }

            app.manage(transport.clone());

            // Start the dedicated plaintext-TCP pairing listener on the same
            // numeric port as the QUIC endpoint (UDP/QUIC and TCP/pairing
            // cohabit on a single port number).
            //
            // The accept closure enforces three WIRE-PROTOCOL-0.3.1 hardening
            // requirements before spawning the handler:
            //   §H1 — refuse outright while the responder is locked out
            //         (drop the TCP socket immediately).
            //   §H6 — cap = 1 concurrent pairing; refuse anything else with
            //         no permit available.
            //   §H6 — wrap the handler in `PAIRING_PROTOCOL_TIMEOUT` so an
            //         accepted-but-idle socket can't hold the single-flight
            //         slot indefinitely.
            {
                let pairing_state = listener_state.clone();
                let pairing_handle = listener_handle.clone();
                let pairing_transport = transport.clone();
                if let Err(e) = crate::transport::start_pairing_listener(port, move |stream, peer_addr| {
                    let state = pairing_state.clone();
                    let app = pairing_handle.clone();
                    let t = pairing_transport.clone();
                    if !state.settings.lock().unwrap().pairing_accept_enabled {
                        tracing::debug!(
                            "Pairing TCP accept from {} dropped: pairing paused by user (issue #16).",
                            peer_addr
                        );
                        drop(stream);
                        return;
                    }
                    if state.is_pairing_locked_out() {
                        tracing::warn!(
                            "Pairing TCP accept from {} refused: listener locked out (§H1).",
                            peer_addr
                        );
                        drop(stream);
                        return;
                    }
                    let permit = match state.pairing_slot.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => {
                            tracing::warn!(
                                "Pairing TCP accept from {} refused: another pairing in flight (cap = 1, §H6).",
                                peer_addr
                            );
                            drop(stream);
                            return;
                        }
                    };
                    tauri::async_runtime::spawn(async move {
                        let _permit = permit; // released on drop
                        match tokio::time::timeout(
                            crate::transport::PAIRING_PROTOCOL_TIMEOUT,
                            pairing::handle_pairing_connection(stream, peer_addr, state, app, t),
                        )
                        .await
                        {
                            Ok(()) => {}
                            Err(_elapsed) => {
                                tracing::warn!(
                                    "Pairing from {} aborted: exceeded {:?} idle timeout (§H6).",
                                    peer_addr,
                                    crate::transport::PAIRING_PROTOCOL_TIMEOUT,
                                );
                            }
                        }
                    });
                }) {
                    tracing::error!("Failed to bind pairing TCP listener on port {}: {}", port, e);
                }
            }

            // Start Listening
            // Start Listening
            let transport_inside = transport.clone();
            let file_state = listener_state.clone();
            let file_handle = listener_handle.clone();

            transport.start_listening(
                move |data, addr| {
                    tracing::trace!("Received {} bytes from {}", data.len(), addr);
                    let listener_handle = listener_handle.clone();
                    let listener_state = listener_state.clone();
                    let transport_inside = transport_inside.clone();

                    // ... Existing Message Handler Code ...
                    tauri::async_runtime::spawn(async move {
                         match serde_json::from_slice::<Message>(&data) {
                             Ok(msg) => handlers::handle_message(msg, addr, listener_state, listener_handle, transport_inside).await,
                             Err(e) => tracing::error!("Failed to parse message: {}", e), 
                         }
                    });
                },
                move |recv, addr| {
                    tracing::info!("Received FILE stream from {}", addr);
                    let state = file_state.clone();
                    let handle = file_handle.clone();
                    
                    tauri::async_runtime::spawn(async move {
                         handlers::handle_incoming_file_stream(recv, addr, state, handle).await;
                    });
                }
            );
            // Start Clipboard Monitor
            let transport_for_clipboard = transport.clone();
            let state_for_clipboard = (*app.state::<AppState>()).clone();

            clipboard::start_monitor(
                app.handle().clone(),
                state_for_clipboard,
                transport_for_clipboard,
            );

            // Background Task: Heartbeat (Keep Manual Peers Alive)

            let hb_state = (*app.state::<AppState>()).clone();
            let hb_transport = transport.clone();
            let hb_handle = app.handle().clone();

            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                    let peers: Vec<Peer> = {
                        // FIX: Heartbeat ALL runtime peers, not just known (connected) ones.
                        // This prevents pruning of discovered-but-not-yet-trusted peers.
                        let peers_map = hb_state.get_peers();
                        peers_map.values().cloned().collect()
                    };

                    if peers.is_empty() { continue; }

                    let local_id = hb_state.local_device_id.lock().unwrap().clone();
                    let hostname = hostname::get().map(|h| h.to_string_lossy().to_string()).unwrap_or("Unknown".to_string());
                    let network_name = hb_state.network_name.lock().unwrap().clone();

                    // Self Peer (for payload)
                    let my_peer = Peer {
                        id: local_id,
                        ip: hb_transport.local_addr().unwrap().ip(),
                        port: hb_transport.local_addr().unwrap().port(),
                        hostname,
                        last_seen: 0,
                        is_trusted: false,
                        is_manual: true,
                        network_name: Some(network_name),
                        signature: None,
                        fingerprint: Some(hb_transport.local_fingerprint()),
                        protocol_version: Some(crate::discovery::CLUSTERCUT_PROTOCOL_VERSION.to_string()),
                    };

                    let msg = Message::PeerDiscovery(my_peer);
                    let data = serde_json::to_vec(&msg).unwrap_or_default();

                    let mut any_success = false;
                    for p in &peers {
                        let addr = std::net::SocketAddr::new(p.ip, p.port);
                        if hb_transport.send_message(addr, &data).await.is_ok() {
                            any_success = true;
                        }
                    }

                    // Heartbeat fallback: track consecutive rounds where all sends failed
                    if any_success {
                        let prev = hb_state.consecutive_heartbeat_failures.swap(0, std::sync::atomic::Ordering::Relaxed);
                        if prev >= 3 {
                            tracing::info!("[Netmon] Heartbeat fallback: network recovered (was down for {} rounds)", prev);
                            crate::netmon::on_network_up(&hb_state);
                            crate::netmon::start_recovery_tasks(&hb_handle);
                        }
                    } else {
                        let failures = hb_state.consecutive_heartbeat_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if failures == 3 {
                            tracing::warn!("[Netmon] Heartbeat fallback: 3 consecutive failed rounds — marking network as down");
                            crate::netmon::on_network_down(&hb_state);
                        }
                    }
                }
            });

            // Background Task: Pruning (Remove Stale Untrusted Peers)
            let prune_handle = app.handle().clone();
            let prune_state = (*app.state::<AppState>()).clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                    let timeout = 300; // 300 seconds (5 minutes) timeout to allow for network hiccups

                    // Fix Deadlock: Acquire known_peers FIRST, then peers.
                    // This matches perform_factory_reset and PeerDiscovery.
                    let mut kp_lock = prune_state.known_peers.lock().unwrap();
                    let mut peers_lock = prune_state.peers.lock().unwrap();

                    let mut to_remove = Vec::new();
                    
                    // Iterate over peers to find stale ones
                    for (id, p) in peers_lock.iter() {
                        if now - p.last_seen > timeout {
                            tracing::info!("Pruning stale peer: {} ({}) - Last seen {}s ago", p.hostname, id, now - p.last_seen);
                            to_remove.push(p.clone());
                        }
                    }

                    if !to_remove.is_empty() {
                         for peer in to_remove {
                             let id = peer.id.clone();
                             let was_trusted = peer.is_trusted;

                             // Always remove from RUNTIME peers (UI)
                             peers_lock.remove(&id);

                             // If Untrusted, forget them completely.
                             // If Trusted, KEEP them in known_peers (Reverse Discovery)
                             if !was_trusted {
                                 kp_lock.remove(&id);
                             }
                             
                             check_and_notify_leave(&prune_handle, &prune_state, &peer);
                             let _ = prune_handle.emit("peer-remove", &id);
                         }
                         save_known_peers(prune_handle.app_handle(), &kp_lock);
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::peers::get_local_ip,
            commands::peers::get_peers,

            commands::peers::add_manual_peer,
            pairing::start_pairing,
            commands::peers::delete_peer,
            commands::peers::leave_network,
            commands::identity::get_network_name,
            commands::clipboard::request_file,
            commands::clipboard::delete_history_item,
            commands::system::check_gnome_extension_status,
            commands::identity::get_network_pin,
            commands::identity::get_device_id,
            commands::identity::get_hostname,
            commands::settings::get_settings,
            commands::peers::get_known_peers,
            commands::peers::expects_remote_manual_peers,
            commands::system::log_frontend,
            commands::settings::save_settings,
            commands::identity::set_network_identity,
            commands::identity::regenerate_network_identity,
            commands::clipboard::send_clipboard,
            commands::clipboard::set_local_clipboard,
            commands::clipboard::set_local_clipboard_files,
            commands::clipboard::confirm_pending_clipboard,
            commands::clipboard::promote_pending_rich,
            commands::system::get_launch_args,
            commands::system::exit_app,
            commands::peers::retry_connection,
            commands::system::configure_autostart,
            commands::system::get_autostart_state,
            commands::peers::get_listening_port,
            commands::system::show_native_notification,
            commands::theme::get_theme_override,
            commands::theme::get_current_theme,
            commands::peers::get_legacy_peers,
            commands::peers::dismiss_legacy_peer_banner,
            pairing::is_pairing_locked_out,
            pairing::rearm_pairing,
            pairing::get_pairing_accept,
            pairing::set_pairing_accept,
        ])

        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                     // Minimize to Tray behavior
                     let _ = window.hide();
                     api.prevent_close();
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle: &tauri::AppHandle, event: tauri::RunEvent| {
        match event {
            tauri::RunEvent::WindowEvent { event: tauri::WindowEvent::Focused(true), .. } => {
                // Clear badge on focus
                #[cfg(target_os = "linux")]
                {
                    // Linux does not have a standard way to clear badges via notification hints 
                    // that is consistent across all DEs without side effects (like empty notifications).
                    // We simply do nothing here for now to avoid the "Empty Notification" bug.
                }
                #[cfg(desktop)]
                {
                     // Clear custom tray badge
                     crate::tray::set_badge(app_handle, false);
                }

                #[cfg(target_os = "macos")]
                {
                     // In Tauri v2, badge API is often on the window or requires trait.
                     // We use the main window.
                     use tauri::Manager; // Ensure Manager trait is in scope for get_webview_window
                     if let Some(window) = app_handle.get_webview_window("main") {
                         let _ = window.set_badge_count(Some(0));
                     }
                }
            }
            tauri::RunEvent::Exit => {
                tracing::info!("App exiting, signaling shutdown to background threads...");
                
                // Clear Cache on Exit
                clear_cache(app_handle);
                
                let state = app_handle.state::<AppState>();

                // Signal shutdown to background threads FIRST
                // This allows the clipboard monitor to exit gracefully before cleanup
                state.request_shutdown();

                // Give threads a moment to notice the shutdown signal
                std::thread::sleep(std::time::Duration::from_millis(100));

                tracing::info!("Dropping discovery service...");
                let mut discovery = state.discovery.lock().unwrap();
                *discovery = None; // Explicitly drop to trigger unregister

                // No "Goodbye" broadcast. We used to send Message::PeerRemoval(local_id)
                // here so peers' UIs would mark us offline immediately, but the
                // receiver's PeerRemoval handler (lib.rs ~4217) treats that as
                // "this peer is gone, drop them" and *removes the pinned
                // fingerprint from known_peers*. After both sides shut down,
                // whichever was still alive when the other quit lost its pin
                // and could not reconnect without re-pairing. mDNS
                // service-remove + QUIC keepalive already surface offline
                // status within seconds, so this broadcast was net-negative.
            }
            _ => {}
        }
    });
}

fn clear_cache(app: &tauri::AppHandle) {
    if let Ok(root_cache_dir) = app.path().app_cache_dir() {
        // Use a subdirectory to avoid nuking Webview2/GTK cache
        let cache_dir = root_cache_dir.join("temp_downloads");
        
        if func_exists(&cache_dir) {
            tracing::info!("Clearing temp downloads: {:?}", cache_dir);
            if let Err(e) = std::fs::remove_dir_all(&cache_dir) {
                tracing::error!("Failed to clear temp downloads: {}", e);
            }
            // Re-create it immediately
            let _ = std::fs::create_dir_all(&cache_dir);
        }
    }
    
    fn func_exists(path: &std::path::Path) -> bool {
        path.exists()
    }
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

