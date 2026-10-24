//! Pressing keys through the RemoteDesktop portal: KDE Plasma, and any other
//! desktop with the portal that isn't Hyprland or GNOME. One session per key
//! press, closed straight after. The first one shows the desktop's permission
//! dialog; KDE ticks "Allow restoring on future sessions" by default, and the
//! restore token that comes back skips the dialog from then on.
//!
//! KDE merges a physically held modifier into injected keys (a held Alt turns
//! Ctrl+C into Ctrl+Alt+C), ignores injected releases for keys it didn't
//! inject, and doesn't report modifier state. So: wait for the shortcut's
//! Deactivated (its trigger key going up), give the modifiers a moment, press,
//! and if the chord came back as one of our own shortcuts (KGlobalAccel caught
//! the Ctrl+Alt+C, so the app never saw it), wait and press again.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tauri::{Emitter, Manager};
use tokio::sync::{Mutex, OnceCell};
use zbus::zvariant::{OwnedObjectPath, Value};
use zbus::Connection;

use super::{Chord, Key, KeysError};
use crate::portal::{self, next_token, parent_window, session_path, PORTAL_NAME, PORTAL_PATH};
use crate::portal_shortcuts;
use crate::state::AppState;

const REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";
/// Device type bit, in `types` and `AvailableDeviceTypes`.
const KEYBOARD: u32 = 1;
/// `persist_mode`: until the user revokes it.
const PERSIST_UNTIL_REVOKED: u32 = 2;

/// How long the user gets to let go of the shortcut.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(1);
/// After the trigger key goes up, for the modifiers to follow.
const MODIFIER_GRACE: Duration = Duration::from_millis(80);
/// KWin re-fires our shortcut within ~15 ms of a leaked chord.
const ECHO_WINDOW: Duration = Duration::from_millis(150);
/// How long to keep pressing again while the modifiers are still held. Polled:
/// an echo's own Deactivated comes at once (the injected key goes straight
/// back up), so it says nothing about the physical keys.
const HELD_DEADLINE: Duration = Duration::from_millis(2500);
const RETRY_PAUSE: Duration = Duration::from_millis(200);

const CONTROL_L: i32 = 0xffe3;
const SHIFT_L: i32 = 0xffe1;

/// KWin types the exact keysym it's given, adjusting the modifiers to get it:
/// a lowercase `c` comes out unshifted even with our Shift down, so a
/// terminal would get Ctrl+C (SIGINT) instead of Ctrl+Shift+C. With Shift,
/// send the uppercase letter.
fn keysym(key: Key, shift: bool) -> i32 {
    let letter = |lower: u8| i32::from(if shift { lower.to_ascii_uppercase() } else { lower });
    match key {
        Key::C => letter(b'c'),
        Key::X => letter(b'x'),
        Key::V => letter(b'v'),
        Key::Insert => 0xff63,
    }
}

fn failed(e: zbus::Error) -> KeysError {
    KeysError::Failed(format!("RemoteDesktop portal: {e}"))
}

/// Identified as ClusterCut (`portal::connect_as_app`), so the desktop
/// remembers the permission for the app. Kept for the app's lifetime.
async fn conn() -> zbus::Result<Connection> {
    static CONN: OnceLock<Mutex<Option<Connection>>> = OnceLock::new();
    let mut guard = CONN.get_or_init(Default::default).lock().await;
    if let Some(conn) = guard.as_ref() {
        return Ok(conn.clone());
    }
    let conn = portal::connect_as_app().await?;
    *guard = Some(conn.clone());
    Ok(conn)
}

/// Whether the desktop's portal can press keys. Probed once.
pub(super) async fn available() -> bool {
    static AVAILABLE: OnceCell<bool> = OnceCell::const_new();
    *AVAILABLE
        .get_or_init(|| async {
            let probe = async {
                let conn = conn().await?;
                portal::version(&conn, REMOTE_DESKTOP).await?;
                portal::property_u32(&conn, REMOTE_DESKTOP, "AvailableDeviceTypes").await
            };
            match probe.await {
                Ok(types) => types & KEYBOARD != 0,
                Err(e) => {
                    tracing::info!("No RemoteDesktop portal, so ClusterCut can't press keys: {}", e);
                    false
                }
            }
        })
        .await
}

