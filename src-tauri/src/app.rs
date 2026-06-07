//! Application entry point: Tauri builder, CLI arg parsing, logging setup,
//! and OS-level startup/lifecycle helpers.

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
#[cfg(target_os = "linux")]
use tauri::Listener;

use crate::discovery::Discovery;
use crate::peer::Peer;
use rand::Rng;
use crate::state::{AppState, LegacyPeerInfo};
use crate::storage::{
    load_cluster_id, load_device_id, load_known_peers, load_network_name,
    save_cluster_id, save_device_id, save_known_peers,
    wipe_legacy_cluster_key,
    load_settings,
};
use tauri::{Emitter, Manager};
use crate::transport::Transport;
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

    tracing::info!("Logging initialized. Level: {}, Hostname: {}", level, crate::get_hostname_internal());

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
                crate::check_and_notify_leave(&handle, &state, &peer);
            }
            let _ = handle.emit("peer-remove", &peer_id);
        } else {
            tracing::debug!("[Discovery] Removal debounce cancelled (nonce updated during probe) for {}", peer_id);
        }
    } else {
        tracing::debug!("[Discovery] Removal debounce cancelled (entry removed during probe) for {}", peer_id);
    }
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

pub(crate) fn run() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // Initialize Logging and get Args
    let args = init_logging();
    let minimized_arg = args.minimized;

    // Detect clipboard backend on Linux before building
    #[cfg(target_os = "linux")]
    let _clipboard_backend = crate::clipboard::detect_backend();

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
        if crate::clipboard::should_init_plugin() {
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
        .plugin(tauri_plugin_global_shortcut::Builder::new().with_handler(crate::shortcuts::handle_shortcut).build())
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
            let (cert_der, key_der) = match crate::storage::load_device_cert(app.handle()) {
                Some((c, k)) => (c, k),
                None => {
                    let (c, k) = crate::transport::generate_self_signed_cert()
                        .expect("Failed to generate device cert");
                    crate::storage::save_device_cert(app.handle(), &c, &k);
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
                // Issue #18: skip auto-config when the user disabled it. Settings
                // are loaded into state below (later in setup), but this block runs
                // earlier, so read straight from disk here.
                if crate::storage::load_settings(app_handle).configure_firewall {
                    // Ensure firewall rule exists; checks first and only prompts UAC if needed.
                    std::thread::spawn(|| {
                        crate::net_util::configure_windows_firewall();
                    });
                }
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
                // Re-apply owner-only perms to secret files for installs created
                // before this hardening (issue: secret file perms).
                crate::storage::harden_secret_files(app_handle);
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
                        if peer.needs_repair() {
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
                crate::shortcuts::register_shortcuts(app_handle);
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

                // 3b-ii. Load the cluster-name register version + origin
                // (issue: cluster-name convergence). A pre-feature install has
                // no version/origin files → version 0, origin seeded to our own
                // device_id so the register is well-formed.
                let nn_version = crate::storage::load_network_name_version(app_handle);
                let mut nn_origin = crate::storage::load_network_name_origin(app_handle);
                if nn_origin.is_empty() {
                    nn_origin = device_id.clone();
                }
                *state.network_name_version.lock().unwrap() = nn_version;
                *state.network_name_origin.lock().unwrap() = nn_origin;

                // 3c. Establish Network PIN — mode-aware: persisted in
                // provisioned, ephemeral (in-memory, file deleted) in auto.
                // Issue 4. Settings are already loaded into state above, so the
                // mode is available here.
                let cluster_mode = state.settings.lock().unwrap().cluster_mode.clone();
                let network_pin = crate::storage::establish_network_pin(app_handle, &cluster_mode);
                *state.network_pin.lock().unwrap() = network_pin.clone();
                tracing::info!("Network PIN established (mode: {})", cluster_mode);

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
                                 crate::net_util::probe_ip(peer.ip, peer.port, s, t, a).await;
                             });
                         }
                     }
                });

                // 4. Register Discovery. Browsing always runs so we can still
                // discover peers; advertising (register) is gated on the
                // mdns_advertising setting (issue #18).
                let mut discovery = Discovery::new().expect("Failed to initialize discovery");
                if state.settings.lock().unwrap().mdns_advertising {
                    discovery
                        .register(&device_id, &network_name, port)
                        .expect("Failed to register service");
                } else {
                    tracing::info!("mDNS advertising disabled by settings; browsing only.");
                }
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
                                    let _ = d_handle.emit("peer-update", crate::peer::PeerView::from_peer(&peer));

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
                                                    crate::send_notification(&verify_handle, "Device Joined", &format!("{} has joined your cluster", verify_peer.hostname), false, Some(1), "devices", crate::NotificationPayload::None);
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
                            crate::pairing::handle_pairing_connection(stream, peer_addr, state, app, t),
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
            let conn_state = listener_state.clone();
            let conn_app = listener_handle.clone();

            transport.start_listening(
                move |data, addr| {
                    tracing::trace!("Received {} bytes from {}", data.len(), addr);
                    let listener_handle = listener_handle.clone();
                    let listener_state = listener_state.clone();
                    let transport_inside = transport_inside.clone();

                    // ... Existing Message Handler Code ...
                    tauri::async_runtime::spawn(async move {
                         match serde_json::from_slice::<Message>(&data) {
                             Ok(msg) => crate::handlers::handle_message(msg, addr, listener_state, listener_handle, transport_inside).await,
                             Err(e) => tracing::error!("Failed to parse message: {}", e),
                         }
                    });
                },
                move |recv, addr| {
                    tracing::info!("Received FILE stream from {}", addr);
                    let state = file_state.clone();
                    let handle = file_handle.clone();

                    tauri::async_runtime::spawn(async move {
                         crate::handlers::handle_incoming_file_stream(recv, addr, state, handle).await;
                    });
                },
                move |kind: &str, addr: std::net::SocketAddr, detail: Option<String>| {
                    let (level, msg) = match kind {
                        "connect" => (crate::diagnostics::DiagLevel::Minimal, "mTLS connection established".to_string()),
                        "drop" => (crate::diagnostics::DiagLevel::Minimal, "mTLS connection dropped".to_string()),
                        "handshake_failed" => (
                            crate::diagnostics::DiagLevel::Detailed,
                            format!("mTLS handshake failed: {}", detail.unwrap_or_default()),
                        ),
                        _ => (crate::diagnostics::DiagLevel::Detailed, kind.to_string()),
                    };
                    crate::diagnostics::push_diagnostic(&conn_state, &conn_app, level, "mtls", Some(addr.to_string()), msg);
                }
            );
            // Start Clipboard Monitor
            let transport_for_clipboard = transport.clone();
            let state_for_clipboard = (*app.state::<AppState>()).clone();

            crate::clipboard::start_monitor(
                app.handle().clone(),
                state_for_clipboard,
                transport_for_clipboard,
            );

            // Windows-only concurrent-clipboard race self-test. Inert unless the
            // CLUSTERCUT_CLIPRACE env var is set; see run_clip_race_selftest.
            #[cfg(target_os = "windows")]
            crate::clipboard::run_clip_race_selftest(app.handle().clone());

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

                             crate::check_and_notify_leave(&prune_handle, &prune_state, &peer);
                             let _ = prune_handle.emit("peer-remove", &id);
                         }
                         save_known_peers(prune_handle.app_handle(), &kp_lock);
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            crate::commands::peers::get_local_ip,
            crate::commands::peers::get_peers,

            crate::commands::peers::add_manual_peer,
            crate::commands::peers::add_remote_peer,
            crate::pairing::start_pairing,
            crate::commands::peers::delete_peer,
            crate::commands::peers::leave_network,
            crate::commands::identity::get_network_name,
            crate::commands::clipboard::request_file,
            crate::commands::clipboard::delete_history_item,
            crate::commands::system::check_gnome_extension_status,
            crate::commands::identity::get_network_pin,
            crate::commands::identity::get_device_id,
            crate::commands::identity::get_hostname,
            crate::commands::settings::get_settings,
            crate::commands::diagnostics::get_diagnostic_events,
            crate::commands::diagnostics::clear_diagnostic_events,
            crate::commands::peers::get_known_peers,
            crate::commands::peers::expects_remote_manual_peers,
            crate::commands::system::log_frontend,
            crate::commands::settings::save_settings,
            crate::commands::identity::set_network_identity,
            crate::commands::identity::regenerate_network_identity,
            crate::commands::clipboard::send_clipboard,
            crate::commands::clipboard::set_local_clipboard,
            crate::commands::clipboard::set_local_clipboard_files,
            crate::commands::clipboard::confirm_pending_clipboard,
            crate::commands::clipboard::promote_pending_rich,
            crate::commands::system::get_launch_args,
            crate::commands::system::exit_app,
            crate::commands::peers::retry_connection,
            crate::commands::system::configure_autostart,
            crate::commands::system::get_autostart_state,
            crate::commands::peers::get_listening_port,
            crate::commands::system::show_native_notification,
            crate::commands::theme::get_theme_override,
            crate::commands::theme::get_current_theme,
            crate::commands::peers::get_legacy_peers,
            crate::commands::peers::dismiss_legacy_peer_banner,
            crate::pairing::is_pairing_locked_out,
            crate::pairing::rearm_pairing,
            crate::pairing::get_pairing_accept,
            crate::pairing::set_pairing_accept,
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
