//! Global shortcuts on Wayland through the XDG GlobalShortcuts portal
//! (`org.freedesktop.portal.GlobalShortcuts`). The Tauri global-shortcut
//! plugin is X11-only, so on Wayland the manual Send/Receive shortcuts are
//! bound here instead.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use futures::StreamExt;
use tauri::{Emitter, Manager};
use tokio::sync::Mutex;
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

use crate::state::AppState;

/// App ID the portal identifies us by. Must match the installed
/// `app.clustercut.clustercut.desktop`, or `Registry.Register` is refused.
pub(crate) const APP_ID: &str = "app.clustercut.clustercut";

/// Stable shortcut IDs: KDE and GNOME remember approvals per app ID + shortcut
/// ID, and Hyprland binds reference them as `app.clustercut.clustercut:<id>`.
pub(crate) const SEND_ID: &str = "manual-send";
pub(crate) const RECEIVE_ID: &str = "manual-receive";

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const GLOBAL_SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";

/// Where the portal binding stands, for the settings UI.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum PortalStatus {
    /// Both directions are automatic, so no shortcuts are bound.
    #[default]
    Inactive,
    /// This desktop has no GlobalShortcuts portal.
    Unavailable,
    /// The user turned down the desktop's dialog. Not asked again until manual
    /// mode is switched off and on.
    Declined,
    /// Bound. `triggers` maps shortcut ID to the desktop's own description of
    /// the key ("Ctrl+Alt+C" on KDE, "Press <Control><Alt>c" on GNOME, empty on
    /// Hyprland, which leaves keys to the user's config).
    Bound { version: u32, triggers: HashMap<String, String> },
    Error { message: String },
}

struct Session {
    /// Dedicated connection: `Registry.Register` must be its first portal call.
    conn: Connection,
    handle: OwnedObjectPath,
    listeners: Vec<tauri::async_runtime::JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        for listener in &self.listeners {
            listener.abort();
        }
    }
}

#[derive(Default)]
struct Portal {
    session: Option<Session>,
    status: PortalStatus,
    declined: bool,
}

fn portal() -> &'static Mutex<Portal> {
    static PORTAL: OnceLock<Mutex<Portal>> = OnceLock::new();
    PORTAL.get_or_init(Default::default)
}

pub(crate) async fn status() -> PortalStatus {
    portal().lock().await.status.clone()
}

/// Open the desktop's own settings for ClusterCut's shortcuts (portal v2+).
pub(crate) async fn configure(app: &tauri::AppHandle) -> Result<(), String> {
    let parent = parent_window(app).await;
    let portal = portal().lock().await;
    let session = portal.session.as_ref().ok_or("No global shortcuts session")?;
    let options: HashMap<&str, Value> = HashMap::new();
    session
        .conn
        .call_method(Some(PORTAL_NAME), PORTAL_PATH, Some(GLOBAL_SHORTCUTS), "ConfigureShortcuts", &(&session.handle, parent.as_str(), options))
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Ask again after the user declined the desktop's dialog, or retry after an error.
pub(crate) fn retry(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        portal().lock().await.declined = false;
        spawn_sync(app);
    });
}

/// Run `sync` as its own task. A plain fn on purpose: async code in this module
/// that awaited `sync` directly would make its future type recursive (and
/// unprovably `Send`).
pub(crate) fn spawn_sync(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(sync(app));
}