/// Whether the user has allowed it for good (a restore token is stored).
pub(super) fn has_permission(app: &tauri::AppHandle) -> bool {
    app.state::<AppState>().settings.lock().unwrap().remote_desktop_token.is_some()
}

/// Ask for the permission now (the Settings button): a session with no key presses.
pub(super) async fn request_permission(app: &tauri::AppHandle) -> Result<(), KeysError> {
    let (conn, handle) = open(app, Dialog::OnWindow).await?;
    portal::close_session(&conn, &handle).await;
    Ok(())
}

/// Press `chord` in the focused window, once the user has let go of the shortcut.
pub(super) fn press(app: &tauri::AppHandle, chord: Chord) -> Result<(), KeysError> {
    let (conn, handle) = tauri::async_runtime::block_on(open(app, Dialog::Unattached))?;
    let result = press_when_released(&conn, &handle, chord);
    tauri::async_runtime::block_on(portal::close_session(&conn, &handle));
    result
}

fn press_when_released(conn: &Connection, handle: &OwnedObjectPath, chord: Chord) -> Result<(), KeysError> {
    if !portal_shortcuts::wait_until_released(RELEASE_TIMEOUT) {
        tracing::debug!("Shortcut still held after {:?}; pressing anyway", RELEASE_TIMEOUT);
    }
    std::thread::sleep(MODIFIER_GRACE);
    let deadline = Instant::now() + HELD_DEADLINE;
    loop {
        let since = portal_shortcuts::activation_count();
        tauri::async_runtime::block_on(send_chord(conn, handle, chord)).map_err(failed)?;
        if !portal_shortcuts::wait_for_activation_after(since, ECHO_WINDOW) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(KeysError::Failed(format!("the shortcut's modifiers were still held after {HELD_DEADLINE:?}")));
        }
        tracing::info!("{} came back as a ClusterCut shortcut, so modifiers are still held; pressing again", chord);
        portal_shortcuts::wait_until_released(RELEASE_TIMEOUT);
        std::thread::sleep(RETRY_PAUSE);
    }
}

async fn send_chord(conn: &Connection, handle: &OwnedObjectPath, chord: Chord) -> zbus::Result<()> {
    let mut keys = Vec::new();
    if chord.ctrl {
        keys.push(CONTROL_L);
    }
    if chord.shift {
        keys.push(SHIFT_L);
    }
    keys.push(keysym(chord.key, chord.shift));
    for &key in &keys {
        send_key(conn, handle, key, true).await?;
    }
    for &key in keys.iter().rev() {
        send_key(conn, handle, key, false).await?;
    }
    Ok(())
}

async fn send_key(conn: &Connection, handle: &OwnedObjectPath, keysym: i32, pressed: bool) -> zbus::Result<()> {
    let options: HashMap<&str, Value> = HashMap::new();
    conn.call_method(Some(PORTAL_NAME), PORTAL_PATH, Some(REMOTE_DESKTOP), "NotifyKeyboardKeysym", &(handle, options, keysym, u32::from(pressed)))
        .await
        .map(|_| ())
}

/// Where the desktop's permission dialog goes.
#[derive(Clone, Copy)]
enum Dialog {
    /// On ClusterCut's window, when the user asked from it.
    OnWindow,
    /// On its own, when a shortcut asked: attached, KDE raises ClusterCut's
    /// window with it and the keys then land there instead of in the user's app.
    Unattached,
}

