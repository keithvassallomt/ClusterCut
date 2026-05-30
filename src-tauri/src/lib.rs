mod clipboard;
mod compression;
#[cfg(target_os = "linux")]
mod dbus;
mod net_util;
mod pairing;
mod discovery;
mod netmon;
mod peer;
mod protocol;
mod state;
mod storage;
mod transport;
mod tray;

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState, ShortcutEvent};
#[cfg(target_os = "linux")]
use tauri::Listener;
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncBufReadExt, BufReader};
use std::str::FromStr;
use std::path::PathBuf;
use tokio::fs::File;
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

#[tauri::command]
async fn get_theme_override() -> Option<String> {
    std::env::var("CLUSTERCUT_THEME").ok()
}

#[tauri::command]
async fn get_current_theme(state: tauri::State<'_, AppState>) -> Result<Option<String>, ()> {
    Ok(state.current_theme.lock().unwrap().clone())
}

#[tauri::command]
async fn configure_autostart(app_handle: tauri::AppHandle, enable: bool) -> Result<bool, String> {
    // Check if running in Flatpak
    if cfg!(target_os = "linux") && std::env::var("FLATPAK_ID").is_ok() {
        let conn = zbus::Connection::session().await
            .map_err(|e| format!("Failed to connect to session bus: {}", e))?;

        // Build a proxy for the Background portal
        let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(&conn)
            .interface("org.freedesktop.portal.Background").unwrap()
            .path("/org/freedesktop/portal/desktop").unwrap()
            .destination("org.freedesktop.portal.Desktop").unwrap()
            .build()
            .await
            .map_err(|e| format!("Failed to create Background portal proxy: {}", e))?;

        // Compute a predictable handle_token from our unique bus name
        let unique_name = conn.unique_name()
            .ok_or("No unique bus name")?
            .as_str()
            .to_string();
        let sender_part = unique_name
            .trim_start_matches(':')
            .replace('.', "_");
        let handle_token = "clustercut_autostart";
        let request_path = format!(
            "/org/freedesktop/portal/desktop/request/{}/{}",
            sender_part, handle_token
        );

        // Subscribe to the Response signal on the request path BEFORE calling the method
        let response_proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(&conn)
            .interface("org.freedesktop.portal.Request").unwrap()
            .path(request_path.as_str()).unwrap()
            .destination("org.freedesktop.portal.Desktop").unwrap()
            .build()
            .await
            .map_err(|e| format!("Failed to create request proxy: {}", e))?;

        let mut response_stream: zbus::proxy::SignalStream<'_> = response_proxy
            .receive_signal("Response")
            .await
            .map_err(|e| format!("Failed to subscribe to Response signal: {}", e))?;

        // Build the options dict for RequestBackground
        let mut options = std::collections::HashMap::<&str, zbus::zvariant::Value<'_>>::new();
        options.insert("handle_token", zbus::zvariant::Value::from(handle_token));
        options.insert("reason", zbus::zvariant::Value::from(
            "ClusterCut needs to start at login to sync your clipboard across devices."
        ));
        options.insert("autostart", zbus::zvariant::Value::from(enable));
        if enable {
            let cmd: Vec<String> = vec![
                "clustercut".into(),
                "--minimized".into(),
            ];
            options.insert("commandline", zbus::zvariant::Value::from(cmd));
        }

        // Call RequestBackground (parent_window = "")
        proxy.call_method("RequestBackground", &("", options)).await
            .map_err(|e| format!("RequestBackground call failed: {}", e))?;

        // Wait for the Response signal with a 60s timeout
        use futures::StreamExt;
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            response_stream.next(),
        )
        .await
        .map_err(|_| "Portal request timed out — no response within 60 seconds".to_string())?
        .ok_or("Response signal stream ended unexpectedly")?;

        // Parse the response: (uint32 response, dict results)
        let body: zbus::message::Body = response.body();
        let (response_code, _results): (u32, std::collections::HashMap<String, zbus::zvariant::OwnedValue>) = body
            .deserialize()
            .map_err(|e| format!("Failed to deserialize portal response: {}", e))?;

        if response_code == 0 {
            // Approved — persist the state
            let state = app_handle.state::<AppState>();
            {
                let mut settings = state.settings.lock().unwrap();
                settings.flatpak_autostart = enable;
            }
            storage::save_settings(&app_handle, &state.settings.lock().unwrap());
            tracing::info!("Flatpak autostart {} via Background portal", if enable { "enabled" } else { "disabled" });
            Ok(true)
        } else {
            Err(format!("Autostart request was denied by the user (response code: {})", response_code))
        }
    } else {
        Ok(false) // Not handled — let tauri-plugin-autostart handle it
    }
}