/// Bring the portal session in line with the settings: one exists while either
/// direction is in manual mode. Idempotent, and an existing session is never
/// re-bound (the portal allows one `BindShortcuts` per session).
pub(crate) async fn sync(app: tauri::AppHandle) {
    let wanted = {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().unwrap();
        !settings.auto_send || !settings.auto_receive
    };
    let mut portal = portal().lock().await;
    let changed = if wanted && portal.session.is_none() && !portal.declined {
        match start(&app).await {
            Ok((session, status)) => {
                portal.session = Some(session);
                portal.status = status;
            }
            Err(StartError::Declined) => {
                tracing::info!("Global shortcuts dialog declined; not asking again until manual mode is re-enabled");
                portal.declined = true;
                portal.status = PortalStatus::Declined;
            }
            Err(StartError::Unavailable(e)) => {
                tracing::warn!("No GlobalShortcuts portal, manual shortcuts unavailable: {}", e);
                portal.status = PortalStatus::Unavailable;
            }
            Err(StartError::Failed(e)) => {
                tracing::error!("Failed to bind global shortcuts through the portal: {}", e);
                portal.status = PortalStatus::Error { message: e.to_string() };
            }
        }
        true
    } else if !wanted {
        portal.declined = false;
        portal.status = PortalStatus::Inactive;
        if let Some(session) = portal.session.take() {
            stop(session).await;
        }
        true
    } else {
        false
    };
    let active = portal.session.is_some();
    drop(portal);
    if changed {
        let _ = app.emit("shortcut-status-changed", ());
    }
    // Keys or the Hyprland opt-in may have changed even when the session didn't.
    let _ = tauri::async_runtime::spawn_blocking(move || crate::hyprland::sync(&app, active)).await;
}

enum StartError {
    Unavailable(zbus::Error),
    Declined,
    Failed(zbus::Error),
}

impl From<zbus::Error> for StartError {
    fn from(e: zbus::Error) -> Self {
        StartError::Failed(e)
    }
}

async fn start(app: &tauri::AppHandle) -> Result<(Session, PortalStatus), StartError> {
    // Dev builds only: every desktop we test on has the portal, so this is the
    // way to see the no-portal UI and its gdbus fallback.
    #[cfg(debug_assertions)]
    if std::env::var_os("CLUSTERCUT_FORCE_NO_PORTAL").is_some() {
        return Err(StartError::Unavailable(zbus::Error::Failure("forced by CLUSTERCUT_FORCE_NO_PORTAL".into())));
    }

    let conn = Connection::session().await?;
    let unique_name = conn.unique_name().map(|n| n.to_string()).unwrap_or_default();

    // Host apps tell the portal who they are; without this it guesses from the
    // launching process (from a VS Code terminal we became com.microsoft.VSCode).
    // Inside Flatpak the sandbox supplies the ID.
    if !crate::clipboard::is_flatpak() {
        let options: HashMap<&str, Value> = HashMap::new();
        if let Err(e) = conn
            .call_method(Some(PORTAL_NAME), PORTAL_PATH, Some("org.freedesktop.host.portal.Registry"), "Register", &(APP_ID, options))
            .await
        {
            tracing::warn!("Portal Registry.Register failed, app ID will be guessed: {}", e);
        }
    }

    let version: u32 = conn
        .call_method(Some(PORTAL_NAME), PORTAL_PATH, Some("org.freedesktop.DBus.Properties"), "Get", &(GLOBAL_SHORTCUTS, "version"))
        .await
        .and_then(|reply| reply.body().deserialize::<OwnedValue>())
        .and_then(|value| u32::try_from(value).map_err(zbus::Error::from))
        .map_err(StartError::Unavailable)?;

    let token = next_token();
    let session_token = format!("clustercut{}", next_token());
    let options = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("session_handle_token", Value::from(session_token.as_str())),
    ]);
    let (code, mut results) = request(&conn, "CreateSession", &(options,), &token).await?;
    if code != 0 {
        return Err(StartError::Failed(zbus::Error::Failure(format!("CreateSession response {code}"))));
    }
    // Spec quirk: `session_handle` is a string, not an object path.
    let handle = results
        .remove("session_handle")
        .and_then(|v| String::try_from(v).ok())
        .unwrap_or_else(|| session_path(&unique_name, &session_token));
    let handle = OwnedObjectPath::try_from(handle).map_err(zbus::Error::from)?;

    // Listen before binding so an activation right after approval isn't lost.
    let listeners = vec![
        spawn_activation_listener(app, &conn, &handle).await?,
        spawn_change_listener(app, &conn, &handle).await?,
        spawn_portal_restart_listener(app, &conn).await?,
    ];
    let session = Session { conn, handle, listeners };

    let (send_accel, receive_accel) = {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().unwrap();
        (settings.shortcut_send.clone(), settings.shortcut_receive.clone())
    };
    let shortcut = |description: &'static str, accel: Option<String>| {
        let mut props = HashMap::from([("description", Value::from(description))]);
        if let Some(trigger) = accel.as_deref().and_then(to_xdg_trigger) {
            props.insert("preferred_trigger", Value::from(trigger));
        }
        props
    };
    let shortcuts = vec![
        (SEND_ID, shortcut("Send clipboard to cluster", send_accel)),
        (RECEIVE_ID, shortcut("Apply received clipboard", receive_accel)),
    ];
    let parent = parent_window(app).await;
    let token = next_token();
    let options = HashMap::from([("handle_token", Value::from(token.as_str()))]);
    let (code, results) = request(&session.conn, "BindShortcuts", &(&session.handle, shortcuts, parent.as_str(), options), &token).await?;
    match code {
        0 => {
            let triggers = triggers_from(results);
            tracing::info!("Global shortcuts bound through the portal (v{}, parent {:?}): {:?}", version, parent, triggers);
            Ok((session, PortalStatus::Bound { version, triggers }))
        }
        1 => {
            stop(session).await;
            Err(StartError::Declined)
        }
        _ => {
            stop(session).await;
            Err(StartError::Failed(zbus::Error::Failure(format!("BindShortcuts response {code}"))))
        }
    }
}

