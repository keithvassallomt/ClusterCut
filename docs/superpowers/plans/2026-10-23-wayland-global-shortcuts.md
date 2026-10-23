# Wayland Global Shortcuts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the manual Send/Receive global shortcuts work on Wayland. Today they are registered through `tauri-plugin-global-shortcut`, whose `global-hotkey` backend is X11-only (still true in `global-hotkey` 0.9.0 and plugin 3.0.0-alpha.3). In the Flatpak there is no X server at all; in native builds the XWayland grab only fires while an X11 window has focus.

**Architecture:** Three routes, chosen at runtime.

| Session | Route |
|---|---|
| X11, Windows, macOS | Unchanged: `tauri-plugin-global-shortcut` with the in-app recorder |
| Wayland with the GlobalShortcuts portal (GNOME 48+, KDE Plasma 5.27+, Hyprland) | `portal_shortcuts` module talking to `org.freedesktop.portal.GlobalShortcuts` over zbus; on Hyprland, optional runtime binds over its IPC (`hyprland` module) |
| Wayland without the portal (GNOME 47 and older, Sway/wlroots, COSMIC) | `ManualSend` / `ManualReceive` D-Bus methods the user binds in their compositor |

**Tech Stack:** Rust (Tauri 2 backend), zbus 4 (already a dependency), React settings UI.

## Status (2026-10-23)

**Done and verified:** Tasks 1 to 7 and 5b. Verified end to end with the real app on local Hyprland 0.56, GNOME 51 and KDE Plasma 6.7.5 (Fedora 45 VMs). Keith checked the dialogs, settings UI, key caps and close button in both VMs. 152 Rust tests pass; `npm run build` passes.

**Fixed along the way (not in the original plan):**
- Native packages now ship `app.clustercut.clustercut.desktop` + SVG icon (prerequisite below). Committed to `main` as `2f6fce3`.
- D-Bus `ToggleAutoSend`/`ToggleAutoReceive` deadlocked the settings mutex (`update_tray_menu` relocked it). Committed to `main` as `0ef516b`.
- Tray and D-Bus auto-mode toggles never re-registered shortcuts (all platforms). On this branch.
- Close button unresponsive until minimise/maximise: a regression from `5455873` (v0.4.3), which removed the focus-time `set_resizable` toggle that had fixed it in `7c52c89`. Restored as false then true (stays resizable), skipped on Hyprland; `unminimize()` restored before `show()` on the tray Show item, tray click and D-Bus `ShowWindow`. Committed to `main` as `719374a`.
- Desktop-assigned keys render as key caps (`Kbd`) instead of raw strings like `Press <Control><Alt>c`. On this branch.
- Manual send (shortcut, D-Bus) showed an empty "Sent" entry in History and didn't record it: it emitted a raw `ClipboardPayload` on `clipboard-change`, but since `fd64f11` (2026-06-07) the UI reads `ClipboardPreview.text_preview`. Now goes through `record_and_emit` like the "Broadcast to Cluster" button. Affected X11 too. On this branch.

**Follow-ups (separate change, not this branch):**
- Manual send is text-only (shortcut, D-Bus, "Broadcast to Cluster"); automatic send also carries images, files and rich text. Read the clipboard the same way for manual send.
- No feedback when there's nothing to send ("No text in clipboard" only reaches the log). Show a notification.
- Ctrl+Alt+C doesn't copy the selection first; that's issue #21's copy-and-send, still shelved.

**Remaining:**
- [x] "Unavailable" path with `CLUSTERCUT_FORCE_NO_PORTAL=1`: status reported, and the exact `gdbus` command the UI shows reached `ManualSend`.
- [x] Keith checked the Hyprland switch on, the conflict hint (which surfaced the leftover-bind bug) and manual send with History. The config line with the switch off is built for every manual direction regardless of the switch (unit-tested formats); its rendering wasn't looked at.
- [ ] Optional, not done: `ubuntu2604 (GNOME)` VM (GNOME 50); a Flatpak build (sandboxed path: no `Registry.Register`, `--filesystem=xdg-run/hypr`).
- [ ] Untested on Windows/macOS: the tray toggles now call `register_shortcuts`, and tray Show now unminimizes first.
- [x] CHANGELOG (Task 9); committed to `main`.

## Global Constraints

- **No new crates.** The portal is plain D-Bus; zbus 4 directly (not `ashpd`). `gtk` 0.18 became a direct Linux dependency for 5b, but it was already in the tree (Cargo.lock gains one edge, no package).
- **X11, Windows and macOS behaviour unchanged**, apart from the toggle and tray fixes above. The Wayland route is gated on `clipboard::is_wayland()`, never on "portal present".
- **On Wayland the Tauri plugin registers nothing.** `register_shortcuts` still calls `unregister_all()` first.
- **Stable identifiers:** app ID `app.clustercut.clustercut`; shortcut IDs `manual-send`, `manual-receive`.
- **No version bumps.** CHANGELOG entries only.
- Branch `feat/wayland-global-shortcuts` (fast-forwarded onto `main` after `0ef516b`). Issue #21 is shelved.

