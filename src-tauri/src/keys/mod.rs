//! Pressing Copy, Cut or Paste in the focused app, for the one-step manual
//! shortcuts (issue #21): Copy & Send, Cut & Send and Receive & Paste.
//!
//! Callers name the action, never keys: each backend picks the chord, and a
//! terminal gets its own (Ctrl+C there interrupts the running program).

use std::fmt;

#[cfg(target_os = "linux")]
mod gnome;
#[cfg(target_os = "linux")]
pub(crate) mod kwin;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod remote_desktop;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Copy,
    Cut,
    Paste,
}

#[derive(Debug)]
pub(crate) enum KeysError {
    /// No way to press keys on this desktop yet.
    Unsupported,
    /// The user turned down the desktop's permission dialog.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Declined,
    /// GNOME, with an extension older than 4.2 (or none).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    UpdateExtension,
    /// macOS, until ClusterCut is allowed under Accessibility.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    NeedsAccessibility,
    Failed(String),
}

impl fmt::Display for KeysError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeysError::Unsupported => write!(f, "pressing keys isn't supported on this desktop"),
            KeysError::Declined => write!(f, "the user declined the desktop's permission dialog"),
            KeysError::UpdateExtension => write!(f, "pressing keys on GNOME needs ClusterCut GNOME extension 4.2 or newer"),
            KeysError::NeedsAccessibility => write!(f, "ClusterCut isn't allowed under Accessibility yet"),
            KeysError::Failed(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    C,
    X,
    V,
    Insert,
}

impl Key {
    /// xkb keysym name, as Hyprland and the Wayland portals take it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Key::C => "C",
            Key::X => "X",
            Key::V => "V",
            Key::Insert => "Insert",
        }
    }
}

/// `ctrl` means Cmd on macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub key: Key,
}

impl Chord {
    const fn ctrl(key: Key) -> Self {
        Self { ctrl: true, shift: false, key }
    }

    /// Modifiers in a fixed order, under the caller's names for CTRL and SHIFT.
    pub(crate) fn modifiers<'a>(&self, names: [&'a str; 2]) -> Vec<&'a str> {
        [self.ctrl, self.shift]
            .into_iter()
            .zip(names)
            .filter_map(|(on, name)| on.then_some(name))
            .collect()
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = self.modifiers(["Ctrl", "Shift"]);
        parts.push(self.key.name());
        write!(f, "{}", parts.join("+"))
    }
}

/// The chord for `action`. Terminals copy and paste on Ctrl+Shift+C/V
/// (Ctrl/Shift+Insert on Windows) and have nothing to cut, so Cut copies there.
pub(crate) fn chord(action: Action, terminal: bool) -> Chord {
    match (action, terminal) {
        (Action::Copy, false) => Chord::ctrl(Key::C),
        (Action::Cut, false) => Chord::ctrl(Key::X),
        (Action::Paste, false) => Chord::ctrl(Key::V),
        (Action::Copy | Action::Cut, true) if cfg!(windows) => Chord::ctrl(Key::Insert),
        (Action::Paste, true) if cfg!(windows) => Chord { ctrl: false, shift: true, key: Key::Insert },
        (Action::Copy | Action::Cut, true) => Chord { ctrl: true, shift: true, key: Key::C },
        (Action::Paste, true) => Chord { ctrl: true, shift: true, key: Key::V },
    }
}

/// Terminal app IDs, window classes and Windows executables, lowercase.
const TERMINALS: &[&str] = &[
    "alacritty",
    "alacritty.exe",
    "blackbox",
    "com.gexperts.tilix",
    "com.mitchellh.ghostty",
    "com.raggesilver.blackbox",
    "com.system76.cosmicterm",
    "conhost.exe",
    "contour",
    "cool-retro-term",
    "deepin-terminal",
    "dev.warp.warp",
    "foot",
    "footclient",
    "ghostty",
    "gnome-terminal",
    "gnome-terminal-server",
    "guake",
    "io.elementary.terminal",
    "kgx",
    "kitty",
    "konsole",
    "lxterminal",
    "mate-terminal",
    "mintty.exe",
    "openconsole.exe",
    "org.gnome.console",
    "org.gnome.ptyxis",
    "org.gnome.ptyxis.devel",
    "org.gnome.terminal",
    "org.kde.konsole",
    "org.kde.yakuake",
    "org.wezfurlong.wezterm",
    "ptyxis",
    "qterminal",
    "rio",
    "sakura",
    "st",
    "st-256color",
    "terminator",
    "terminology",
    "tilda",
    "tilix",
    "urxvt",
    "uxterm",
    "warp",
    "wezterm",
    "wezterm-gui",
    "wezterm-gui.exe",
    "windowsterminal.exe",
    "xfce4-terminal",
    "xterm",
    "yakuake",
];

