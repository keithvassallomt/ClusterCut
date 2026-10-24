//! Pressing keys on X11 through the XTEST extension. Best effort: written but
//! not tested (Keith, 2026-10-23). Waits for the shortcut's modifiers to go up
//! first (polling `QueryKeymap`), so a held Alt can't turn Ctrl+C into
//! Ctrl+Alt+C, which the shortcut's own grab would then swallow.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, AtomEnum, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;

use super::{Chord, Key, KeysError};

const RELEASE_TIMEOUT: Duration = Duration::from_millis(1500);
const RELEASE_POLL: Duration = Duration::from_millis(10);

const XK_CONTROL_L: u32 = 0xffe3;
const XK_SHIFT_L: u32 = 0xffe1;

fn keysym(key: Key) -> u32 {
    match key {
        Key::C => 0x63,
        Key::X => 0x78,
        Key::V => 0x76,
        Key::Insert => 0xff63,
    }
}

fn failed(e: impl std::fmt::Display) -> KeysError {
    KeysError::Failed(format!("X11: {e}"))
}

/// The focused window's WM_CLASS parts (instance, then class), for spotting terminals.
pub(super) fn active_window_classes() -> Vec<String> {
    let Ok((conn, screen)) = x11rb::connect(None) else {
        return Vec::new();
    };
    let root = conn.setup().roots[screen].root;
    let property = |window, name: AtomEnum, atom_type: AtomEnum, length| {
        conn.get_property(false, window, name, atom_type, 0, length).ok()?.reply().ok()
    };
    let active = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.atom);
    let window = active
        .and_then(|atom| conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 1).ok()?.reply().ok())
        .and_then(|reply| reply.value32()?.next())
        .filter(|&window| window != x11rb::NONE);
    let Some(class) = window.and_then(|window| property(window, AtomEnum::WM_CLASS, AtomEnum::STRING, 256)) else {
        return Vec::new();
    };
    class
        .value
        .split(|&b| b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

/// Press `chord` in the focused window.
pub(super) fn press(chord: Chord) -> Result<(), KeysError> {
    let (conn, screen) = x11rb::connect(None).map_err(failed)?;
    conn.xtest_get_version(2, 2).map_err(failed)?.reply().map_err(failed)?;
    let root = conn.setup().roots[screen].root;

    // Keycodes from the current layout, so the right key is pressed on any of them.
    let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
    let mapping = conn.get_keyboard_mapping(min, max - min + 1).map_err(failed)?.reply().map_err(failed)?;
    let per_keycode = usize::from(mapping.keysyms_per_keycode).max(1);
    let keycode = |keysym: u32| {
        mapping
            .keysyms
            .chunks(per_keycode)
            .position(|keysyms| keysyms.contains(&keysym))
            .and_then(|index| u8::try_from(usize::from(min) + index).ok())
    };
    let mut keysyms = Vec::new();
    if chord.ctrl {
        keysyms.push(XK_CONTROL_L);
    }
    if chord.shift {
        keysyms.push(XK_SHIFT_L);
    }
    keysyms.push(keysym(chord.key));
    let codes = keysyms
        .into_iter()
        .map(keycode)
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(|| failed(format!("no key for {chord} in this keyboard layout")))?;

    wait_for_modifiers_released(&conn)?;
    for &code in &codes {
        conn.xtest_fake_input(xproto::KEY_PRESS_EVENT, code, x11rb::CURRENT_TIME, root, 0, 0, 0).map_err(failed)?;
    }
    for &code in codes.iter().rev() {
        conn.xtest_fake_input(xproto::KEY_RELEASE_EVENT, code, x11rb::CURRENT_TIME, root, 0, 0, 0).map_err(failed)?;
    }
    // A round trip, so the server has the presses before we report them done.
    conn.get_input_focus().map_err(failed)?.reply().map_err(failed)?;
    Ok(())
}

/// Until no modifier key is physically down, or the timeout passes (then press anyway).
fn wait_for_modifiers_released(conn: &impl Connection) -> Result<(), KeysError> {
    let modifier_codes: Vec<u8> = conn
        .get_modifier_mapping()
        .map_err(failed)?
        .reply()
        .map_err(failed)?
        .keycodes
        .into_iter()
        .filter(|&code| code != 0)
        .collect();
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    loop {
        let keys = conn.query_keymap().map_err(failed)?.reply().map_err(failed)?.keys;
        let held = modifier_codes.iter().any(|&code| keys[usize::from(code / 8)] & (1 << (code % 8)) != 0);
        if !held || Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(RELEASE_POLL);
    }
}
