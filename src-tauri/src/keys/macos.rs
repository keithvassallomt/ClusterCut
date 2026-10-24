//! Pressing keys on macOS with CGEvent. Needs the Accessibility permission
//! (System Settings > Privacy & Security > Accessibility); without it the
//! events are silently dropped. Waits for the shortcut's modifiers to go up
//! first, and finds the key for "c"/"x"/"v" in the current layout, since a
//! fixed ANSI keycode would press Cmd+J for Copy on Dvorak.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use core_foundation::base::{CFRelease, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};

use super::KeysError;

const RELEASE_TIMEOUT: Duration = Duration::from_millis(1500);
const RELEASE_POLL: Duration = Duration::from_millis(10);

// CGEventTypes.h / CGEventSource.h
const COMBINED_SESSION_STATE: i32 = 0;
const HID_SYSTEM_STATE: i32 = 1;
const HID_EVENT_TAP: u32 = 0;
const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_ALTERNATE: u64 = 0x0008_0000;
const FLAG_COMMAND: u64 = 0x0010_0000;

// UnicodeUtilities.h
const UC_KEY_ACTION_DISPLAY: u16 = 3;
const UC_KEY_TRANSLATE_NO_DEAD_KEYS: u32 = 1;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceCreate(state: i32) -> *mut c_void;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, keycode: u16, key_down: bool) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CGEventSourceFlagsState(state: i32) -> u64;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const u8,
        keycode: u16,
        action: u16,
        modifiers: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_length: usize,
        actual_length: *mut usize,
        chars: *mut u16,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
}

/// Whether ClusterCut may post key events. With `prompt`, macOS shows its
/// "would like to control this computer" alert the first time.
pub(super) fn accessibility_trusted(prompt: bool) -> bool {
    // SAFETY: the key is a CFString constant owned by the framework; the
    // dictionary lives until the call returns.
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::from(prompt).as_CFType())]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef() as *const c_void)
    }
}

/// The keycode that types `ch` in the current keyboard layout. Main thread
/// only: the input source APIs assert it on recent macOS.
pub(super) fn layout_keycode(ch: char) -> Option<u16> {
    // SAFETY: the input source is released before returning; the layout data
    // belongs to it (get rule) and is only read while it's alive.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return None;
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let found = if data.is_null() {
            None
        } else {
            let layout = CFDataGetBytePtr(data);
            let keyboard_type = u32::from(LMGetKbdType());
            (0u16..128).find(|&keycode| {
                let (mut dead_keys, mut length, mut chars) = (0u32, 0usize, [0u16; 4]);
                UCKeyTranslate(
                    layout,
                    keycode,
                    UC_KEY_ACTION_DISPLAY,
                    0,
                    keyboard_type,
                    UC_KEY_TRANSLATE_NO_DEAD_KEYS,
                    &mut dead_keys,
                    chars.len(),
                    &mut length,
                    chars.as_mut_ptr(),
                ) == 0
                    && length == 1
                    && u32::from(chars[0]) == u32::from(ch)
            })
        };
        CFRelease(source as *const c_void);
        found
    }
}

/// The ANSI keycode for `ch`, when the layout lookup isn't available.
pub(super) fn ansi_keycode(ch: char) -> u16 {
    match ch {
        'c' => 8,
        'x' => 7,
        _ => 9, // 'v'
    }
}

/// Post Cmd(+Shift)+`keycode` once the user has let go of the shortcut.
pub(super) fn press(keycode: u16, shift: bool) -> Result<(), KeysError> {
    let held = FLAG_SHIFT | FLAG_CONTROL | FLAG_ALTERNATE | FLAG_COMMAND;
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    // Combined, not HID: the HID state is the physical keyboard only, so keys
    // from RustDesk or other remote control looked released while still held
    // and TextEdit got Cmd+Option+C (Copy Style).
    // SAFETY: reads the current modifier state.
    while unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) } & held != 0 && Instant::now() < deadline {
        std::thread::sleep(RELEASE_POLL);
    }
    // SAFETY: as above.
    let still_held = unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) } & held;
    if still_held != 0 {
        let names = [(FLAG_COMMAND, "Cmd"), (FLAG_ALTERNATE, "Option"), (FLAG_CONTROL, "Ctrl"), (FLAG_SHIFT, "Shift")];
        let held_names: Vec<&str> = names.iter().filter(|(flag, _)| still_held & flag != 0).map(|(_, name)| *name).collect();
        tracing::warn!("Still held after {:?}: {}; pressing anyway", RELEASE_TIMEOUT, held_names.join("+"));
    }
    let flags = FLAG_COMMAND | if shift { FLAG_SHIFT } else { 0 };
    // SAFETY: each created object is checked and released after posting.
    unsafe {
        let source = CGEventSourceCreate(HID_SYSTEM_STATE);
        for key_down in [true, false] {
            let event = CGEventCreateKeyboardEvent(source, keycode, key_down);
            if event.is_null() {
                if !source.is_null() {
                    CFRelease(source as *const c_void);
                }
                return Err(KeysError::Failed("macOS couldn't create the key event".to_string()));
            }
            // Explicit flags: the event carries exactly these modifiers.
            CGEventSetFlags(event, flags);
            CGEventPost(HID_EVENT_TAP, event);
            CFRelease(event as *const c_void);
        }
        if !source.is_null() {
            CFRelease(source as *const c_void);
        }
    }
    Ok(())
}
