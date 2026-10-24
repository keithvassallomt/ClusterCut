//! Hyprland: optionally bind the manual shortcuts live over Hyprland's IPC
//! socket. Hyprland's GlobalShortcuts portal registers ClusterCut's actions
//! but never assigns keys, so otherwise users must add binds to their config.
//! Runtime binds touch no files and are removed when ClusterCut exits.
//!
//! Also presses Copy/Cut/Paste for the one-step shortcuts (`keys`): Hyprland's
//! portal has no RemoteDesktop, but its `send_shortcut` dispatcher does it.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use tauri::{Emitter, Manager};
use tokio::io::AsyncBufReadExt;

use crate::keys::Chord;
use crate::portal::APP_ID;
use crate::portal_shortcuts::Accelerator;
use crate::shortcuts::ManualAction;
use crate::state::AppState;

/// How this Hyprland takes binds. Lua configs (0.56+) refuse `keyword` ("can't
/// work with non-legacy parsers"); older Hyprland has no `eval`.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Syntax {
    Lua,
    Legacy,
}

/// For the settings UI. Keys are formatted as a Lua config writes them
/// ("CTRL + ALT + C").
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct HyprlandStatus {
    pub syntax: Option<Syntax>,
    /// Shortcut ID to keys ClusterCut has bound.
    pub bound: HashMap<String, String>,
    /// Shortcut ID to keys left alone because something else already uses them.
    pub conflicts: HashMap<String, String>,
    /// Shortcut ID to the config line for users who'd rather own the bind.
    pub config_lines: HashMap<String, String>,
}

#[derive(Default)]
struct Hyprland {
    /// Exactly what we bound, so unbinding never touches the user's own binds.
    binds: Vec<Accelerator>,
    syntax: Option<Syntax>,
    /// Whether the portal session exists; kept for re-applying after a reload.
    session_active: bool,
    reload_listener: Option<tauri::async_runtime::JoinHandle<()>>,
    status: HyprlandStatus,
}

fn hyprland() -> &'static Mutex<Hyprland> {
    static HYPRLAND: OnceLock<Mutex<Hyprland>> = OnceLock::new();
    HYPRLAND.get_or_init(Default::default)
}

/// `$XDG_RUNTIME_DIR/hypr/<instance>`, if we're running under Hyprland.
fn socket_dir() -> Option<PathBuf> {
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime).join("hypr").join(instance))
}

pub(crate) fn is_hyprland() -> bool {
    socket_dir().is_some()
}