async fn stop(session: Session) {
    if let Err(e) = session
        .conn
        .call_method(Some(PORTAL_NAME), &session.handle, Some("org.freedesktop.portal.Session"), "Close", &())
        .await
    {
        tracing::debug!("Closing global shortcuts session failed: {}", e);
    }
}

/// The main window as an xdg-foreign handle ("wayland:<handle>") so the desktop
/// can attach its shortcuts dialog to it. GNOME's dialog never names the app,
/// so the parent window is its only context. Empty when the window is hidden or
/// not a Wayland surface, or GDK doesn't answer within a second.
async fn parent_window(app: &tauri::AppHandle) -> String {
    let Some(window) = app.get_webview_window("main") else {
        return String::new();
    };
    if !window.is_visible().unwrap_or(false) {
        return String::new();
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let main = window.clone();
    if window.run_on_main_thread(move || gdk_export::export(&main, tx)).is_err() {
        return String::new();
    }
    match tokio::time::timeout(std::time::Duration::from_secs(1), rx).await {
        Ok(Ok(handle)) => format!("wayland:{handle}"),
        _ => String::new(),
    }
}

/// GDK's xdg-foreign export (gdk/gdkwayland.h, GTK 3.22+). Never unexported:
/// GTK reuses one export per window until the window goes, and unexporting
/// right after `ConfigureShortcuts` would race the desktop importing it.
mod gdk_export {
    use std::ffi::{c_char, c_int, c_void, CStr};

    use gtk::glib::translate::ToGlibPtr;
    use gtk::prelude::*;
    use tokio::sync::oneshot::Sender;

    type Exported = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void);

    // libgdk-3 is already linked through gtk.
    extern "C" {
        fn gdk_wayland_window_export_handle(
            window: *mut c_void,
            callback: Exported,
            user_data: *mut c_void,
            destroy: Option<unsafe extern "C" fn(*mut c_void)>,
        ) -> c_int;
    }

    unsafe extern "C" fn on_exported(_window: *mut c_void, handle: *const c_char, slot: *mut c_void) {
        let slot = &mut *(slot as *mut Option<Sender<String>>);
        if let (Some(tx), false) = (slot.take(), handle.is_null()) {
            let _ = tx.send(CStr::from_ptr(handle).to_string_lossy().into_owned());
        }
    }

    unsafe extern "C" fn free_slot(slot: *mut c_void) {
        drop(Box::from_raw(slot as *mut Option<Sender<String>>));
    }

    /// Main thread only. Sends the handle once GDK has it; drops `tx` unsent if
    /// the window can't be exported.
    pub(super) fn export(window: &tauri::WebviewWindow, tx: Sender<String>) {
        let Some(gdk_window) = window.gtk_window().ok().and_then(|w| w.window()) else {
            return;
        };
        // An X11 window (GDK_BACKEND=x11) has no xdg-foreign handle.
        if gdk_window.type_().name() != "GdkWaylandWindow" {
            return;
        }
        let ptr: *mut gtk::gdk::ffi::GdkWindow = gdk_window.to_glib_none().0;
        let slot = Box::into_raw(Box::new(Some(tx))) as *mut c_void;
        // SAFETY: `ptr` is a live GdkWaylandWindow (checked above) and we're on
        // the GTK main thread. GDK calls `on_exported` at most once and then
        // `free_slot` exactly once, unless it returns FALSE, in which case it
        // kept nothing and the slot is still ours to free.
        let queued = unsafe { gdk_wayland_window_export_handle(ptr as *mut c_void, on_exported, slot, Some(free_slot)) };
        if queued == 0 {
            unsafe { free_slot(slot) };
        }
    }
}

