//! Shared plumbing for the XDG desktop portals ClusterCut uses on Linux: the
//! GlobalShortcuts portal (`portal_shortcuts`) and the RemoteDesktop portal
//! (`keys::remote_desktop`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use futures::StreamExt;
use tauri::Manager;
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

/// App ID the portals identify us by. Must match the installed
/// `app.clustercut.clustercut.desktop`, or `Registry.Register` is refused.
pub(crate) const APP_ID: &str = "app.clustercut.clustercut";

pub(crate) const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
pub(crate) const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// A session-bus connection for one portal, identified as ClusterCut. Host
/// apps tell the portal who they are; without this it guesses from the
/// launching process (from a VS Code terminal we became com.microsoft.VSCode).
/// `Registry.Register` must be the first portal call on a connection, hence a
/// dedicated one. Inside Flatpak the sandbox supplies the ID.
pub(crate) async fn connect_as_app() -> zbus::Result<Connection> {
    let conn = Connection::session().await?;
    if !crate::clipboard::is_flatpak() {
        let options: HashMap<&str, Value> = HashMap::new();
        if let Err(e) = conn
            .call_method(Some(PORTAL_NAME), PORTAL_PATH, Some("org.freedesktop.host.portal.Registry"), "Register", &(APP_ID, options))
            .await
        {
            tracing::warn!("Portal Registry.Register failed, app ID will be guessed: {}", e);
        }
    }
    Ok(conn)
}

/// A `u32` property of a portal interface.
pub(crate) async fn property_u32(conn: &Connection, interface: &str, name: &str) -> zbus::Result<u32> {
    conn.call_method(Some(PORTAL_NAME), PORTAL_PATH, Some("org.freedesktop.DBus.Properties"), "Get", &(interface, name))
        .await
        .and_then(|reply| reply.body().deserialize::<OwnedValue>())
        .and_then(|value| u32::try_from(value).map_err(zbus::Error::from))
}

/// A portal interface's `version` property; an error means the portal lacks it.
pub(crate) async fn version(conn: &Connection, interface: &str) -> zbus::Result<u32> {
    property_u32(conn, interface, "version").await
}

/// Close a portal session; failures only matter to the log.
pub(crate) async fn close_session(conn: &Connection, handle: &zbus::zvariant::ObjectPath<'_>) {
    if let Err(e) = conn.call_method(Some(PORTAL_NAME), handle, Some("org.freedesktop.portal.Session"), "Close", &()).await {
        tracing::debug!("Closing portal session {} failed: {}", handle, e);
    }
}

/// The main window as an xdg-foreign handle ("wayland:<handle>") so the desktop
/// can attach its dialog to it. GNOME's dialog never names the app,
/// so the parent window is its only context. Empty when the window is hidden or
/// not a Wayland surface, or GDK doesn't answer within a second.
pub(crate) async fn parent_window(app: &tauri::AppHandle) -> String {
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

pub(crate) fn next_token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("clustercut_{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Call a portal method that answers through a Request object and wait for
/// its Response. Subscribes first, so a fast reply can't be missed.
pub(crate) async fn request<B>(conn: &Connection, interface: &str, method: &str, body: &B, token: &str) -> zbus::Result<(u32, HashMap<String, OwnedValue>)>
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
    conn.call_method(Some(PORTAL_NAME), PORTAL_PATH, Some(interface), method, body).await?;
    // No timeout: BindShortcuts can wait on the user for as long as the dialog is open.
    let response = responses
        .next()
        .await
        .ok_or_else(|| zbus::Error::Failure(format!("{method}: portal went away before responding")))??;
    response.body().deserialize()
}

/// The sender element of portal object paths: the unique bus name without
/// its leading `:` and with `.` replaced by `_` (`:1.42` becomes `1_42`).
fn sender_element(unique_name: &str) -> String {
    unique_name.trim_start_matches(':').replace('.', "_")
}

/// Object path the portal will use for the Request created with `token`.
/// Known in advance so the Response signal can be subscribed before calling.
fn request_path(unique_name: &str, token: &str) -> String {
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
    fn builds_portal_paths() {
        assert_eq!(request_path(":1.27312", "cc_1"), "/org/freedesktop/portal/desktop/request/1_27312/cc_1");
        assert_eq!(session_path(":1.93", "ccsession"), "/org/freedesktop/portal/desktop/session/1_93/ccsession");
    }
}
