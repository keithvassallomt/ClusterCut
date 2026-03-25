# Wayland Clipboard Support — Three-Path Architecture

## Context

ClusterCut's clipboard stack (`tauri-plugin-clipboard` -> `clipboard-rs` -> `x11rb`) is X11-only. On Wayland, clipboard operations fail completely. Past attempts:
- `wl-clipboard-rs` directly: didn't work
- Repeated `wl-paste` subprocess invocations: caused input flickering (new Wayland client per poll)

GNOME/Mutter does not support `wlr-data-control` or `ext-data-control-v1`, so background clipboard monitoring on GNOME Wayland is only possible from inside the compositor (i.e., a Shell extension).

## Architecture

| Environment | Monitoring | Read/Write |
|---|---|---|
| **X11** (any desktop) | `tauri-plugin-clipboard` (unchanged) | `tauri-plugin-clipboard` (unchanged) |
| **Wayland + KDE/Sway/Hyprland** | `wl-clipboard-rs` watcher (`wlr-data-control`) | `wl-clipboard-rs` |
| **Wayland + GNOME** | GNOME extension clipboard bridge (D-Bus) | GNOME extension clipboard bridge (D-Bus) |

Detection order at startup:
1. Check `WAYLAND_DISPLAY` / `XDG_SESSION_TYPE` — if X11, use current plugin path
2. If Wayland, check if GNOME extension is available on D-Bus — if yes, use extension bridge
3. If Wayland without extension, try `wlr-data-control` via `wl-clipboard-rs`
4. If nothing works, fall back to plugin (degraded/non-functional) with warning

## Files modified/created

- `src-tauri/Cargo.toml` — add `wl-clipboard-rs` as Linux-only dep; make `tauri-plugin-clipboard` non-Linux-only
- `src-tauri/src/clipboard/mod.rs` — public API, detection logic, backend selection
- `src-tauri/src/clipboard/common.rs` — `ClipboardContent` enum, `IGNORED_CONTENT`, `broadcast_clipboard()`, helpers
- `src-tauri/src/clipboard/plugin.rs` — macOS/Windows backend (current `tauri-plugin-clipboard` code)
- `src-tauri/src/clipboard/wayland.rs` — `wl-clipboard-rs` backend for KDE/Sway/Hyprland
- `src-tauri/src/clipboard/dbus_clipboard.rs` — GNOME extension D-Bus clipboard bridge client
- `src-tauri/src/lib.rs` — conditionally skip `tauri_plugin_clipboard::init()` on Linux; update extension detection to strongly urge install on GNOME Wayland
- `src-tauri/src/dbus.rs` — add clipboard-related D-Bus methods/signals for extension communication
- `gnome-extension/extension.js` — add `St.Clipboard` monitoring + D-Bus clipboard methods