fn next_token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("clustercut_{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Call a portal method that answers through a Request object and wait for
/// its Response. Subscribes first, so a fast reply can't be missed.
async fn request<B>(conn: &Connection, method: &str, body: &B, token: &str) -> zbus::Result<(u32, HashMap<String, OwnedValue>)>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let unique_name = conn.unique_name().map(|n| n.to_string()).unwrap_or_default();
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface("org.freedesktop.portal.Request")?
        .member("Response")?
        .path(request_path(&unique_name, token))?
        .build();
    let mut responses = MessageStream::for_match_rule(rule, conn, Some(1)).await?;
    conn.call_method(Some(PORTAL_NAME), PORTAL_PATH, Some(GLOBAL_SHORTCUTS), method, body).await?;
    // No timeout: BindShortcuts can wait on the user for as long as the dialog is open.
    let response = responses
        .next()
        .await
        .ok_or_else(|| zbus::Error::Failure(format!("{method}: portal went away before responding")))??;
    response.body().deserialize()
}

/// Shortcut ID to `trigger_description` from a Bind/ListShortcuts response.
fn triggers_from(mut results: HashMap<String, OwnedValue>) -> HashMap<String, String> {
    let shortcuts = results
        .remove("shortcuts")
        .and_then(|v| Vec::<(String, HashMap<String, OwnedValue>)>::try_from(v).ok())
        .unwrap_or_default();
    triggers_from_list(shortcuts)
}

fn triggers_from_list(shortcuts: Vec<(String, HashMap<String, OwnedValue>)>) -> HashMap<String, String> {
    shortcuts
        .into_iter()
        .map(|(id, mut props)| {
            let trigger = props
                .remove("trigger_description")
                .and_then(|v| String::try_from(v).ok())
                .unwrap_or_default();
            (id, trigger)
        })
        .collect()
}

async fn signal_stream(conn: &Connection, interface: &'static str, member: &'static str, path: &str) -> zbus::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(interface)?
        .member(member)?
        .path(path.to_string())?
        .build();
    MessageStream::for_match_rule(rule, conn, None).await
}

async fn spawn_activation_listener(
    app: &tauri::AppHandle,
    conn: &Connection,
    handle: &OwnedObjectPath,
) -> zbus::Result<tauri::async_runtime::JoinHandle<()>> {
    let mut activations = signal_stream(conn, GLOBAL_SHORTCUTS, "Activated", PORTAL_PATH).await?;
    let (app, handle) = (app.clone(), handle.clone());
    Ok(tauri::async_runtime::spawn(async move {
        while let Some(Ok(msg)) = activations.next().await {
            type Activated = (OwnedObjectPath, String, u64, HashMap<String, OwnedValue>);
            let Ok((session, id, _timestamp, _options)) = msg.body().deserialize::<Activated>() else {
                continue;
            };
            if session != handle {
                continue;
            }
            tracing::info!("Portal shortcut activated: {}", id);
            let app = app.clone();
            // Off the executor: the GNOME clipboard bridge is a blocking D-Bus call.
            match id.as_str() {
                SEND_ID => drop(tauri::async_runtime::spawn_blocking(move || crate::shortcuts::manual_send(&app))),
                RECEIVE_ID => drop(tauri::async_runtime::spawn_blocking(move || crate::shortcuts::manual_receive(&app))),
                _ => {}
            }
        }
    }))
}

