//! Pressing keys on Windows with SendInput. Waits for the shortcut's modifiers
//! to go up first (GetAsyncKeyState), so a held Alt can't turn Ctrl+C into
//! Ctrl+Alt+C. Windows won't deliver input to an app running as administrator
//! unless ClusterCut is too (UIPI).

use std::time::{Duration, Instant};

use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_INSERT, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::{Chord, Key, KeysError};

const RELEASE_TIMEOUT: Duration = Duration::from_millis(1500);
const RELEASE_POLL: Duration = Duration::from_millis(10);

fn virtual_key(key: Key) -> VIRTUAL_KEY {
    // Letter virtual keys follow the active layout, so 'C' is C on AZERTY too.
    match key {
        Key::C => VIRTUAL_KEY(u16::from(b'C')),
        Key::X => VIRTUAL_KEY(u16::from(b'X')),
        Key::V => VIRTUAL_KEY(u16::from(b'V')),
        Key::Insert => VK_INSERT,
    }
}

/// The foreground window's executable ("WindowsTerminal.exe"), for spotting terminals.
pub(super) fn foreground_exe() -> Option<String> {
    // SAFETY: plain Win32 queries; the process handle is closed before returning.
    unsafe {
        let window = GetForegroundWindow();
        if window.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid as *mut u32));
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length);
        let _ = CloseHandle(process);
        queried.ok()?;
        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        path.rsplit('\\').next().map(str::to_string)
    }
}

fn modifiers_held() -> bool {
    [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
        .into_iter()
        // SAFETY: GetAsyncKeyState only reads key state.
        .any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } as u16 & 0x8000 != 0)
}

fn keyboard_input(key: VIRTUAL_KEY, up: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    // Without it, Insert is the numeric keypad's 0/Ins key.
    if key == VK_INSERT {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: key, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

/// Press `chord` in the foreground window.
pub(super) fn press(chord: Chord) -> Result<(), KeysError> {
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    while modifiers_held() && Instant::now() < deadline {
        std::thread::sleep(RELEASE_POLL);
    }
    let mut keys = Vec::new();
    if chord.ctrl {
        keys.push(VK_CONTROL);
    }
    if chord.shift {
        keys.push(VK_SHIFT);
    }
    keys.push(virtual_key(chord.key));
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&key| keyboard_input(key, false))
        .chain(keys.iter().rev().map(|&key| keyboard_input(key, true)))
        .collect();
    // SAFETY: `inputs` are fully initialised keyboard INPUTs of the size passed.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(KeysError::Failed(
            "Windows blocked the key presses (the app may be running as administrator)".to_string(),
        ))
    }
}