/// One request on Hyprland's command socket, the way `hyprctl` sends it.
fn request(command: &str) -> std::io::Result<String> {
    let dir = socket_dir().ok_or_else(|| std::io::Error::other("not running under Hyprland"))?;
    let mut stream = UnixStream::connect(dir.join(".socket.sock"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    stream.write_all(command.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply)
}

/// Lua when this Hyprland has `eval` and the user's config is Lua.
fn detect_syntax() -> Syntax {
    let has_eval = request("eval return 1").is_ok_and(|reply| reply.trim() == "ok");
    let lua_config = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config/hypr/hyprland.lua"))
        .is_some_and(|path| path.exists());
    if has_eval && lua_config {
        Syntax::Lua
    } else {
        Syntax::Legacy
    }
}

fn hypr_key(accel: &Accelerator) -> String {
    if accel.key.chars().count() == 1 {
        accel.key.to_ascii_uppercase()
    } else {
        accel.key.clone()
    }
}

/// "CTRL + ALT + C"
fn lua_keys(accel: &Accelerator) -> String {
    let key = hypr_key(accel);
    let mut parts = accel.modifiers(["CTRL", "ALT", "SHIFT", "SUPER"]);
    parts.push(&key);
    parts.join(" + ")
}

/// ("CTRL ALT", "C"), the first two fields of a hyprlang `bind`.
fn legacy_keys(accel: &Accelerator) -> (String, String) {
    (accel.modifiers(["CTRL", "ALT", "SHIFT", "SUPER"]).join(" "), hypr_key(accel))
}

/// Hyprland's modifier bitmask, as `j/binds` reports it.
fn modmask(accel: &Accelerator) -> u64 {
    [(accel.shift, 1), (accel.ctrl, 4), (accel.alt, 8), (accel.logo, 64)]
        .into_iter()
        .filter_map(|(on, bit)| on.then_some(bit))
        .sum()
}

/// Tags ClusterCut's binds in `j/binds`, so binds left behind by a run that
/// never reached its exit hook (killed, crashed, Ctrl+C in `tauri dev`) are
/// recognised as ours rather than reported as conflicts.
const DESCRIPTION_PREFIX: &str = "ClusterCut: ";

fn description(id: &str) -> String {
    let action = ManualAction::from_id(id).map_or(id, |action| action.description());
    format!("{DESCRIPTION_PREFIX}{action}")
}

fn bind_command(syntax: Syntax, accel: &Accelerator, id: &str) -> String {
    match syntax {
        Syntax::Lua => format!(
            "eval hl.bind(\"{}\", hl.dsp.global(\"{APP_ID}:{id}\"), {{ description = \"{}\" }})",
            lua_keys(accel),
            description(id)
        ),
        Syntax::Legacy => {
            let (mods, key) = legacy_keys(accel);
            format!("keyword bindd {mods}, {key}, {}, global, {APP_ID}:{id}", description(id))
        }
    }
}

/// The accelerator for a bind as `j/binds` reports it (modmask + key).
fn accelerator_from_bind(modmask: u64, key: &str) -> Accelerator {
    Accelerator {
        ctrl: modmask & 4 != 0,
        alt: modmask & 8 != 0,
        shift: modmask & 1 != 0,
        logo: modmask & 64 != 0,
        key: key.to_string(),
    }
}

fn unbind_command(syntax: Syntax, accel: &Accelerator) -> String {
    match syntax {
        Syntax::Lua => format!("eval hl.unbind(\"{}\")", lua_keys(accel)),
        Syntax::Legacy => {
            let (mods, key) = legacy_keys(accel);
            format!("keyword unbind {mods}, {key}")
        }
    }
}

fn config_line(syntax: Syntax, accel: &Accelerator, id: &str) -> String {
    match syntax {
        Syntax::Lua => format!("hl.bind(\"{}\", hl.dsp.global(\"{APP_ID}:{id}\"))", lua_keys(accel)),
        Syntax::Legacy => {
            let (mods, key) = legacy_keys(accel);
            format!("bind = {mods}, {key}, global, {APP_ID}:{id}")
        }
    }
}

struct ExistingBind {
    modmask: u64,
    key: String,
    /// Tagged with our description: ClusterCut made it.
    ours: bool,
}

/// Every bind outside submaps.
fn existing_binds() -> Vec<ExistingBind> {
    let Ok(reply) = request("j/binds") else {
        return Vec::new();
    };
    let binds: Vec<serde_json::Value> = serde_json::from_str(&reply).unwrap_or_default();
    binds
        .iter()
        .filter(|b| b["submap"].as_str().unwrap_or("").is_empty())
        .filter_map(|b| {
            Some(ExistingBind {
                modmask: b["modmask"].as_u64()?,
                key: b["key"].as_str()?.to_string(),
                ours: b["description"].as_str().unwrap_or("").starts_with(DESCRIPTION_PREFIX),
            })
        })
        .collect()
}

/// The accelerators ClusterCut wants per shortcut ID: only for directions in
/// manual mode, matching the X11 behaviour.
fn wanted(app: &tauri::AppHandle) -> (bool, Vec<(&'static str, Option<Accelerator>)>) {
    let state = app.state::<AppState>();
    let s = state.settings.lock().unwrap();
    let wanted = ManualAction::ALL
        .into_iter()
        .map(|action| {
            let accel = action.is_manual(&s).then(|| action.accelerator(&s)).flatten().and_then(Accelerator::parse);
            (action.id(), accel)
        })
        .collect();
    (s.hyprland_bind_keys, wanted)
}

fn unbind_all(state: &mut Hyprland) {
    if let Some(syntax) = state.syntax {
        for accel in state.binds.drain(..) {
            if let Err(e) = request(&unbind_command(syntax, &accel)) {
                tracing::debug!("Hyprland unbind failed: {}", e);
            }
        }
    }
    state.binds.clear();
    state.status.bound.clear();
}

/// Make ClusterCut's runtime binds match the settings: present while the portal
/// session exists, the user opted in, and that direction is in manual mode.
pub(crate) fn sync(app: &tauri::AppHandle, session_active: bool) {
    if !is_hyprland() {
        return;
    }
    let (enabled, wanted) = wanted(app);
    let mut state = hyprland().lock().unwrap();
    state.session_active = session_active;
    let syntax = *state.syntax.get_or_insert_with(detect_syntax);

    // Start from nothing every time: key changes, toggles and reloads share this path.
    unbind_all(&mut state);
    state.status.conflicts.clear();
    state.status.config_lines = wanted
        .iter()
        .filter_map(|(id, accel)| Some((id.to_string(), config_line(syntax, accel.as_ref()?, id))))
        .collect();

    // Always, even when not binding now: left behind, they'd keep swallowing the keys.
    let existing = existing_binds();
    for leftover in existing.iter().filter(|b| b.ours) {
        let accel = accelerator_from_bind(leftover.modmask, &leftover.key);
        tracing::info!("Removing ClusterCut's leftover Hyprland bind {} from an earlier run", lua_keys(&accel));
        if let Err(e) = request(&unbind_command(syntax, &accel)) {
            tracing::debug!("Hyprland unbind failed: {}", e);
        }
    }

    if session_active && enabled {
        for (id, accel) in wanted {
            let Some(accel) = accel else { continue };
            let keys = lua_keys(&accel);
            let key = hypr_key(&accel);
            if existing.iter().any(|b| !b.ours && b.modmask == modmask(&accel) && b.key.eq_ignore_ascii_case(&key)) {
                tracing::warn!("Not binding {} in Hyprland: {} is already bound", id, keys);
                state.status.conflicts.insert(id.to_string(), keys);
                continue;
            }
            match request(&bind_command(syntax, &accel, id)) {
                Ok(reply) if reply.trim() == "ok" => {
                    tracing::info!("Bound {} to {} in Hyprland", id, keys);
                    state.status.bound.insert(id.to_string(), keys);
                    state.binds.push(accel);
                }
                Ok(reply) => tracing::warn!("Hyprland refused bind for {}: {}", id, reply.trim()),
                Err(e) => tracing::warn!("Hyprland IPC failed binding {}: {}", id, e),
            }
        }
        if state.reload_listener.is_none() {
            state.reload_listener = Some(spawn_reload_listener(app));
        }
    } else if let Some(listener) = state.reload_listener.take() {
        listener.abort();
    }
    state.status.syntax = Some(syntax);
    drop(state);
    let _ = app.emit("shortcut-status-changed", ());
}

/// Hyprland drops runtime binds whenever it reloads its config, which it does
/// on every save of the config file. Re-apply when its event socket says so.
fn spawn_reload_listener(app: &tauri::AppHandle) -> tauri::async_runtime::JoinHandle<()> {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(path) = socket_dir().map(|dir| dir.join(".socket2.sock")) else {
            return;
        };
        let stream = match tokio::net::UnixStream::connect(&path).await {
            Ok(stream) => stream,
            Err(e) => {
                tracing::warn!("Can't watch Hyprland events, binds won't survive config reloads: {}", e);
                return;
            }
        };
        let mut lines = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.starts_with("configreloaded>>") {
                tracing::info!("Hyprland reloaded its config; re-applying binds");
                let app = app.clone();
                tauri::async_runtime::spawn_blocking(move || reapply_after_reload(&app));
            }
        }
    })
}