## Design decisions (as built)

1. **Portal desktops (GNOME, KDE) lose the in-app recorder.** The portal takes a *preferred* trigger at first bind only; afterwards the desktop owns the key. The card shows the bound key as key caps and a "Change" button (`ConfigureShortcuts`, portal v2) that opens the desktop's shortcut settings.
2. **One session binds both actions**, while either direction is in manual mode; closed when both are automatic.
3. **A shortcut acts even if its direction is in auto mode** while the session is alive.
4. **Hyprland:** its portal ignores `preferred_trigger` and returns no trigger description. ClusterCut offers opt-in runtime binds over Hyprland's IPC (Task 6a), with the config line as the fallback.
5. **Declined dialog:** not re-asked on every settings save; only after manual mode is switched off and on, or via "Ask Again".

## Prerequisite (done): native launcher renamed to the app ID

`Registry.Register` fails with "App info not found" unless a `.desktop` file matching the app ID is installed, and without it the portal guesses the ID from the launching process (from a VS Code terminal it became `com.microsoft.VSCode`). deb and rpm now ship the Flatpak's `src-tauri/flatpak/app.clustercut.clustercut.desktop` and `.svg` icon; the bundler-generated `ClusterCut.desktop` (name hard-coded to `{productName}`) is rendered from `packaging/linux/ClusterCut-legacy.desktop` as a hidden alias. This also fixed the app icon never showing (only a 1024 px PNG was shipped, a size hicolor never searches). `just dev-setup` installs the launcher + icon for `tauri dev`.

---

### Task 1: Spike: portal behaviour with our app ID (DONE)

Python + Gio script in the session scratchpad (`portal_spike.py`).

| | Hyprland 0.56 (local) | GNOME 51 (Fedora 45 VM) | KDE Plasma 6.7.5 (Fedora 45 VM) |
|---|---|---|---|
| Portal / GlobalShortcuts version | xdp 1.22.1 / v1 | xdp 1.22.1 / v2 | xdp 1.22.1 / v2 |
| `Registry.Register` | OK once the launcher exists | OK | OK |
| Dialog | none | "Add Keyboard Shortcuts" | "Global Shortcuts Requested" |
| `preferred_trigger` honoured | no | yes | yes |
| `trigger_description` | `""` | `Press <Control><Alt>c` | `Ctrl+Alt+C` |
| `Activated` delivered | yes | yes | yes |
| Approval persists across restarts | n/a | yes | yes |

- [x] Script: `Register`, `CreateSession`, `BindShortcuts`, `ListShortcuts`, listen for `Activated`.
- [x] Hyprland: bound and activated via `hyprctl dispatch 'hl.dsp.global("app.clustercut.clustercut:manual-send")'` (0.56 Lua syntax).
- [x] GNOME and KDE VMs: bound, activated with real keys (`virsh send-key`), approval persisted.
- [x] App name/icon in dialogs: neither shows an app icon (by design). KDE names the app in its subtitle from the launcher's `Name` (confirmed: "ClusterCut wants to register..."). GNOME Settings hides the app-name heading outside search and relies on `parent_window` for context (hence 5b).

### Task 2: Extract the manual actions (DONE)

- [x] `shortcuts::manual_send` / `manual_receive`, shared by the X11 plugin handler, D-Bus and the portal.

### Task 3: D-Bus `ManualSend` / `ManualReceive` (DONE)

- [x] Methods on `app.clustercut.clustercut`, run via `spawn_blocking` (the GNOME clipboard bridge is a blocking zbus call). Smoke-tested on Hyprland.

### Task 4: `portal_shortcuts` pure helpers (DONE)

- [x] `Accelerator` (parse a stored Tauri accelerator into modifiers + xkb keysym), `to_xdg_trigger` (`CommandOrControl+Alt+C` to `CTRL+ALT+c`), `request_path` / `session_path`. 5 tests.

### Task 5: Portal session lifecycle (DONE)

Deviation: state lives in a module-level `tokio::sync::Mutex<Portal>`, not `AppState`.

- [x] `request()`: subscribes to `Request.Response` on the predicted path before calling.
- [x] Dedicated session-bus connection per session; `Registry.Register` first (skipped in Flatpak).
- [x] `PortalStatus`: `inactive` / `unavailable` / `declined` / `bound { version, triggers }` / `error`.
- [x] Listeners: `Activated` (dispatched via `spawn_blocking`), `ShortcutsChanged`, `NameOwnerChanged` for portal restarts.
- [x] `sync()` idempotent; `spawn_sync()` is a plain fn so `sync`'s future type isn't recursive. Every `sync` also passes the session state on to `hyprland::sync`.
- [x] `configure()` (`ConfigureShortcuts`) and `retry()`.

### Task 5b: Attach the desktop's dialog to ClusterCut's window (DONE)