/// The user can rebind the keys in their desktop's settings; keep the status current.
async fn spawn_change_listener(
    app: &tauri::AppHandle,
    conn: &Connection,
    handle: &OwnedObjectPath,
) -> zbus::Result<tauri::async_runtime::JoinHandle<()>> {
    let mut changes = signal_stream(conn, GLOBAL_SHORTCUTS, "ShortcutsChanged", PORTAL_PATH).await?;
    let (app, handle) = (app.clone(), handle.clone());
    Ok(tauri::async_runtime::spawn(async move {
        while let Some(Ok(msg)) = changes.next().await {
            type Changed = (OwnedObjectPath, Vec<(String, HashMap<String, OwnedValue>)>);
            let Ok((session, shortcuts)) = msg.body().deserialize::<Changed>() else {
                continue;
            };
            if session != handle {
                continue;
            }
            let mut portal = portal().lock().await;
            if let PortalStatus::Bound { triggers, .. } = &mut portal.status {
                *triggers = triggers_from_list(shortcuts);
                tracing::info!("Global shortcuts changed by the desktop: {:?}", triggers);
                let _ = app.emit("shortcut-status-changed", ());
            }
        }
    }))
}

/// If xdg-desktop-portal restarts (e.g. a package upgrade), our session is
/// gone with it. Drop it and bind again once the portal is back.
async fn spawn_portal_restart_listener(app: &tauri::AppHandle, conn: &Connection) -> zbus::Result<tauri::async_runtime::JoinHandle<()>> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, PORTAL_NAME)?
        .build();
    let mut owners = MessageStream::for_match_rule(rule, conn, None).await?;
    let app = app.clone();
    Ok(tauri::async_runtime::spawn(async move {
        while let Some(Ok(msg)) = owners.next().await {
            let Ok((_name, _old, new_owner)) = msg.body().deserialize::<(String, String, String)>() else {
                continue;
            };
            if new_owner.is_empty() {
                continue;
            }
            tracing::warn!("xdg-desktop-portal restarted; re-binding global shortcuts");
            // From a separate task: dropping the session aborts this listener.
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                drop(portal().lock().await.session.take());
                spawn_sync(app);
            });
            return;
        }
    }))
}

/// A stored Tauri accelerator (`CommandOrControl+Alt+C`) split into modifiers
/// and an xkb keysym name, ready to format for the portal or for Hyprland.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Accelerator {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    /// xkb keysym name without `XKB_KEY_` (`c`, `Return`, `F1`).
    pub key: String,
}

impl Accelerator {
    /// `None` for keys with no known keysym name (punctuation, for one).
    pub(crate) fn parse(accelerator: &str) -> Option<Self> {
        let (mut ctrl, mut alt, mut shift, mut logo) = (false, false, false, false);
        let mut key = None;
        for part in accelerator.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "commandorcontrol" | "cmdorctrl" | "control" | "ctrl" => ctrl = true,
                "alt" | "option" => alt = true,
                "shift" => shift = true,
                "super" | "command" | "cmd" | "meta" => logo = true,
                _ if key.is_none() => key = Some(keysym_name(part)?),
                _ => return None,
            }
        }
        Some(Self { ctrl, alt, shift, logo, key: key? })
    }

    /// Modifiers in a fixed order, under the caller's names for CTRL/ALT/SHIFT/LOGO.
    pub(crate) fn modifiers<'a>(&self, names: [&'a str; 4]) -> Vec<&'a str> {
        [self.ctrl, self.alt, self.shift, self.logo]
            .into_iter()
            .zip(names)
            .filter_map(|(on, name)| on.then_some(name))
            .collect()
    }
}

