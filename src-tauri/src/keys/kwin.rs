//! The focused window's app on KDE Plasma, for spotting terminals. KWin only
//! tells scripts, so ClusterCut loads a small one that reports each window
//! activation back over D-Bus (`dbus::KWinReports`). Loaded on first use,
//! unloaded on exit.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use tauri::Manager;

const PLUGIN: &str = "clustercut-active-window";
const KWIN: &str = "org.kde.KWin";
/// Where the script calls back (`dbus::KWinReports`).
pub(crate) const REPORT_PATH: &str = "/app/clustercut/clustercut/KWin";
/// The script reports the current window as soon as it runs.
const FIRST_REPORT_TIMEOUT: Duration = Duration::from_millis(500);

// KWin 6 names first, Plasma 5's as the fallback.
const SCRIPT: &str = r#"// Loaded by ClusterCut (app.clustercut.clustercut): reports the focused app so
// its Copy & Send and Receive & Paste shortcuts press Ctrl+Shift+C/V in
// terminals instead of Ctrl+C/V. Unloaded when ClusterCut quits.
function report(window) {
    var id = window ? (window.desktopFileName || window.resourceClass || "") : "";
    callDBus("app.clustercut.clustercut", "/app/clustercut/clustercut/KWin",
             "app.clustercut.clustercut.KWin", "ActiveWindow", String(id));
}
(workspace.windowActivated || workspace.clientActivated).connect(report);
report(workspace.activeWindow !== undefined ? workspace.activeWindow : workspace.activeClient);
"#;

struct Active {
    loaded: bool,
    /// `None` until the script's first report.
    app_id: Option<String>,
}

static ACTIVE: Mutex<Active> = Mutex::new(Active { loaded: false, app_id: None });
static REPORTED: Condvar = Condvar::new();

/// From the script, over D-Bus.
pub(crate) fn report(app_id: String) {
    ACTIVE.lock().unwrap().app_id = Some(app_id);
    REPORTED.notify_all();
}

/// The focused window's desktop file name (or class), loading the script the
/// first time. `None` if KWin won't run it.
pub(super) fn active_window(app: &tauri::AppHandle) -> Option<String> {
    let loaded = ACTIVE.lock().unwrap().loaded;
    if !loaded {
        match tauri::async_runtime::block_on(load(app)) {
            Ok(()) => ACTIVE.lock().unwrap().loaded = true,
            Err(e) => {
                tracing::warn!("Couldn't load the KWin script that spots terminals: {}", e);
                return None;
            }
        }
    }
    let active = ACTIVE.lock().unwrap();
    let (active, _) = REPORTED.wait_timeout_while(active, FIRST_REPORT_TIMEOUT, |a| a.app_id.is_none()).unwrap();
    active.app_id.clone().filter(|id| !id.is_empty())
}

async fn load(app: &tauri::AppHandle) -> Result<(), String> {
    // KWin reads the file itself; the app cache dir is the same path outside Flatpak's sandbox.
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("kwin-active-window.js");
    std::fs::write(&path, SCRIPT).map_err(|e| e.to_string())?;

    let conn = zbus::Connection::session().await.map_err(|e| e.to_string())?;
    // An earlier run that never reached its exit hook leaves it loaded.
    unload_with(&conn).await;
    let id: i32 = conn
        .call_method(Some(KWIN), "/Scripting", Some("org.kde.kwin.Scripting"), "loadScript", &(path.to_string_lossy().as_ref(), PLUGIN))
        .await
        .and_then(|reply| reply.body().deserialize())
        .map_err(|e| e.to_string())?;
    if id < 0 {
        return Err("KWin refused the script".to_string());
    }
    // Plasma 6 path, then Plasma 5's.
    let mut result = Err(String::new());
    for path in [format!("/Scripting/Script{id}"), format!("/{id}")] {
        result = conn.call_method(Some(KWIN), path.as_str(), Some("org.kde.kwin.Script"), "run", &()).await.map(|_| ()).map_err(|e| e.to_string());
        if result.is_ok() {
            break;
        }
    }
    tracing::info!("Loaded the KWin script that reports the focused app (id {})", id);
    result
}

async fn unload_with(conn: &zbus::Connection) {
    let _ = conn.call_method(Some(KWIN), "/Scripting", Some("org.kde.kwin.Scripting"), "unloadScript", &(PLUGIN,)).await;
}

/// On exit; harmless if it was never loaded.
pub(crate) fn unload() {
    if !ACTIVE.lock().unwrap().loaded {
        return;
    }
    tauri::async_runtime::block_on(async {
        if let Ok(conn) = zbus::Connection::session().await {
            unload_with(&conn).await;
        }
    });
}
