use crate::state::AppState;
use crate::protocol::Message;
use crate::peer::Peer;
use std::sync::atomic::Ordering;
use tauri::Manager;

const GRACE_PERIOD_SECS: u64 = 45;

// ── Shared Recovery Logic ──────────────────────────────────────────────────

pub fn on_suspend(state: &AppState) {
    tracing::info!("[Netmon] System suspending — suppressing notifications");
    state.network_suspended.store(true, Ordering::Relaxed);
}

pub fn on_resume(state: &AppState) {
    tracing::info!("[Netmon] System resumed — starting recovery");
    resume_recovery(state, true);
}

pub fn on_network_down(state: &AppState) {
    let was_available = state.network_available.swap(false, Ordering::Relaxed);
    if was_available {
        tracing::info!("[Netmon] Network down — suppressing notifications");
    }
}

pub fn on_network_up(state: &AppState) {
    let was_down = !state.network_available.load(Ordering::Relaxed);
    if was_down {
        tracing::info!("[Netmon] Network restored — starting recovery");
        resume_recovery(state, false);
    }
}

fn resume_recovery(state: &AppState, from_suspend: bool) {
    // 1. Set grace period
    {
        let mut grace = state.resume_grace_until.lock().unwrap();
        *grace = Some(std::time::Instant::now() + std::time::Duration::from_secs(GRACE_PERIOD_SECS));
    }
    tracing::info!("[Netmon] Grace period set to {}s", GRACE_PERIOD_SECS);

    // 2. Check for network change (different IP = wifi changed)
    let ip_changed = check_ip_changed(state);
    if ip_changed {
        tracing::info!("[Netmon] Local IP changed — peers on old network will be silently removed");
        // Don't clear pending removals: old-network peers are genuinely gone.
        // The grace period suppresses their notifications.
    } else {
        // 3. Clear pending removals only if IP didn't change
        // (same network, peers are likely still there — cancel stale removals)
        let mut pending = state.pending_removals.lock().unwrap();
        let count = pending.len();
        pending.clear();
        if count > 0 {
            tracing::info!("[Netmon] Cleared {} pending removals", count);
        }
    }

    // 4. Mark network as available and clear suspend flag
    state.network_available.store(true, Ordering::Relaxed);
    if from_suspend {
        state.network_suspended.store(false, Ordering::Relaxed);
    }
    state.consecutive_heartbeat_failures.store(0, Ordering::Relaxed);

    // 5. Re-register mDNS and re-probe are done asynchronously in start_recovery_tasks
}

fn check_ip_changed(state: &AppState) -> bool {
    match local_ip_address::local_ip() {
        Ok(current_ip) => {
            let mut last_ip = state.last_known_local_ip.lock().unwrap();
            let changed = last_ip.map_or(false, |prev| prev != current_ip);
            *last_ip = Some(current_ip);
            changed
        }
        Err(e) => {
            tracing::warn!("[Netmon] Failed to get local IP: {}", e);
            false
        }
    }
}

/// Spawns async tasks for mDNS re-registration and peer re-probing.
/// Must be called from an async context with access to AppHandle.
pub fn start_recovery_tasks(app_handle: &tauri::AppHandle) {
    let state: AppState = (*app_handle.state::<AppState>()).clone();
    let _handle = app_handle.clone();

    tauri::async_runtime::spawn(async move {
        // Re-register mDNS
        {
            let device_id = state.local_device_id.lock().unwrap().clone();
            let network_name = state.network_name.lock().unwrap().clone();

            let transport_opt = state.transport.lock().unwrap().clone();
            if let Some(transport) = transport_opt {
                if let Ok(addr) = transport.local_addr() {
                    let mut discovery = state.discovery.lock().unwrap();
                    if let Some(disc) = discovery.as_mut() {
                        match disc.register(&device_id, &network_name, addr.port()) {
                            Ok(()) => tracing::info!("[Netmon] Re-registered mDNS service"),
                            Err(e) => tracing::error!("[Netmon] Failed to re-register mDNS: {}", e),
                        }
                    }
                }
            }
        }

        // Re-probe all known peers
        {
            let known_peers: Vec<Peer> = {
                state.known_peers.lock().unwrap().values().cloned().collect()
            };
            let transport_opt = state.transport.lock().unwrap().clone();

            if let Some(transport) = transport_opt {
                if !known_peers.is_empty() {
                    tracing::info!("[Netmon] Re-probing {} known peers", known_peers.len());
                    for peer in known_peers {
                        let addr = std::net::SocketAddr::new(peer.ip, peer.port);
                        if let Ok(ping_data) = serde_json::to_vec(&Message::Ping) {
                            let t = transport.clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = tokio::time::timeout(
                                    std::time::Duration::from_secs(3),
                                    t.send_message(addr, &ping_data),
                                ).await;
                            });
                        }
                    }
                }
            }
        }
    });
}