fn reapply_after_reload(app: &tauri::AppHandle) {
    let session_active = {
        let mut state = hyprland().lock().unwrap();
        // The reload already removed ours; unbinding again could hit the user's.
        state.binds.clear();
        state.status.bound.clear();
        state.session_active
    };
    sync(app, session_active);
}

pub(crate) fn status() -> Option<HyprlandStatus> {
    is_hyprland().then(|| hyprland().lock().unwrap().status.clone())
}

/// The focused window's class, for spotting terminals.
pub(crate) fn active_window_class() -> Option<String> {
    let reply = request("j/activewindow").ok()?;
    let window: serde_json::Value = serde_json::from_str(&reply).ok()?;
    window["class"].as_str().filter(|class| !class.is_empty()).map(str::to_string)
}

/// Press `chord` in the focused window. Hyprland sends exactly these
/// modifiers, even while the user still holds the shortcut's own (Ctrl+Alt).
pub(crate) fn press(chord: Chord) -> std::io::Result<()> {
    let syntax = *hyprland().lock().unwrap().syntax.get_or_insert_with(detect_syntax);
    let reply = request(&press_command(syntax, chord))?;
    if reply.trim() == "ok" {
        Ok(())
    } else {
        Err(std::io::Error::other(reply.trim().to_string()))
    }
}