pub(crate) fn is_terminal(app_id: &str) -> bool {
    TERMINALS.contains(&app_id.to_ascii_lowercase().as_str())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
// Each platform only ever constructs its own.
#[allow(dead_code)]
pub(crate) enum Backend {
    Hyprland,
    /// GNOME, through the ClusterCut extension.
    Gnome,
    /// KDE Plasma, or another desktop with the RemoteDesktop portal.
    RemoteDesktop,
    X11,
    Windows,
    MacOs,
    None,
}

async fn backend() -> Backend {
    #[cfg(target_os = "linux")]
    {
        if crate::hyprland::is_hyprland() {
            return Backend::Hyprland;
        }
        // GNOME has the RemoteDesktop portal too, but the extension needs no
        // dialog and shows no screen-sharing indicator.
        if crate::clipboard::is_wayland() && crate::clipboard::is_gnome() {
            return if gnome::available().await { Backend::Gnome } else { Backend::None };
        }
        if crate::clipboard::is_wayland() && remote_desktop::available().await {
            return Backend::RemoteDesktop;
        }
        if !crate::clipboard::is_wayland() && std::env::var_os("DISPLAY").is_some() {
            return Backend::X11;
        }
    }
    #[cfg(windows)]
    return Backend::Windows;
    #[cfg(target_os = "macos")]
    return Backend::MacOs;
    #[allow(unreachable_code)]
    Backend::None
}

/// How keys get pressed here, for the settings UI.
#[derive(serde::Serialize)]
pub(crate) struct KeyStatus {
    backend: Backend,
    /// `Some(false)` while the desktop's permission is still to be given.
    permission: Option<bool>,
    /// GNOME, but the extension can't press keys: older than 4.2, or missing.
    needs_extension_update: bool,
}

pub(crate) async fn status(app: &tauri::AppHandle) -> KeyStatus {
    let backend = backend().await;
    let permission = match backend {
        #[cfg(target_os = "linux")]
        Backend::RemoteDesktop => Some(remote_desktop::has_permission(app)),
        #[cfg(target_os = "macos")]
        Backend::MacOs => Some(macos::accessibility_trusted(false)),
        _ => None,
    };
    let _ = app;
    let needs_extension_update = matches!(no_backend_reason(backend), Some(KeysError::UpdateExtension));
    KeyStatus { backend, permission, needs_extension_update }
}

/// Why keys can't be pressed with `backend`, if they can't.
fn no_backend_reason(backend: Backend) -> Option<KeysError> {
    if backend != Backend::None {
        return None;
    }
    #[cfg(target_os = "linux")]
    if crate::clipboard::is_wayland() && crate::clipboard::is_gnome() {
        return Some(KeysError::UpdateExtension);
    }
    Some(KeysError::Unsupported)
}

/// Ask for the desktop's permission now rather than on first use.
pub(crate) async fn request_permission(app: &tauri::AppHandle) -> Result<(), KeysError> {
    match backend().await {
        #[cfg(target_os = "linux")]
        Backend::RemoteDesktop => remote_desktop::request_permission(app).await,
        // Shows macOS's alert pointing to System Settings; granting happens there.
        #[cfg(target_os = "macos")]
        Backend::MacOs => {
            macos::accessibility_trusted(true);
            Ok(())
        }
        _ => {
            let _ = app;
            Ok(())
        }
    }
}

/// Whether keys can be pressed here, so the one-step shortcuts can say why not
/// up front instead of half-running. Blocking.
pub(crate) fn check() -> Result<(), KeysError> {
    no_backend_reason(tauri::async_runtime::block_on(backend())).map_or(Ok(()), Err)
}

/// Press the keys for `action` in the focused app. Blocking: waits for the
/// user to let go of the shortcut where the desktop needs that.
pub(crate) fn perform(app: &tauri::AppHandle, action: Action) -> Result<(), KeysError> {
    match tauri::async_runtime::block_on(backend()) {
        #[cfg(target_os = "linux")]
        Backend::Hyprland => {
            let app_id = crate::hyprland::active_window_class();
            let chord = chord(action, app_id.as_deref().is_some_and(is_terminal));
            tracing::info!("Pressing {} ({:?}) in {}", chord, action, app_id.as_deref().unwrap_or("the focused window"));
            crate::hyprland::press(chord).map_err(|e| KeysError::Failed(format!("Hyprland: {e}")))
        }
        #[cfg(target_os = "linux")]
        Backend::Gnome => {
            let app_id = tauri::async_runtime::block_on(gnome::active_window());
            let chord = chord(action, app_id.as_deref().is_some_and(is_terminal));
            tracing::info!("Pressing {} ({:?}) in {} through the GNOME extension", chord, action, app_id.as_deref().unwrap_or("the focused window"));
            tauri::async_runtime::block_on(gnome::press(chord))
        }
        #[cfg(target_os = "linux")]
        Backend::RemoteDesktop => {
            let app_id = crate::clipboard::is_kde().then(|| kwin::active_window(app)).flatten();
            let chord = chord(action, app_id.as_deref().is_some_and(is_terminal));
            tracing::info!("Pressing {} ({:?}) in {} through the RemoteDesktop portal", chord, action, app_id.as_deref().unwrap_or("the focused window"));
            remote_desktop::press(app, chord)
        }
        #[cfg(target_os = "linux")]
        Backend::X11 => {
            let classes = x11::active_window_classes();
            let chord = chord(action, classes.iter().any(|class| is_terminal(class)));
            tracing::info!("Pressing {} ({:?}) in {} through XTEST", chord, action, classes.last().map_or("the focused window", String::as_str));
            x11::press(chord)
        }
        #[cfg(windows)]
        Backend::Windows => {
            let exe = windows::foreground_exe();
            let chord = chord(action, exe.as_deref().is_some_and(is_terminal));
            tracing::info!("Pressing {} ({:?}) in {}", chord, action, exe.as_deref().unwrap_or("the foreground window"));
            windows::press(chord)
        }
        // Cmd+C/X/V is safe in terminals too, so no terminal check.
        #[cfg(target_os = "macos")]
        Backend::MacOs => {
            if !macos::accessibility_trusted(true) {
                return Err(KeysError::NeedsAccessibility);
            }
            let chord = chord(action, false);
            let ch = match chord.key {
                Key::C => 'c',
                Key::X => 'x',
                _ => 'v',
            };
            let keycode = on_main_thread(app, move || macos::layout_keycode(ch)).flatten().unwrap_or_else(|| macos::ansi_keycode(ch));
            tracing::info!("Pressing Cmd+{} ({:?}, keycode {})", ch.to_ascii_uppercase(), action, keycode);
            macos::press(keycode, chord.shift)
        }
        backend => {
            let _ = (app, action);
            Err(no_backend_reason(backend).unwrap_or(KeysError::Unsupported))
        }
    }
}

/// Run `f` on the main thread and wait for its result.
#[cfg(target_os = "macos")]
fn on_main_thread<T: Send + 'static>(app: &tauri::AppHandle, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .ok()?;
    rx.recv_timeout(std::time::Duration::from_secs(2)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_get_the_standard_chords() {
        assert_eq!(chord(Action::Copy, false).to_string(), "Ctrl+C");
        assert_eq!(chord(Action::Cut, false).to_string(), "Ctrl+X");
        assert_eq!(chord(Action::Paste, false).to_string(), "Ctrl+V");
    }

    #[cfg(not(windows))]
    #[test]
    fn terminals_never_get_ctrl_c_or_ctrl_x() {
        assert_eq!(chord(Action::Copy, true).to_string(), "Ctrl+Shift+C");
        assert_eq!(chord(Action::Cut, true).to_string(), "Ctrl+Shift+C");
        assert_eq!(chord(Action::Paste, true).to_string(), "Ctrl+Shift+V");
    }

    #[test]
    fn recognises_terminals_by_app_id_or_class() {
        assert!(is_terminal("kitty"));
        assert!(is_terminal("org.gnome.Ptyxis"));
        assert!(is_terminal("WindowsTerminal.exe"));
        assert!(!is_terminal("firefox"));
        assert!(!is_terminal("code"));
    }
}