/// Convert a stored Tauri accelerator (`CommandOrControl+Alt+C`) to the
/// freedesktop shortcuts-spec trigger the portal expects (`CTRL+ALT+c`).
/// Returns `None` for keys with no known keysym name; the shortcut is then
/// bound without a preferred trigger and the desktop asks the user for one.
pub(crate) fn to_xdg_trigger(accelerator: &str) -> Option<String> {
    let accel = Accelerator::parse(accelerator)?;
    let mut trigger = accel.modifiers(["CTRL", "ALT", "SHIFT", "LOGO"]);
    trigger.push(&accel.key);
    Some(trigger.join("+"))
}

/// xkb keysym name (without `XKB_KEY_`) for a key as `ShortcutRecorder` stores it.
fn keysym_name(key: &str) -> Option<String> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return c.is_ascii_alphanumeric().then(|| c.to_ascii_lowercase().to_string());
    }
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=24).contains(&n).then(|| format!("F{n}"));
    }
    let name = match key {
        "Space" => "space",
        "Enter" => "Return",
        "Backspace" => "BackSpace",
        "Tab" => "Tab",
        "Delete" => "Delete",
        "Insert" => "Insert",
        "Home" => "Home",
        "End" => "End",
        "PageUp" => "Page_Up",
        "PageDown" => "Page_Down",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        _ => return None,
    };
    Some(name.to_string())
}

/// The sender element of portal object paths: the unique bus name without
/// its leading `:` and with `.` replaced by `_` (`:1.42` becomes `1_42`).
fn sender_element(unique_name: &str) -> String {
    unique_name.trim_start_matches(':').replace('.', "_")
}

/// Object path the portal will use for the Request created with `token`.
/// Known in advance so the Response signal can be subscribed before calling.
pub(crate) fn request_path(unique_name: &str, token: &str) -> String {
    format!("/org/freedesktop/portal/desktop/request/{}/{}", sender_element(unique_name), token)
}

/// Object path of the Session created with `session_handle_token`.
pub(crate) fn session_path(unique_name: &str, token: &str) -> String {
    format!("/org/freedesktop/portal/desktop/session/{}/{}", sender_element(unique_name), token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_default_shortcuts() {
        assert_eq!(to_xdg_trigger("CommandOrControl+Alt+C").as_deref(), Some("CTRL+ALT+c"));
        assert_eq!(to_xdg_trigger("CommandOrControl+Alt+V").as_deref(), Some("CTRL+ALT+v"));
    }

    #[test]
    fn orders_modifiers_and_maps_super() {
        assert_eq!(to_xdg_trigger("Super+Shift+Space").as_deref(), Some("SHIFT+LOGO+space"));
        assert_eq!(to_xdg_trigger("Shift+Alt+Control+K").as_deref(), Some("CTRL+ALT+SHIFT+k"));
    }

    #[test]
    fn maps_named_keys() {
        assert_eq!(to_xdg_trigger("CommandOrControl+Enter").as_deref(), Some("CTRL+Return"));
        assert_eq!(to_xdg_trigger("Alt+ArrowUp").as_deref(), Some("ALT+Up"));
        assert_eq!(to_xdg_trigger("Alt+PageDown").as_deref(), Some("ALT+Page_Down"));
        assert_eq!(to_xdg_trigger("Control+F12").as_deref(), Some("CTRL+F12"));
        assert_eq!(to_xdg_trigger("Control+Alt+1").as_deref(), Some("CTRL+ALT+1"));
    }

    #[test]
    fn rejects_unmappable_accelerators() {
        assert_eq!(to_xdg_trigger("Control+/"), None);
        assert_eq!(to_xdg_trigger("Control+F25"), None);
        assert_eq!(to_xdg_trigger("Control+Alt"), None);
        assert_eq!(to_xdg_trigger("Control+A+B"), None);
        assert_eq!(to_xdg_trigger(""), None);
    }

    #[test]
    fn builds_portal_paths() {
        assert_eq!(request_path(":1.27312", "cc_1"), "/org/freedesktop/portal/desktop/request/1_27312/cc_1");
        assert_eq!(session_path(":1.93", "ccsession"), "/org/freedesktop/portal/desktop/session/1_93/ccsession");
    }
}