// ── Linux Implementation ───────────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub async fn start_network_monitor(app_handle: tauri::AppHandle) {
    let state: AppState = (*app_handle.state::<AppState>()).clone();

    // Initialize last known IP
    if let Ok(ip) = local_ip_address::local_ip() {
        *state.last_known_local_ip.lock().unwrap() = Some(ip);
    }

    // Spawn logind monitor (suspend/resume)
    let logind_state = state.clone();
    let logind_handle = app_handle.clone();
    let logind_task = tauri::async_runtime::spawn(async move {
        if let Err(e) = linux_logind_monitor(logind_state, logind_handle).await {
            tracing::error!("[Netmon] logind monitor failed: {}", e);
        }
    });

    // Spawn network monitor (portal first, NM fallback)
    let net_state = state.clone();
    let net_handle = app_handle.clone();
    let net_task = tauri::async_runtime::spawn(async move {
        if let Err(e) = linux_portal_network_monitor(net_state.clone(), net_handle.clone()).await {
            tracing::warn!("[Netmon] Portal NetworkMonitor unavailable ({}), trying NetworkManager...", e);
            if let Err(e2) = linux_nm_network_monitor(net_state, net_handle).await {
                tracing::warn!("[Netmon] NetworkManager monitor unavailable ({}), relying on heartbeat fallback", e2);
            }
        }
    });

    // Keep alive — these tasks run forever
    let _ = tokio::join!(logind_task, net_task);
}

