# Issue #21: Copy & Send, Cut & Send, Receive & Paste, Keep My Clipboard

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** cut the manual-mode round trip from four key presses to two (issue #21, mtudor). The sender's shortcut copies the selection and sends it; the receiver's shortcut applies the pending content and pastes it. Plus a cut variant, the old "send what's on the clipboard" behaviour on its own combo, and an optional mode that leaves the local clipboard as it was.

**Status:** stage 1 (core + Hyprland) done and tested by Keith 2026-10-23. Stage 2 (KDE) done 2026-10-24, Keith tested on the VM. Stage 3 (GNOME) done 2026-10-24, Keith tested on the VM. Stages 4-6 (X11, Windows, macOS) written 2026-10-24: X11 untested by design, Windows and macOS awaiting Keith. X11 is best effort: written, not tested (Keith, 2026-10-23).

## Decisions (Keith, 2026-10-23)

- **Layout: the issue's.** Defaults, all `CommandOrControl` so macOS gets Cmd:

  | Shortcut | Default | Does |
  |---|---|---|
  | Copy & Send | Ctrl+Alt+C | presses Copy in the focused app, sends the result |
  | Cut & Send | Ctrl+Alt+X | presses Cut, sends the result |
  | Send Clipboard | Ctrl+Alt+Shift+C | today's Ctrl+Alt+C: sends what's already on the clipboard |
  | Receive & Paste | Ctrl+Alt+V | applies the pending content, presses Paste |
  | Receive Only | Ctrl+Alt+Shift+V | today's Ctrl+Alt+V: applies the pending content |

  Registered only for directions in manual mode, as today.
- **Nothing copied:** if Copy/Cut & Send presses the key and the clipboard doesn't change, notify "Nothing copied. Select something first." and send nothing. Never fall back to the old clipboard.
- **All platforms in this pass:** Windows and macOS backends get written now; Keith tests them.

## Spike results (2026-10-23)

Test: a zenity entry with known text, inject Ctrl+A then Ctrl+C, read the clipboard. A control on both desktops confirmed that a real Ctrl+Alt+C does **not** copy in that entry, so the test catches leaking modifiers. Physical keys were held via ydotool (local) and QEMU `send-key` (VM), with evdev timestamps to prove the hold overlapped the injection.

- **Hyprland 0.56 (local):** `hl.dsp.send_shortcut({ mods = "CTRL", key = "C" })` over the IPC socket works, and it **overrides held modifiers**: it copied with Ctrl+Alt physically down. Legacy config syntax: `dispatch sendshortcut CTRL, C, activewindow`. No Wayland protocol needed, and Flatpak already has `xdg-run/hypr`. Hyprland's portal has no RemoteDesktop.
- **KDE Plasma 6.7 (VM):** the RemoteDesktop portal (v2) works. The first `Start` shows a "Remote Control: Control input devices" dialog with **"Allow restoring on future sessions" ticked by default**; after that, `Start` with the restore token returns in 0.00 s with no dialog. Each `Start` hands back a fresh token to store.
  - A physically held **Alt leaks** into the injected keys: the app receives Ctrl+Alt+C and nothing is copied.
  - Injecting key releases for the modifiers first does **not** clear them.
  - The GlobalShortcuts `Deactivated` signal fires when the **C** key is released, not the modifiers. KWin exposes no modifier state over D-Bus.
  - The leaked Ctrl+Alt+C **re-fires ClusterCut's own shortcut** (`Activated` 15 ms after the injection). That's a usable "modifiers still held" signal whenever the leftover modifiers plus the injected Ctrl form one of our shortcuts, which is exactly the default case.
  - KDE exposes no `zwp_virtual_keyboard_manager_v1` to ordinary clients.
- **GNOME 51 (VM):** RemoteDesktop portal v2 is present but unused: the extension is already required on GNOME and can do it in-shell with no dialog and no screen-sharing indicator. Not spiked yet.

## Design

### Key injection: `src-tauri/src/keys/`

`perform(app, Action) -> Result<Performed, KeysError>` with `Action::{Copy, Cut, Paste}`. Callers never name keys; each backend picks the chord. That keeps the GNOME extension's D-Bus surface tiny (see Security).

| Platform | Backend | Waiting out held modifiers | Focused-app check (terminals) |
|---|---|---|---|
| Hyprland | IPC `send_shortcut` | not needed (overrides) | IPC `j/activewindow` class |
| KDE | RemoteDesktop portal, `NotifyKeyboardKeysym`, session per use, restore token in settings | after `Deactivated`, short delay; if our shortcut echoes, wait for its release and retry (cap ~2 s) | KWin script (loaded once via `org.kde.kwin.Scripting`) that reports `workspace.activeWindow.resourceClass` to ClusterCut over D-Bus |
| GNOME | extension `Keys.Perform(s action)`, Clutter virtual keyboard | extension polls `global.get_pointer()` modifier mask | extension: `global.display.focus_window` app id / WM class |
| X11 | XTest via x11rb (`xtest` feature), keysym to keycode via the keyboard mapping | poll `QueryKeymap` for modifier keycodes | EWMH `_NET_ACTIVE_WINDOW` then `WM_CLASS` |
| Windows | `SendInput` | poll `GetAsyncKeyState` | foreground window's process exe |
| macOS | `CGEventPost` with explicit flags | poll `CGEventSourceFlagsState` | not needed (Cmd is safe in terminals) |

- **Terminals.** Ctrl+C in a terminal sends SIGINT, and Ctrl+X exits nano. For a known terminal (built-in list of app ids/classes: kitty, Alacritty, foot, Ghostty, WezTerm, GNOME Console/Terminal, Ptyxis, Konsole, Yakuake, Tilix, Terminator, xfce4-terminal, xterm, urxvt, Windows Terminal, conhost, mintty, …) Copy becomes Ctrl+Shift+C (Ctrl+Insert on Windows) and Paste becomes Ctrl+Shift+V (Shift+Insert on Windows). Cut in a terminal copies instead, since there's nothing to cut.
- **Never Shift+Delete for Cut:** in Windows Explorer it permanently deletes the selected files.
- **Layouts:** inject by keysym (X11, KDE, GNOME, Hyprland key name) or virtual key (Windows) so non-QWERTY layouts press the right key. macOS needs the layout's keycode for "c"/"x"/"v" (`UCKeyTranslate` search); `kVK_ANSI_C` alone would press Cmd+J on Dvorak.
- **Permissions:** KDE asks once (dialog above). macOS needs Accessibility (`AXIsProcessTrustedWithOptions` prompt on first use, status in Settings). Windows can't inject into elevated windows (UIPI). Flatpak: KDE needs `--talk-name=org.kde.KWin` for the KWin script; the portal, the extension and the Hyprland socket already work from the sandbox.
- **Other wlroots compositors (Sway, river, Wayfire):** no RemoteDesktop portal. The new shortcuts report "not supported on this desktop yet". An optional final stage adds `zwp_virtual_keyboard_v1` (testable on Hyprland).

### Knowing the copy landed

The monitors dedupe by content, so copying the same thing twice is invisible to them today. Add a **selection generation**: an atomic bumped by every change listener event (data-control `selection`, XFixes, `WM_CLIPBOARDUPDATE`, macOS `changeCount`, the extension's change signals), plus a "processed up to" mark the monitors set after each check.

- **Copy/Cut & Send:** note the generation, inject, wait for it to move (timeout ~1 s). If it doesn't move, show the "Nothing copied" notification. If it does, wait for the monitor to catch up, then run the existing `manual_send` (kept payload, or plain-text fallback). Same content as before still counts as a copy.
- **Receive & Paste:** apply the pending content, wait for our own write to bump the generation, then press Paste. A large image arrives as a descriptor: paste once the fetch completes if that's within ~3 s, otherwise notify "Image ready, press Ctrl+V".
- **One action at a time** (busy flag). While injecting, `Activated` for our own IDs is treated as an echo (KDE retry signal), never as a new action.

### Manual receive applies everything

`manual_receive` writes plain text for anything that isn't an image, which drops rich formats (and needs checking for files). Make it use the same apply path as Auto-Receive. Receive & Paste depends on it.

### Keep My Clipboard (feature 5)

Setting `keep_local_clipboard` (default off), used only by Copy/Cut & Send and Receive & Paste.

- Monitors share the last seen local `ClipboardContent`. That's the snapshot, along with the kept manual-send payload.
- **Copy/Cut & Send:** copy, send, then write the snapshot back and restore the kept payload.
- **Receive & Paste:** write the received content, press Paste, wait, then write the snapshot back.
- **Restore timing is the weak spot:** paste reads are asynchronous, so restoring too early pastes the old content. Start with a fixed delay (~500 ms after the Paste press); add per-backend "target has read it" signals later only if needed (data-control `send`, X11 `SelectionRequest`, Windows delayed rendering, macOS data provider).
- **Clipboard managers** (Klipper, GNOME history extensions, Win+V, macOS managers) will see the temporary content. Mark restores and temporary writes where conventions exist: `x-kde-passwordManagerHint` (Klipper), `ExcludeClipboardContentFromMonitorProcessing` / `CanIncludeInClipboardHistory` (Windows), `org.nspasteboard.TransientType` (macOS).

### Settings and shortcut IDs

- **Settings:** keep `shortcut_send` (Send Clipboard) and `shortcut_receive` (Receive Only); add `shortcut_copy_send`, `shortcut_cut_send`, `shortcut_receive_paste`, `keep_local_clipboard`, `remote_desktop_token`, and a `shortcuts_version` marker.
- **Migration (v0 to v1):** a stored `CommandOrControl+Alt+C` / `+V` (the old defaults) moves to `+Shift+C` / `+Shift+V`, and the new ones get the defaults. Customised keys keep their meaning (send clipboard / receive only), and a new default that would collide with one is left unset.
- **Portal IDs:** rename to `copy-send`, `cut-send`, `send-clipboard`, `receive-paste`, `receive-only`. GNOME and KDE remember the trigger per ID, so reusing `manual-send` would keep Ctrl+Alt+C on Send Clipboard and block the new default. The portal work is unreleased, so only dev machines and the VMs see the change.
- **Hyprland:** runtime binds and config lines for all five.
- **D-Bus:** add `CopySend`, `CutSend`, `ReceivePaste` beside `ManualSend` / `ManualReceive`.

### Security (GNOME extension)

A D-Bus method that presses keys is an input-injection path for anything on the session bus. Limit it:

- Only the three actions, no arbitrary keys.
- Only when the caller is the current owner of `app.clustercut.clustercut`.
- Pressed only into the focused window.

This needs a line in the EGO review notes.

## Stages

Each stage ends with Keith testing on the named desktop.

### 1. Core + Hyprland (local) (DONE 2026-10-23, Keith tested)
- [x] `keys` module (`keys/mod.rs`): `Action`, `Chord`, terminal list, `supported()`, `perform()`; Hyprland backend in `hyprland.rs` (`press`, `active_window_class`, Lua and legacy syntax; legacy untested).
- [x] Change counts in `change_events.rs` (`note_change`, `mark_processed`, `wait_for_change_after`, `wait_until_processed`), bumped by the X11/Windows/data-control listeners, macOS `changeCount` and the GNOME signals; every monitor marks what it has checked. Unit test.
- [x] `shortcuts.rs`: `ManualAction` (IDs, descriptions, accelerators, manual gate, `run`) drives the X11 plugin, the portal, Hyprland binds and D-Bus. `copy_send` / `receive_paste` with a one-at-a-time guard; notifications "Nothing copied. Select something first." and "Can't press keys on this desktop yet…".
- [x] Manual receive goes through `common::apply_received` (shared with the app's Apply button): rich text, History, echo guard. Dropped `write_text_direct` (no echo guard, so with Auto-Send on a received text was re-broadcast).
- [x] Found and fixed: a fetched descriptor was re-stashed as pending in manual mode instead of applied (`handlers.rs` stream handler).
- [x] A whitespace-only clipboard drops the kept manual-send copy.
- [x] Settings fields + v1 migration in `load_settings` (saved straight away); tests for old defaults, customised and colliding keys.
- [x] Portal binds all five under the new IDs and ignores activations for an automatic direction; Hyprland binds five; D-Bus `CopySend`, `CutSend`, `ReceivePaste`.
- [x] Settings UI: five fields grouped per direction (polish in stage 8).
- [x] 164 Rust tests pass; frontend builds.
- [x] Tested locally (real keys via ydotool, Ctrl+Alt held 0.5 s): Copy & Send copies and sends; the same content copied again is still sent; no selection gives the notice and sends nothing; kitty gets Ctrl+Shift+C and its `sleep` survives; `"CTRL SHIFT"` applies both modifiers (GTK Ctrl+Shift+A deselects); Cut & Send cuts and sends; Receive & Paste with nothing pending says so. Migration of Keith's real settings ran as designed (backup in the session scratchpad).
- [ ] Keith: Receive & Paste with a real peer (needs a re-paired peer), a browser, LibreOffice. Note: Ctrl+Alt+Shift+V is taken in Keith's Hyprland config, so Receive Only isn't bound there (reported as a conflict).

### 2. KDE (VM) (DONE 2026-10-24, Keith tested)
- [x] Shared portal plumbing moved to `portal.rs` (`connect_as_app`, `request` with an interface, tokens, paths, `parent_window`, `close_session`); `portal_shortcuts` uses it.
- [x] `keys/remote_desktop.rs`: session per press, restore token in `remote_desktop_token` (backend-owned, preserved by `save_settings`), replaced after every `Start`; `Declined` when the user says no. Keysyms, so layouts are respected.
- [x] Release handling: `portal_shortcuts` tracks held shortcuts from Activated/Deactivated. Wait for release (1 s cap), 80 ms grace, press; if one of our shortcuts fires within 150 ms (the echo), wait for release again and press again, for up to 2.5 s. The echo's Deactivated arrives when the physical keys go up, so the retry waits properly.
- [x] `keys/kwin.rs`: KWin script (`desktopFileName`, else `resourceClass`) loaded on first use via `org.kde.kwin.Scripting`, reports to `app.clustercut.clustercut.KWin.ActiveWindow` at `/app/clustercut/clustercut/KWin`, unloaded on exit. Flatpak: `--talk-name=org.kde.KWin`.
- [x] Settings: "Allow Key Presses" row while KDE's permission is still to be given (`allow_key_presses`, `ShortcutStatus.keys`).
- [x] Tested on the VM: first use shows "ClusterCut is asking for special privileges" once, then copies and sends; second use 340 ms from shortcut to send; Alt held 0.7 s after C: one echo, then copied; KWin script reports `org.kde.konsole`; no tray indicator while a session is open. Keith: Konsole with a selection, Kate, holding Ctrl+Alt all work.
- Findings:
  - Attaching the permission dialog to ClusterCut's window raised it, so the keys landed in ClusterCut. Shortcut-triggered dialogs are unattached; only the Settings button attaches it.
  - KWin types the exact keysym it's given: a lowercase `c` with our Shift held may come out unshifted. With Shift, send the uppercase letter.
  - Konsole on Fedora 45 sends Ctrl+C on a real Ctrl+Shift+C when nothing is selected (Keith's control), so Copy & Send there with no selection interrupts, exactly like pressing the key yourself. With a selection it copies.
  - Dev machines that ran the earlier portal build keep the old `manual-send`/`manual-receive` IDs in KDE's kglobalaccel with the same keys; only the new IDs are in the session, so only they reach ClusterCut. Not an issue for released builds.

### 3. GNOME (VM) (DONE 2026-10-24, Keith tested)
- [x] Extension 4.2, `app.clustercut.clustercut.Keys` on the existing object path: `FocusedApp() -> s` (app ID without `.desktop`, else WM class) and `Press(s chord) -> b`, chords whitelisted (`ctrl+c`, `ctrl+x`, `ctrl+v`, `ctrl+shift+c`, `ctrl+shift+v`; shifted ones use uppercase keyvals). Deviation from the plan: chord choice and the terminal list stay in Rust, so the extension only presses. Waits for modifiers via `global.get_pointer()` (1.5 s cap), presses through a Clutter virtual keyboard from `global.stage.context.get_backend()` (`Clutter.get_default_backend()` is gone from newer shells).
- [x] Caller check: same PID as the owner of `app.clustercut.clustercut` (ClusterCut uses several bus connections). Verified: gdbus gets AccessDenied, ClusterCut gets through.
- [x] `SelectionChanged` signal on Clipboard2, after any content signal for the same owner change, or straight away for ClusterCut's own writes (which the extension suppresses for 500 ms). Rust's GNOME monitor counts it as a change event (biased `select!`, so content goes first). Without it, a re-copy of identical content or our own write (Receive & Paste) never registered.
- [x] `keys/gnome.rs`; `keys::check()` replaces `supported()` and says why not: "Needs version 4.2 or newer of the ClusterCut GNOME extension." `KeyStatus.needs_extension_update` for Settings (UI in stage 8).
- [x] `version-name` 4.2.
- [x] Tested on the VM over D-Bus `CopySend`: copied and sent 28 ms after the request. Found and fixed: `_busPid` ran its callback inside `try`, so an exception while pressing came back as AccessDenied.
- [x] Keith: real keys on the VM, Text Editor, Ptyxis (Terminal) with a selection, same text twice, holding Ctrl+Alt: all work (2026-10-24).
- [ ] EGO review note for the Keys interface (submission time).

### 4. X11 (best effort, not tested) (written 2026-10-24)
- [x] `keys/x11.rs`: XTEST (`x11rb` `xtest` feature), keycodes from the current keyboard mapping, waits on `QueryKeymap` for modifier keycodes (1.5 s cap), `_NET_ACTIVE_WINDOW` then `WM_CLASS` (instance and class) for terminals. Picked when not Wayland and `DISPLAY` is set. Builds; not run (Keith, 2026-10-23).

### 5. Windows (Keith) (written 2026-10-24)
- [x] `keys/windows.rs`: `SendInput` (Insert gets `KEYEVENTF_EXTENDEDKEY`), waits on `GetAsyncKeyState` for Ctrl/Alt/Shift/Win, foreground window's exe (`QueryFullProcessImageNameW`) for terminals, which get Ctrl+Insert / Shift+Insert. Fewer events sent than asked means UIPI blocked it (elevated app). New `windows` features: `Win32_UI_Input_KeyboardAndMouse`, `Win32_System_Threading`.
- [x] Compile-checked for `x86_64-pc-windows-msvc` in an isolated crate (the full app can't be cross-built here: `aws-lc-sys`), including the calls `keys/mod.rs` makes.
- [ ] Keith: Notepad, a browser, Explorer (files), Windows Terminal with a selection, holding Ctrl+Alt.

### 6. macOS (Keith) (written 2026-10-24)
- [x] `keys/macos.rs`: raw CoreGraphics FFI, Cmd(+Shift)+key with explicit event flags, waits on `CGEventSourceFlagsState`, keycode for c/x/v from the current layout via `UCKeyTranslate` (on the main thread; the input source APIs assert it on recent macOS), ANSI fallback. `AXIsProcessTrustedWithOptions` with the prompt before pressing; `KeysError::NeedsAccessibility` and the Settings Allow row (`permission: false`) until granted. No terminal check (Cmd is safe there).
- [x] Compile-checked for `aarch64-apple-darwin` in an isolated crate.
- [ ] Keith: first use shows the Accessibility alert; after granting, TextEdit, Safari, Finder (files), Terminal. Note: unsigned or ad-hoc-signed builds may lose the Accessibility grant on each update.

### 7. Keep My Clipboard
- [ ] Shared last-seen content, snapshot/restore around both flows, kept-payload restore, clipboard-manager hints.
- [ ] Test the restore delay against slow readers (browser, LibreOffice, a large image).

### 8. UI, docs, CHANGELOG
- [ ] Settings: five shortcut fields grouped under Send and Receive, the "Keep My Clipboard" switch, permission states (KDE approval, macOS Accessibility, GNOME extension too old, unsupported desktop).
- [ ] Manual Sync panel / tray wording for the renamed actions.
- [ ] CHANGELOG `[Unreleased]`; README shortcut table; reply on issue #21.

### 9. Optional: wlroots virtual keyboard
- [ ] `zwp_virtual_keyboard_v1` backend for Sway/river/Wayfire, tested on Hyprland.

## Known limits

- KDE: a shortcut whose leftover modifiers don't form one of our shortcuts (e.g. a custom Super+Shift+C held down) can't be detected; the copy fails and shows the "Nothing copied" notice.
- Apps that don't use Ctrl+C/X/V (Emacs, some games) won't copy or paste.
- VS Code's integrated terminal isn't detectable as a terminal: Ctrl+C there copies with a selection but interrupts without one.
- Keep My Clipboard can't stop clipboard managers that ignore the hints.