#[tauri::command]
async fn get_autostart_state(app_handle: tauri::AppHandle) -> Result<Option<bool>, String> {
    if cfg!(target_os = "linux") && std::env::var("FLATPAK_ID").is_ok() {
        let state = app_handle.state::<AppState>();
        let settings = state.settings.lock().unwrap();
        Ok(Some(settings.flatpak_autostart))
    } else {
        Ok(None)
    }
}

#[tauri::command]
async fn show_native_notification(_app_handle: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use windows::UI::Notifications::{ToastNotificationManager, ToastNotification};
        use windows::Data::Xml::Dom::XmlDocument;
        use windows::core::HSTRING;

        let aumid = "app.clustercut.clustercut"; 

        // Raw XML for Native Actions
        // activationType="protocol" ensures clicking invokes "clustercut://..." which SingleInstance catches.
        let xml = format!(r#"
<toast activationType="protocol" launch="clustercut://action/show">
    <visual>
        <binding template="ToastGeneric">
            <text>{}</text>
            <text>{}</text>
        </binding>
    </visual>
</toast>
"#, title, body);

        let doc = XmlDocument::new().map_err(|e| e.to_string())?;
        doc.LoadXml(&HSTRING::from(&xml)).map_err(|e| e.to_string())?;

        let toast = ToastNotification::CreateToastNotification(&doc).map_err(|e| e.to_string())?;
        
        // Create Notifier and Show
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))
            .map_err(|e| e.to_string())?;
            
        notifier.Show(&toast).map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "linux")]
    {
        use notify_rust::Notification;
        let _ = Notification::new()
            .summary(&title)
            .body(&body)
            .appname("ClusterCut")
            .timeout(notify_rust::Timeout::Milliseconds(5000)) 
            .show()
            .map_err(|e| e.to_string());
    }

    #[cfg(target_os = "macos")]
    {
        send_notification(&_app_handle, &title, &body, false, None, "history", NotificationPayload::None);
    }
    
    Ok(())
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