#[cfg(target_os = "linux")]
async fn linux_logind_monitor(state: AppState, app_handle: tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use futures::StreamExt;

    let conn = zbus::Connection::system().await?;
    tracing::info!("[Netmon] Connected to system bus for logind");

    let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(&conn)
        .destination("org.freedesktop.login1")?
        .path("/org/freedesktop/login1")?
        .interface("org.freedesktop.login1.Manager")?
        .build()
        .await?;

    let mut stream: zbus::proxy::SignalStream<'_> = proxy.receive_signal("PrepareForSleep").await?;
    tracing::info!("[Netmon] Listening for PrepareForSleep signals");

    while let Some(signal) = stream.next().await {
        let body: zbus::message::Body = signal.body();
        if let Ok((going_to_sleep,)) = body.deserialize::<(bool,)>() {
            if going_to_sleep {
                on_suspend(&state);
            } else {
                on_resume(&state);
                start_recovery_tasks(&app_handle);
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn linux_portal_network_monitor(state: AppState, app_handle: tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use futures::StreamExt;

    let conn = zbus::Connection::session().await?;
    tracing::info!("[Netmon] Connected to session bus for portal NetworkMonitor");

    let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(&conn)
        .destination("org.freedesktop.portal.Desktop")?
        .path("/org/freedesktop/portal/desktop")?
        .interface("org.freedesktop.portal.NetworkMonitor")?
        .build()
        .await?;

    // Initial connectivity check
    match proxy.call_method("GetConnectivity", &()).await {
        Ok(reply) => {
            let body: zbus::message::Body = reply.body();
            if let Ok((connectivity,)) = body.deserialize::<(u32,)>() {
                tracing::info!("[Netmon] Initial portal connectivity: {}", connectivity);
                if connectivity < 4 {
                    on_network_down(&state);
                }
            }
        }
        Err(e) => {
            return Err(format!("GetConnectivity failed: {}", e).into());
        }
    }

    let mut stream: zbus::proxy::SignalStream<'_> = proxy.receive_signal("changed").await?;
    tracing::info!("[Netmon] Listening for portal NetworkMonitor changes");

    while let Some(_signal) = stream.next().await {
        // Query current connectivity on change
        match proxy.call_method("GetConnectivity", &()).await {
            Ok(reply) => {
                let body: zbus::message::Body = reply.body();
                if let Ok((connectivity,)) = body.deserialize::<(u32,)>() {
                    tracing::debug!("[Netmon] Portal connectivity changed: {}", connectivity);
                    if connectivity >= 4 {
                        on_network_up(&state);
                        start_recovery_tasks(&app_handle);
                    } else {
                        on_network_down(&state);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("[Netmon] Failed to query connectivity: {}", e);
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn linux_nm_network_monitor(state: AppState, app_handle: tauri::AppHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use futures::StreamExt;

    let conn = zbus::Connection::system().await?;
    tracing::info!("[Netmon] Connected to system bus for NetworkManager");

    let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(&conn)
        .destination("org.freedesktop.NetworkManager")?
        .path("/org/freedesktop/NetworkManager")?
        .interface("org.freedesktop.NetworkManager")?
        .build()
        .await?;

    let mut stream: zbus::proxy::SignalStream<'_> = proxy.receive_signal("StateChanged").await?;
    tracing::info!("[Netmon] Listening for NetworkManager StateChanged signals");

    while let Some(signal) = stream.next().await {
        let body: zbus::message::Body = signal.body();
        if let Ok((nm_state,)) = body.deserialize::<(u32,)>() {
            tracing::debug!("[Netmon] NetworkManager state: {}", nm_state);
            // NM states: 20=DISCONNECTED, 30=DISCONNECTING, 40=CONNECTING,
            // 50=CONNECTED_LOCAL, 60=CONNECTED_SITE, 70=CONNECTED_GLOBAL
            if nm_state >= 70 {
                on_network_up(&state);
                start_recovery_tasks(&app_handle);
            } else if nm_state <= 40 {
                on_network_down(&state);
            }
        }
    }
    Ok(())
}


// ── Windows Implementation ─────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub async fn start_network_monitor(app_handle: tauri::AppHandle) {
    let state: AppState = (*app_handle.state::<AppState>()).clone();

    // Initialize last known IP
    if let Ok(ip) = local_ip_address::local_ip() {
        *state.last_known_local_ip.lock().unwrap() = Some(ip);
    }

    // Spawn power monitor (suspend/resume via WM_POWERBROADCAST)
    let power_state = state.clone();
    let power_handle = app_handle.clone();
    std::thread::spawn(move || {
        windows_power_monitor(power_state, power_handle);
    });

    // Spawn network monitor on a dedicated thread (handler is not Send)
    let net_state = state.clone();
    let net_handle = app_handle.clone();
    std::thread::spawn(move || {
        windows_network_monitor(net_state, net_handle);
    });
}

#[cfg(target_os = "windows")]
fn windows_power_monitor(state: AppState, app_handle: tauri::AppHandle) {
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW,
        RegisterClassW, TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE,
        WINDOW_STYLE, WNDCLASSW,
    };
    use windows::core::PCWSTR;

    unsafe {
        let class_name: Vec<u16> = "ClusterCutPowerMonitor\0".encode_utf16().collect();

        let wc = WNDCLASSW {
            lpfnWndProc: Some(power_wndproc),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        // Store state in a Box that we pass via CreateWindowEx lpParam
        // We'll use a static instead since wndproc can't easily access instance data
        POWER_MONITOR_STATE.set(Some((state, app_handle)));

        let _hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE::default(),
            0, 0, 0, 0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        ).expect("Failed to create power monitor window");

        tracing::info!("[Netmon] Windows power monitor started");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(target_os = "windows")]
thread_local! {
    static POWER_MONITOR_STATE: std::cell::Cell<Option<(AppState, tauri::AppHandle)>> = std::cell::Cell::new(None);
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn power_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, WM_POWERBROADCAST, PBT_APMSUSPEND, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND,
    };

    if msg == WM_POWERBROADCAST {
        POWER_MONITOR_STATE.with(|cell| {
            // Safety: we peek without taking
            let opt = cell.take();
            if let Some((ref state, ref handle)) = opt {
                let event = wparam.0 as u32;
                if event == PBT_APMSUSPEND {
                    on_suspend(state);
                } else if event == PBT_APMRESUMEAUTOMATIC || event == PBT_APMRESUMESUSPEND {
                    on_resume(state);
                    start_recovery_tasks(handle);
                }
            }
            cell.set(opt);
        });
        return windows::Win32::Foundation::LRESULT(1); // TRUE = handled
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(target_os = "windows")]
fn windows_network_monitor(state: AppState, app_handle: tauri::AppHandle) {
    use windows::Networking::Connectivity::{NetworkInformation, NetworkConnectivityLevel};

    // Check initial state
    match NetworkInformation::GetInternetConnectionProfile() {
        Ok(profile) => {
            match profile.GetNetworkConnectivityLevel() {
                Ok(level) => {
                    if level != NetworkConnectivityLevel::InternetAccess {
                        tracing::info!("[Netmon] Initial network state: no internet access");
                        on_network_down(&state);
                    }
                }
                Err(_) => {}
            }
        }
        Err(_) => {
            tracing::info!("[Netmon] No internet connection profile at startup");
            on_network_down(&state);
        }
    }

    // Subscribe to network status changes
    let net_state = state.clone();
    let net_handle = app_handle.clone();
    let handler = windows::Networking::Connectivity::NetworkStatusChangedEventHandler::new(
        move |_sender| {
            let connected = match NetworkInformation::GetInternetConnectionProfile() {
                Ok(profile) => {
                    match profile.GetNetworkConnectivityLevel() {
                        Ok(level) => level == NetworkConnectivityLevel::InternetAccess,
                        Err(_) => false,
                    }
                }
                Err(_) => false,
            };

            if connected {
                tracing::debug!("[Netmon] Windows network: connected");
                on_network_up(&net_state);
                start_recovery_tasks(&net_handle);
            } else {
                tracing::debug!("[Netmon] Windows network: disconnected");
                on_network_down(&net_state);
            }
            Ok(())
        },
    );

    match NetworkInformation::NetworkStatusChanged(&handler) {
        Ok(token) => {
            tracing::info!("[Netmon] Windows network monitor registered");
            // Keep alive — block this thread forever so the token stays in scope
            std::thread::park();
            // Prevent unused warning & ensure cleanup
            let _ = token;
        }
        Err(e) => {
            tracing::error!("[Netmon] Failed to register Windows network monitor: {}", e);
        }
    }
}


// ── macOS Implementation ───────────────────────────────────────────────────

#[cfg(target_os = "macos")]
pub async fn start_network_monitor(app_handle: tauri::AppHandle) {
    let state: AppState = (*app_handle.state::<AppState>()).clone();

    // Initialize last known IP
    if let Ok(ip) = local_ip_address::local_ip() {
        *state.last_known_local_ip.lock().unwrap() = Some(ip);
    }

    // Spawn sleep/wake monitor
    let sleep_state = state.clone();
    let sleep_handle = app_handle.clone();
    std::thread::spawn(move || {
        macos_sleep_monitor(sleep_state, sleep_handle);
    });

    // Spawn network reachability monitor
    let net_state = state.clone();
    let net_handle = app_handle.clone();
    std::thread::spawn(move || {
        macos_network_monitor(net_state, net_handle);
    });

    // Keep the async task alive
    std::future::pending::<()>().await;
}

#[cfg(target_os = "macos")]
fn macos_sleep_monitor(state: AppState, app_handle: tauri::AppHandle) {
    use nsworkspace::NSWorkspace;

    tracing::info!("[Netmon] macOS sleep monitor starting");

    let sleep_state = state.clone();
    NSWorkspace::observe_will_sleep(move || {
        on_suspend(&sleep_state);
    });

    let wake_state = state.clone();
    let wake_handle = app_handle.clone();
    NSWorkspace::observe_did_wake(move || {
        on_resume(&wake_state);
        start_recovery_tasks(&wake_handle);
    });

    // NSWorkspace notifications require a running CFRunLoop on this thread
    unsafe {
        core_foundation::runloop::CFRunLoop::get_current().run();
    }
}

#[cfg(target_os = "macos")]
fn macos_network_monitor(state: AppState, app_handle: tauri::AppHandle) {
    use system_configuration::network_reachability::{
        ReachabilityFlags, SCNetworkReachability,
    };

    tracing::info!("[Netmon] macOS network reachability monitor starting");

    let addr = std::net::SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)),
        0,
    );
    let mut reachability = SCNetworkReachability::from(addr);

    let net_state = state.clone();
    let net_handle = app_handle.clone();

    let callback = move |flags: ReachabilityFlags| {
        let reachable = flags.contains(ReachabilityFlags::REACHABLE)
            && !flags.contains(ReachabilityFlags::CONNECTION_REQUIRED);

        if reachable {
            tracing::debug!("[Netmon] macOS network: reachable");
            on_network_up(&net_state);
            start_recovery_tasks(&net_handle);
        } else {
            tracing::debug!("[Netmon] macOS network: not reachable");
            on_network_down(&net_state);
        }
    };

    if reachability.set_callback(callback).is_ok() {
        if reachability.schedule_with_runloop(
            &core_foundation::runloop::CFRunLoop::get_current(),
            unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
        ) {
            tracing::info!("[Netmon] macOS network reachability callback registered");
            unsafe {
                core_foundation::runloop::CFRunLoop::get_current().run();
            }
        } else {
            tracing::error!("[Netmon] Failed to schedule reachability with run loop");
        }
    } else {
        tracing::error!("[Netmon] Failed to set reachability callback");
    }
}