- [x] `parent_window()` exports the GTK window via `gdk_wayland_window_export_handle` on the main thread (1 s timeout) and returns `wayland:<handle>`; used for `BindShortcuts` and `ConfigureShortcuts`. Never unexported: GTK reuses one export per window, and unexporting right after `ConfigureShortcuts` would race the desktop importing it.
- [x] Verified: handles on Hyprland, GNOME and KDE; Keith confirmed the dialogs attach in both VMs.

### Task 6: Route `register_shortcuts` (DONE)

- [x] On Linux + Wayland, `register_shortcuts` calls `portal_shortcuts::spawn_sync` and returns.
- [x] Tray and D-Bus auto-mode toggles now call `register_shortcuts` (after releasing the settings lock).

### Task 6a: Hyprland runtime binds, opt-in (DONE)

- [x] `hyprland.rs` talks to `$XDG_RUNTIME_DIR/hypr/$SIG/.socket.sock` directly. Lua configs: `eval hl.bind/hl.unbind` (0.56 refuses `keyword` with "can't work with non-legacy parsers"); legacy: `keyword bind/unbind`. Syntax = `eval` works and `~/.config/hypr/hyprland.lua` exists.
- [x] Converters + 3 tests; conflict check against `j/binds`; only directions in manual mode.
- [x] Setting `hyprland_bind_keys` (default off). Re-applied on `configreloaded>>` from `.socket2.sock` (verified: a reload wipes runtime binds); removed on exit. Flatpak gets `--filesystem=xdg-run/hypr`.
- [x] Verified locally with real key presses (`ydotool`): bind at start, press fired, reload re-applied, second press fired, quit removed the bind.
- [x] Leftover binds (Keith, 2026-10-23): a run killed without its exit hook (Ctrl+C in `tauri dev`, crash) left its binds behind, and the next start reported its own Ctrl+Alt+C as "already bound". Binds are now tagged with a description (`ClusterCut: Send clipboard to cluster` / `... Apply received clipboard`; Lua `{ description = ... }`, legacy `bindd`), which `j/binds` reports. Tagged binds are removed as leftovers on every sync (also when not binding, e.g. switch off or no portal session, so they can't keep swallowing the keys) and never count as conflicts. Verified: SIGKILL run A, run B removed both leftovers, bound fresh, a real press fired. (Keith also saw the shortcuts not fire on that second start; not reproduced here, where the leftover bind still reached the new session. Fresh binds on every start should cover it either way.) Note: a process backgrounded with `&` from a non-interactive shell ignores SIGINT, so test abrupt exits with SIGKILL.

### Task 7: Status command and settings UI (DONE)

Deviation: `get_shortcut_status` returns `{ backend: "native" | "portal", portal?: PortalStatus, hyprland?: HyprlandStatus }`.

- [x] Commands `get_shortcut_status`, `configure_portal_shortcuts`, `retry_portal_shortcuts`.
- [x] `GlobalShortcutField`: recorder on native, and on Hyprland with the switch on (plus conflict hint); config line on Hyprland with it off; key caps + "Change" on portal v2; "Ask Again" / "Try Again" / `gdbus` fallback for declined / error / unavailable.
- [x] "Bind Keys in Hyprland" switch; refresh on `shortcut-status-changed`.
- [x] Key caps: `parseTrigger` turns GNOME's `Press <Control><Alt>c` and KDE's `Ctrl+Alt+C` into `Kbd` caps; unrecognised text falls back to plain text.

### Task 8: Testing

**Local Hyprland:** all done (Tasks 3, 6, 6a, 5b smoke tests above).

**VMs** (libvirt `qemu:///system`, user `keith`, passwordless sudo; rpm built on the host and installed with `dnf reinstall`; app launched in the desktop session with the environment copied from `gnome-shell` / `plasmashell`):
- [x] `fedora45 (KDE)`: dialog attached, key caps, "Change" opens system settings, real Ctrl+Alt+C fired `manual-send`, close button fixed.
- [x] `fedora45 (GNOME)` (needs the ClusterCut extension copied into `~/.local/share/gnome-shell/extensions` + re-login): same, plus a re-bind without dialog after restart fired, and `manual_send` read the clipboard through the extension from the blocking thread. Quick Settings toggle no longer freezes. Note: GNOME portal shortcuts don't fire while the Activities overview is open (the portal grabs in normal mode only); GNOME opens the overview at login.
- [ ] `ubuntu2604 (GNOME)`: not run (optional).
- [x] `CLUSTERCUT_FORCE_NO_PORTAL=1` (debug builds only) makes `start()` report `unavailable`.
- [x] Checked: status `unavailable`, and the UI's `gdbus` command (taken from `GlobalShortcutField.tsx`) triggered `ManualSend`. The first attempt collided with Keith's own `tauri dev` instance (single-instance handed the launch to it and the test's `Quit` closed it); always check `pgrep -x clustercut` first.

### Task 9: Docs (DONE)

- [x] CHANGELOG `[Unreleased]`: Added (portal shortcuts, Hyprland opt-in binds, D-Bus fallback); Fixed (tray/Quick Settings toggles now update shortcuts, manual send History entry). The close-button and deadlock entries went in with their own commits.