fn get_hostname_internal() -> String {
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
    reset_network_state, load_settings, AppSettings,
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

fn check_and_notify_leave(app_handle: &tauri::AppHandle, state: &AppState, peer: &Peer) {
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

#[tauri::command]
fn get_device_id(state: tauri::State<'_, AppState>) -> String {
    state.local_device_id.lock().unwrap().clone()
}

#[tauri::command]
fn get_network_name(state: tauri::State<'_, AppState>) -> String {
    state.network_name.lock().unwrap().clone()
}

#[tauri::command]
fn get_network_pin(state: tauri::State<'_, AppState>) -> String {
    state.network_pin.lock().unwrap().clone()
}

#[tauri::command]
fn get_hostname(state: tauri::State<'_, AppState>) -> String {
    let settings = state.settings.lock().unwrap();
    if let Some(custom_name) = &settings.custom_device_name {
        if !custom_name.trim().is_empty() {
             return custom_name.clone();
        }
    }
    
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string())
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> AppSettings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(
    mut settings: AppSettings,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    // Preserve backend-only fields that the frontend doesn't manage
    settings.flatpak_autostart = state.settings.lock().unwrap().flatpak_autostart;
    *state.settings.lock().unwrap() = settings.clone();
    tracing::info!("Saving Settings: auto_send={}, auto_receive={}", settings.auto_send, settings.auto_receive);
    crate::storage::save_settings(&app_handle, &settings);
    let _ = app_handle.emit("settings-changed", settings.clone());
    
    #[cfg(desktop)]
    crate::tray::update_tray_menu(&app_handle);
    
    // Update Shortcuts
    register_shortcuts(&app_handle);
    // If auto_receive is now OFF, we might want to do something?
    // If device name changed, we should probably rebroadcast or something, 
    // but the next heartbeat or discovery probe will pick it up.
    // Ideally we emit an event if needed.
    
    // Check if network name changed via Provisioning (this function saves AppSettings, but UI might call separate commands for Network Name/PIN)
    // Wait, the UI for Provisioned Mode will likely update NetworkName/PIN directly? 
    // Or do we store them in AppSettings too? 
    // The requirement says "Provisioned mode, the user can enter a cluster name and PIN". 
    // Those are actually `state.network_name` and `state.network_pin`. 
    // `AppSettings` stores the *mode*. 
    // So the UI should call `save_network_identity` (new command needed?) or Update existing commands?
    // We already have `load_network_name` but no set command exposed.
    // I will add `set_network_identity` command.
}

#[tauri::command]
fn set_network_identity(
    name: String,
    pin: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    // Validate?
    *state.network_name.lock().unwrap() = name.clone();
    *state.network_pin.lock().unwrap() = pin.clone();
    
    crate::storage::save_network_name(&app_handle, &name);
    crate::storage::save_network_pin(&app_handle, &pin);
    
    // Also likely need to reset keys if we are "provisioning" a new identity? 
    // Or do we keep the key? 
    // If I type a new name/pin, I am essentially saying "I belong to THIS network now".
    // I need the key for THAT network. 
    // If I'm creating it, I generate a key. 
    // If I'm joining it (provisioned), I usually need the Key too OR I need to Pair.
    // But "Provisioned" usually means "I set the config manually". 
    // The prompt says "Toggle... default behaviour applies (random)... Provisioned... user can enter".
    // It doesn't say "User enters Key".
    // So "Provisioned" here effectively just means "Manual valid Network Name/PIN" instead of "Random Name/PIN".
    // It implies we are STARTING a cluster with this name/pin.
    // So we keep our current Key (or gen a new one). 
    // Since we are changing identity, a new Key is safer.
    // But if we just rename the cluster, we might want to keep the key.
    // Actually, if I just want to rename my cluster "My Home", I don't want to break existing peers if I can help it?
    // But existing peers know me by Key? No, they pair with Spake2 using PIN.
    // If I change PIN, they can't pair.
    // If I change Name, they see "My Home" instead of "Fuzzy-Badger".
    // I'll stick to just updating Name/PIN.
    
    // Re-register mDNS with new name
    let device_id = state.local_device_id.lock().unwrap().clone();
    
    // Get actual port from transport
    let port = if let Some(transport) = state.transport.lock().unwrap().as_ref() {
        transport.local_addr().map(|a| a.port()).unwrap_or(4654)
    } else {
        4654
    };

    // Discovery usually stores port.
    if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
          let _ = discovery.register(&device_id, &name, port);
    }
    
    let _ = app_handle.emit("network-update", ());
}

#[tauri::command]
fn regenerate_network_identity(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) {
    let (name, pin) = crate::storage::regenerate_identity(&app_handle);
    
    *state.network_name.lock().unwrap() = name.clone();
    *state.network_pin.lock().unwrap() = pin.clone();
    
    let device_id = state.local_device_id.lock().unwrap().clone();
    
    // Get actual port from transport
    let port = if let Some(transport) = state.transport.lock().unwrap().as_ref() {
        transport.local_addr().map(|a| a.port()).unwrap_or(4654)
    } else {
        4654
    };
    
    if let Some(discovery) = state.discovery.lock().unwrap().as_mut() {
          let _ = discovery.register(&device_id, &name, port);
    }
    
    let _ = app_handle.emit("network-update", ());
}

#[tauri::command]
fn get_listening_port(state: tauri::State<'_, AppState>) -> u16 {
    if let Some(transport) = state.transport.lock().unwrap().as_ref() {
        transport.local_addr().map(|a| a.port()).unwrap_or(4654)
    } else {
        4654
    }
}

#[tauri::command]
fn get_peers(state: tauri::State<AppState>) -> std::collections::HashMap<String, Peer> {
    state.get_peers()
}

#[tauri::command]
fn get_known_peers(state: tauri::State<AppState>) -> std::collections::HashMap<String, Peer> {
    state.known_peers.lock().unwrap().clone()
}

/// List of peers loaded from `known_peers.json` without a stored cert
/// fingerprint. Returns an empty Vec for clean v0.3 installs. The frontend
/// reads this on mount to decide whether to show the "please re-pair"
/// banner after a v0.2 → v0.3 upgrade.
#[tauri::command]
fn get_legacy_peers(state: tauri::State<AppState>) -> Vec<crate::state::LegacyPeerInfo> {
    state.legacy_peers.lock().unwrap().clone()
}

/// Dismiss the legacy-peer banner for the current run. The banner reappears
/// on next startup if any legacy peers are still present in known_peers,
/// so the user is reminded until they re-pair (or forget) every affected
/// peer.
#[tauri::command]
fn dismiss_legacy_peer_banner(state: tauri::State<AppState>) {
    state.legacy_peers.lock().unwrap().clear();
}

/// Returns true when the user has at least one manual peer AND none of those
/// manual peers are on a directly-reachable subnet. This is the gate for the
/// "having trouble connecting?" modal — show it only when we'd actually expect
/// remote/VPN connectivity to a manual peer. If a manual peer is on the local
/// subnet, "no peers online" just means peers are offline, not a connection
/// problem worth surfacing.
#[tauri::command]
fn expects_remote_manual_peers(state: tauri::State<AppState>) -> bool {
    let peers = state.known_peers.lock().unwrap();
    let manual: Vec<_> = peers.values().filter(|p| p.is_manual).collect();
    if manual.is_empty() {
        return false;
    }
    !manual.iter().any(|p| net_util::is_in_local_subnet(p.ip))
}

#[tauri::command]
fn log_frontend(message: String, level: Option<String>) {
    match level.as_deref() {
        Some("error") => tracing::error!("[Frontend] {}", message),
        Some("warn") => tracing::warn!("[Frontend] {}", message),
        Some("debug") => tracing::debug!("[Frontend] {}", message),
        Some("trace") => tracing::trace!("[Frontend] {}", message),
        _ => tracing::info!("[Frontend] {}", message),
    }
}

#[tauri::command]
fn get_local_ip() -> String {
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

use ipnetwork::IpNetwork;

#[tauri::command]
async fn add_manual_peer(
    ip: String, // Can be IP or CIDR
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    
    // 1. Try parsing as CIDR
    if let Ok(net) = ip.parse::<IpNetwork>() {
        tracing::info!("Scanning range: {}", net);
        let ips: Vec<std::net::IpAddr> = net.iter().collect();
        
        // Scan in small batches with concurrency
        let batch_size = 50; 
        for chunk in ips.chunks(batch_size) {
            let mut tasks = Vec::new();
            for ip_addr in chunk {
                 let s = (*state).clone();
                 let t = (*transport).clone();
                 let a = app_handle.clone();
                 let addr = *ip_addr;
                 
                 // Skip own IP
                 if let Ok(local) = t.local_addr() {
                     if local.ip() == addr { continue; }
                 }
                 
                 tasks.push(tauri::async_runtime::spawn(async move {
                     net_util::probe_ip(addr, 4654, s, t, a).await; // Fixed Port 4654
                 }));
            }
            futures::future::join_all(tasks).await;
        }
        Ok(())
    } else {
         // 2. Try parsing as normal IP or SocketAddr
        // If just IP, assume port 4654.
        let (addr, port) = if let Ok(sock) = ip.parse::<std::net::SocketAddr>() {
            (sock.ip(), sock.port())
        } else if let Ok(ip_addr) = ip.parse::<std::net::IpAddr>() {
            (ip_addr, 4654)
        } else {
             return Err("Invalid Format. Use IP, IP:PORT, or CIDR (e.g. 192.168.1.0/24)".to_string());
        };

        // For single IP, PROBE IT.
        net_util::probe_ip(addr, port, (*state).clone(), (*transport).clone(), app_handle).await;
        Ok(())
    }
}

#[tauri::command]
async fn leave_network(
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let local_id = state.local_device_id.lock().unwrap().clone();
    
    // 1. Broadcast "Self-Removal" to Network
    let removal_msg = Message::PeerRemoval(local_id.clone());
    let data = serde_json::to_vec(&removal_msg).unwrap_or_default();
    
    let peers_snapshot = state.get_peers();
    for (id, p) in peers_snapshot.iter() {
         if *id == local_id { continue; }
         
         let addr = std::net::SocketAddr::new(p.ip, p.port);
         let transport_clone = (*transport).clone();
         let data_vec = data.clone();
         
         tauri::async_runtime::spawn(async move {
             let _ = transport_clone.send_message(addr, &data_vec).await;
         });
    }
    
    // 2. Perform Factory Reset Locally
    let port = transport.local_addr().map(|a| a.port()).unwrap_or(0);
    perform_factory_reset(&app_handle, &state, port);
    
    Ok(())
}

#[tauri::command]
async fn delete_peer(
    peer_id: String,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // 0. Broadcast Removal (Kick) to Network
    let removal_msg = Message::PeerRemoval(peer_id.clone());
    let data = serde_json::to_vec(&removal_msg).unwrap_or_default();
    
    // We can allow gossip_peer or manual iteration.
    // Manual iteration is safer to ensure it hits everyone including the target.
    let peers_snapshot = state.get_peers();
    for (id, p) in peers_snapshot.iter() {
         // Don't gossip to self (obv)
         if *id == state.local_device_id.lock().unwrap().clone() {
             continue;
         }
         
         let addr = std::net::SocketAddr::new(p.ip, p.port);
         let transport_clone = (*transport).clone();
         let data_vec = data.clone();
         
         tauri::async_runtime::spawn(async move {
             let _ = transport_clone.send_message(addr, &data_vec).await;
         });
    }

    // 1. Remove from Known Peers
    {
        let mut kp = state.known_peers.lock().unwrap();
        if kp.remove(&peer_id).is_some() {
            save_known_peers(&app_handle, &kp);
        }
    }

    // 2. Remove from Runtime Peers
    {
        let mut peers = state.peers.lock().unwrap();
        peers.remove(&peer_id);
    }

    // 3. Emit Removal
    let _ = app_handle.emit("peer-remove", &peer_id);

    Ok(())
}

// Helper to wipe state and restart network identity
fn perform_factory_reset(app_handle: &tauri::AppHandle, state: &AppState, port: u16) {
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

#[tauri::command]
async fn send_clipboard(
    text: String,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    
    // Manual Send Command
    clipboard::set_clipboard(&app_handle, text.clone()); // Update local clipboard too? Yes, usually.
    
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
    let _ = app_handle.emit("clipboard-change", &payload_obj);

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
async fn delete_history_item(
    app_handle: tauri::AppHandle,
    id: String,
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
) -> Result<(), String> {
    // 1. Emit Local Event (to update UI immediately)
    tracing::info!("Deleting history item locally: {}", id);
    let _ = app_handle.emit("history-delete", &id);

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
async fn set_local_clipboard(app: tauri::AppHandle, text: String) -> Result<(), String> {
    clipboard::set_clipboard(&app, text);
    Ok(())
}

#[tauri::command]
async fn exit_app(app_handle: tauri::AppHandle) {
    app_handle.exit(0);
}

#[tauri::command]
async fn retry_connection(
    state: tauri::State<'_, AppState>,
    transport: tauri::State<'_, Transport>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // Clone inner values to own them for the async task
    let state_owned = (*state).clone();
    let transport_owned = (*transport).clone();
    let app_handle_clone = app_handle.clone();
    
    // Re-run the startup probe logic
    tauri::async_runtime::spawn(async move {
         let known_peers = {
             state_owned.known_peers.lock().unwrap().clone()
         };
         
         if !known_peers.is_empty() {
             tracing::info!("Retry Connection: Probing {} known peers...", known_peers.len());
             for (_id, peer) in known_peers {
                 let s = state_owned.clone();
                 let t = transport_owned.clone();
                 let a = app_handle_clone.clone();

                 tauri::async_runtime::spawn(async move {
                     net_util::probe_ip(peer.ip, peer.port, s, t, a).await;
                 });
             }
         } else {
             // If no known peers, maybe we should try scanning? 
             // But for now, we only care about reconnecting to knowns.
             tracing::warn!("Retry Connection: No known peers to probe.");
         }
    });
    
    Ok(())
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
async fn promote_pending_rich(
    state: tauri::State<'_, AppState>,
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

    clipboard::set_clipboard_rich(&app_handle, payload.text.clone(), formats);
    let _ = app_handle.emit("clipboard-change", &payload);
    Ok(())
}

#[tauri::command]
async fn confirm_pending_clipboard(
    state: tauri::State<'_, AppState>,
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
            clipboard::set_clipboard_image(&app_handle, blob);
        } else if let Some(formats) = payload
            .formats
            .as_ref()
            .filter(|fs| !fs.is_empty())
            .cloned()
        {
            clipboard::set_clipboard_rich(&app_handle, payload.text.clone(), formats);
        } else {
            clipboard::set_clipboard(&app_handle, payload.text.clone());
        }

        // Emit change event so history updates
        let _ = app_handle.emit("clipboard-change", &payload);

        Ok(())
    } else {
        Err("No pending clipboard content".to_string())
    }
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
        .plugin(tauri_plugin_global_shortcut::Builder::new().with_handler(handle_shortcut).build())
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
                register_shortcuts(app_handle);
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
                             Ok(msg) => handle_message(msg, addr, listener_state, listener_handle, transport_inside).await,
                             Err(e) => tracing::error!("Failed to parse message: {}", e), 
                         }
                    });
                },
                move |recv, addr| {
                    tracing::info!("Received FILE stream from {}", addr);
                    let state = file_state.clone();
                    let handle = file_handle.clone();
                    
                    tauri::async_runtime::spawn(async move {
                         handle_incoming_file_stream(recv, addr, state, handle).await;
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
            get_local_ip,
            get_peers,

            add_manual_peer,
            pairing::start_pairing,
            delete_peer,
            leave_network,
            get_network_name,
            request_file,
            delete_history_item,
            check_gnome_extension_status,
            get_network_pin,
            get_device_id,
            get_hostname,
            get_settings,
            get_known_peers,
            expects_remote_manual_peers,
            log_frontend,
            save_settings,
            set_network_identity,
            regenerate_network_identity,
            send_clipboard,
            set_local_clipboard,
            set_local_clipboard_files,
            confirm_pending_clipboard,
            promote_pending_rich,
            get_launch_args,
            exit_app,
            retry_connection,
            configure_autostart,
            get_autostart_state,
            get_listening_port,
            show_native_notification,
            get_theme_override,
            get_current_theme,
            get_legacy_peers,
            dismiss_legacy_peer_banner,
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

#[tauri::command]
async fn set_local_clipboard_files(app: tauri::AppHandle, paths: Vec<String>) -> Result<(), String> {
    clipboard::set_clipboard_paths(&app, paths);
    Ok(())
}

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
    let cap = clipboard::common::MAX_CLIPBOARD_IMAGE_BYTES;
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
        clipboard::set_clipboard_image(&app, blob.clone());
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

async fn handle_incoming_file_stream(recv: quinn::RecvStream, addr: std::net::SocketAddr, state: AppState, app: tauri::AppHandle) {
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

async fn handle_message(msg: Message, addr: std::net::SocketAddr, listener_state: AppState, listener_handle: tauri::AppHandle, transport_inside: Transport) {
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
                                        clipboard::set_clipboard_image(&listener_handle, blob);
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
                                        if blob_size > clipboard::common::LARGE_CLIPBOARD_BLOB_NOTIFY_THRESHOLD {
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
                                            clipboard::get_backend(),
                                            clipboard::ClipboardBackend::GnomeExtension
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
                                        clipboard::set_clipboard(&listener_handle, text.clone());
                                        let _ = listener_handle.emit("clipboard-change", &payload_obj);
                                    } else {
                                        clipboard::set_clipboard_rich(&listener_handle, text.clone(), formats);
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
                                    clipboard::set_clipboard(&listener_handle, text.clone());
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
                 let _ = listener_handle.emit("peer-update", &peer);

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
                     save_known_peers(listener_handle.app_handle(), &kp_lock);
                 } else {
                     if kp_lock.contains_key(&peer.id) {
                         tracing::info!("Removing untrusted auto-peer {} from persistence.", peer.id);
                         kp_lock.remove(&peer.id);
                         save_known_peers(listener_handle.app_handle(), &kp_lock);
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
                        save_known_peers(listener_handle.app_handle(), &kp);
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

#[tauri::command]
async fn request_file(
    _app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    file_id: String,
    file_index: usize,
    peer_id: String,
) -> Result<(), String> {
    request_file_internal(&state, file_id, file_index, peer_id).await
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

fn register_shortcuts(app_handle: &tauri::AppHandle) {
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

fn handle_shortcut(app_handle: &tauri::AppHandle, shortcut: &Shortcut, event: ShortcutEvent) {
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
#[derive(serde::Serialize)]
struct ExtensionStatus {
    is_gnome: bool,
    is_installed: bool,
    /// True when on GNOME Wayland without the extension — clipboard sync will NOT work
    clipboard_requires_extension: bool,
}

#[tauri::command]
async fn check_gnome_extension_status() -> ExtensionStatus {
    let xdg_current_desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let is_gnome = xdg_current_desktop.contains("GNOME");

    if !is_gnome {
        return ExtensionStatus { is_gnome: false, is_installed: false, clipboard_requires_extension: false };
    }

    #[cfg(target_os = "linux")]
    let is_wayland = clipboard::is_wayland();
    #[cfg(not(target_os = "linux"))]
    let is_wayland = false;

    // Try D-Bus first (works in Flatpak if permissions are set)
    if let Ok(connection) = zbus::Connection::session().await {
         let proxy_result: zbus::Result<zbus::Proxy> = zbus::Proxy::new(
             &connection,
             "org.gnome.Shell",
             "/org/gnome/Shell",
             "org.gnome.Shell.Extensions"
         ).await;

         if let Ok(proxy) = proxy_result {
              // Method: ListExtensions() -> a{sa{sv}}
              // Returns a map where key is UUID, value is properties
              // Use OwnedValue to avoid lifetime issues with DynamicDeserialize
              let call_result: zbus::Result<std::collections::HashMap<String, std::collections::HashMap<String, zbus::zvariant::OwnedValue>>> = proxy.call("ListExtensions", &()).await;
              
              if let Ok(extensions) = call_result {
                   let is_installed = extensions.contains_key("clustercut@keithvassallo.com");
                   return ExtensionStatus {
                       is_gnome: true,
                       is_installed,
                       clipboard_requires_extension: is_wayland && !is_installed,
                   };
              }
         }
    }

    // Fallback to File Check (for native builds)
    let home = std::env::var("HOME").unwrap_or_default();
    let local_path = format!("{}/.local/share/gnome-shell/extensions/clustercut@keithvassallo.com", home);
    let system_path = "/usr/share/gnome-shell/extensions/clustercut@keithvassallo.com";

    let is_installed = std::path::Path::new(&local_path).exists() || std::path::Path::new(system_path).exists();

    ExtensionStatus {
        is_gnome: true,
        is_installed,
        clipboard_requires_extension: is_wayland && !is_installed,
    }
}

#[tauri::command]
fn get_launch_args() -> Vec<String> {
    std::env::args().collect()
}