fn press_command(syntax: Syntax, chord: Chord) -> String {
    let mods = chord.modifiers(["CTRL", "SHIFT"]).join(" ");
    let key = chord.key.name();
    match syntax {
        Syntax::Lua => format!("dispatch hl.dsp.send_shortcut({{ mods = \"{mods}\", key = \"{key}\" }})"),
        Syntax::Legacy => format!("dispatch sendshortcut {mods}, {key}"),
    }
}

/// Remove ClusterCut's runtime binds on exit; left behind they'd swallow the keys.
pub(crate) fn unbind_on_exit() {
    if let Ok(mut state) = hyprland().lock() {
        unbind_all(&mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accel(s: &str) -> Accelerator {
        Accelerator::parse(s).unwrap()
    }

    #[test]
    fn formats_lua_binds() {
        let a = accel("CommandOrControl+Alt+C");
        assert_eq!(lua_keys(&a), "CTRL + ALT + C");
        assert_eq!(
            bind_command(Syntax::Lua, &a, ManualAction::CopySend.id()),
            "eval hl.bind(\"CTRL + ALT + C\", hl.dsp.global(\"app.clustercut.clustercut:copy-send\"), \
             { description = \"ClusterCut: Copy and send to cluster\" })"
        );
        assert_eq!(unbind_command(Syntax::Lua, &a), "eval hl.unbind(\"CTRL + ALT + C\")");
        assert_eq!(
            config_line(Syntax::Lua, &a, ManualAction::CopySend.id()),
            "hl.bind(\"CTRL + ALT + C\", hl.dsp.global(\"app.clustercut.clustercut:copy-send\"))"
        );
    }

    #[test]
    fn formats_legacy_binds() {
        let a = accel("Super+Shift+Enter");
        assert_eq!(
            bind_command(Syntax::Legacy, &a, ManualAction::ReceiveOnly.id()),
            "keyword bindd SHIFT SUPER, Return, ClusterCut: Apply received clipboard, global, app.clustercut.clustercut:receive-only"
        );
        assert_eq!(unbind_command(Syntax::Legacy, &a), "keyword unbind SHIFT SUPER, Return");
        assert_eq!(
            config_line(Syntax::Legacy, &a, ManualAction::ReceiveOnly.id()),
            "bind = SHIFT SUPER, Return, global, app.clustercut.clustercut:receive-only"
        );
    }

    #[test]
    fn computes_modmask_like_hyprland() {
        assert_eq!(modmask(&accel("CommandOrControl+Alt+C")), 12);
        assert_eq!(modmask(&accel("Shift+Control+Alt+V")), 13);
        assert_eq!(modmask(&accel("Super+F1")), 64);
    }

    #[test]
    fn rebuilds_accelerator_from_reported_bind() {
        let a = accelerator_from_bind(12, "C");
        assert_eq!(lua_keys(&a), "CTRL + ALT + C");
        assert_eq!(modmask(&a), 12);
        assert_eq!(lua_keys(&accelerator_from_bind(65, "Return")), "SHIFT + SUPER + Return");
    }

    #[test]
    fn formats_key_presses() {
        let copy = crate::keys::chord(crate::keys::Action::Copy, false);
        assert_eq!(press_command(Syntax::Lua, copy), "dispatch hl.dsp.send_shortcut({ mods = \"CTRL\", key = \"C\" })");
        let terminal_paste = crate::keys::chord(crate::keys::Action::Paste, true);
        assert_eq!(press_command(Syntax::Legacy, terminal_paste), "dispatch sendshortcut CTRL SHIFT, V");
    }
}
