//! Pressing keys through the ClusterCut GNOME extension (4.2+), which drives a
//! Clutter virtual keyboard inside the shell: no permission dialog and no
//! screen-sharing indicator. The extension waits for the shortcut's modifiers
//! to go up before pressing, presses only a fixed set of chords, and only
//! takes calls from the ClusterCut process.

use super::{Chord, Key, KeysError};
use crate::clipboard::dbus_clipboard::{DBUS_NAME, DBUS_PATH};

const KEYS_IFACE: &str = "app.clustercut.clustercut.Keys";

fn failed(e: zbus::Error) -> KeysError {
    KeysError::Failed(format!("GNOME extension: {e}"))
}

/// The focused window's app ID (or WM class). An error if the extension is
/// missing or older than 4.2.
async fn focused_app() -> zbus::Result<String> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(Some(DBUS_NAME), DBUS_PATH, Some(KEYS_IFACE), "FocusedApp", &())
        .await?
        .body()
        .deserialize()
}

/// Whether the installed extension can press keys.
pub(super) async fn available() -> bool {
    focused_app().await.is_ok()
}

pub(super) async fn active_window() -> Option<String> {
    focused_app().await.ok().filter(|id| !id.is_empty())
}

/// The extension's name for `chord`; it presses nothing else.
fn chord_name(chord: Chord) -> Option<&'static str> {
    match (chord.ctrl, chord.shift, chord.key) {
        (true, false, Key::C) => Some("ctrl+c"),
        (true, false, Key::X) => Some("ctrl+x"),
        (true, false, Key::V) => Some("ctrl+v"),
        (true, true, Key::C) => Some("ctrl+shift+c"),
        (true, true, Key::V) => Some("ctrl+shift+v"),
        _ => None,
    }
}

pub(super) async fn press(chord: Chord) -> Result<(), KeysError> {
    let name = chord_name(chord).ok_or_else(|| KeysError::Failed(format!("the GNOME extension doesn't press {chord}")))?;
    let conn = zbus::Connection::session().await.map_err(failed)?;
    // Can take a moment: the extension waits for the shortcut's keys to go up.
    let pressed: bool = conn
        .call_method(Some(DBUS_NAME), DBUS_PATH, Some(KEYS_IFACE), "Press", &(name,))
        .await
        .and_then(|reply| reply.body().deserialize())
        .map_err(failed)?;
    if pressed {
        Ok(())
    } else {
        Err(KeysError::Failed("the GNOME extension didn't press the keys".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{chord, Action};

    #[test]
    fn every_linux_chord_has_an_extension_name() {
        for action in [Action::Copy, Action::Cut, Action::Paste] {
            for terminal in [false, true] {
                assert!(chord_name(chord(action, terminal)).is_some(), "{action:?} terminal={terminal}");
            }
        }
    }
}