/// A started session that may send keyboard events. The desktop's dialog
/// shows here unless the stored restore token is still good.
async fn open(app: &tauri::AppHandle, dialog: Dialog) -> Result<(Connection, OwnedObjectPath), KeysError> {
    let conn = conn().await.map_err(failed)?;
    let unique_name = conn.unique_name().map(|n| n.to_string()).unwrap_or_default();
    let token = next_token();
    let session_token = format!("clustercut{}", next_token());
    let options = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("session_handle_token", Value::from(session_token.as_str())),
    ]);
    let (code, mut results) = portal::request(&conn, REMOTE_DESKTOP, "CreateSession", &(options,), &token).await.map_err(failed)?;
    if code != 0 {
        return Err(KeysError::Failed(format!("RemoteDesktop CreateSession response {code}")));
    }
    // Spec quirk: `session_handle` is a string, not an object path.
    let handle = results
        .remove("session_handle")
        .and_then(|v| String::try_from(v).ok())
        .unwrap_or_else(|| session_path(&unique_name, &session_token));
    let handle = OwnedObjectPath::try_from(handle).map_err(|e| failed(e.into()))?;
    match start(app, &conn, &handle, dialog).await {
        Ok(()) => Ok((conn, handle)),
        Err(e) => {
            portal::close_session(&conn, &handle).await;
            Err(e)
        }
    }
}

async fn start(app: &tauri::AppHandle, conn: &Connection, handle: &OwnedObjectPath, dialog: Dialog) -> Result<(), KeysError> {
    let restore_token = app.state::<AppState>().settings.lock().unwrap().remote_desktop_token.clone();
    let token = next_token();
    let mut options = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("types", Value::from(KEYBOARD)),
        ("persist_mode", Value::from(PERSIST_UNTIL_REVOKED)),
    ]);
    if let Some(restore) = restore_token.as_deref() {
        options.insert("restore_token", Value::from(restore));
    }
    let (code, _) = portal::request(conn, REMOTE_DESKTOP, "SelectDevices", &(handle, options), &token).await.map_err(failed)?;
    if code != 0 {
        return Err(KeysError::Failed(format!("RemoteDesktop SelectDevices response {code}")));
    }

    let parent = match dialog {
        Dialog::OnWindow => parent_window(app).await,
        Dialog::Unattached => String::new(),
    };
    let token = next_token();
    let options = HashMap::from([("handle_token", Value::from(token.as_str()))]);
    // No timeout: without a usable restore token, the desktop asks the user first.
    let (code, mut results) = portal::request(conn, REMOTE_DESKTOP, "Start", &(handle, parent.as_str(), options), &token).await.map_err(failed)?;
    match code {
        0 => {}
        1 => {
            tracing::info!("RemoteDesktop permission declined");
            return Err(KeysError::Declined);
        }
        _ => return Err(KeysError::Failed(format!("RemoteDesktop Start response {code}"))),
    }
    let devices = results.remove("devices").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
    // None when the user unticked "Allow restoring": the dialog shows again next time.
    save_token(app, results.remove("restore_token").and_then(|v| String::try_from(v).ok()));
    if devices & KEYBOARD == 0 {
        tracing::info!("RemoteDesktop session started without the keyboard");
        return Err(KeysError::Declined);
    }
    Ok(())
}

fn save_token(app: &tauri::AppHandle, token: Option<String>) {
    let state = app.state::<AppState>();
    let mut settings = state.settings.lock().unwrap();
    if settings.remote_desktop_token == token {
        return;
    }
    let permission_changed = settings.remote_desktop_token.is_some() != token.is_some();
    settings.remote_desktop_token = token;
    crate::storage::save_settings(app, &settings);
    drop(settings);
    if permission_changed {
        let _ = app.emit("shortcut-status-changed", ());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_chords_send_uppercase_keysyms() {
        assert_eq!(keysym(Key::C, false), 0x63);
        assert_eq!(keysym(Key::C, true), 0x43);
        assert_eq!(keysym(Key::V, true), 0x56);
        assert_eq!(keysym(Key::Insert, true), 0xff63);
    }
}
