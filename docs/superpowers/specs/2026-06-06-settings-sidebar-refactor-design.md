# Settings Sidebar Refactor — Design

**Source:** Email from @mdunphy ("Issue 5"), sub-project 1 of 2. Branch `dunphy-mail`.
**Date:** 2026-06-06

## Problem

The Settings view (`src/components/SettingsView.tsx`, ~600 lines) is a single long
scroll of `<Card>` sections, and it keeps growing as settings are added. It needs
a clearer structure, and sub-project 2 (PIN-safe diagnostics) needs a sensible
home for a new in-memory event-log panel.

## Goal

Refactor Settings into a macOS-style two-pane layout: a left sidebar of
categories and a right content pane showing the selected category. **No settings
behavior changes** — autosave, network-identity logic, the mode-switch confirm
dialog, the compression dialog, conditional shortcut display, and every toggle's
effect are preserved exactly. This is presentation + code-organization only.

## Decisions

1. **Five categories:** General, Cluster, Files, Notifications, Diagnostics.
2. **Component split:** break the monolithic `SettingsView` into a shell + five
   focused presentational components (one per category).
3. **Sidebar icons:** lucide outline icons, matching the header bar's style.
4. **Shortcuts stay in General**, rendered conditionally (only when the matching
   auto-toggle is off), exactly as today.
5. The issue-18 "Network" card is dissolved: **mDNS Advertising → Cluster**,
   **Windows Firewall (Windows-only) → General**.

## Design

### Layout

A two-pane flex layout inside the existing `SettingsView` container:
- **Sidebar** (~200px, fixed): a vertical list of the five categories, each a
  button with a lucide outline icon + label. The active category is highlighted
  (emerald, matching existing toggle/active styles). Selecting a category sets
  `activeCategory`.
- **Content pane** (flex-1, scrollable): renders the component for the active
  category.

### Component structure

`SettingsView` becomes a **shell** that:
- Owns all shared state currently in the file: `settings`, `initialSettings`,
  `networkName`, `networkPin`, `provName`, `provPin`, `loading`, `saving`,
  `autostart`, `compressDialogOpen`, `autoRenameDialogOpen`, and a new
  `activeCategory` (default `"general"`).
- Keeps the existing effects: the settings/identity load, the `settings-changed`
  listener, and the debounced autosave `useEffect` (unchanged logic).
- Keeps the existing `<Dialog>`s (compression, auto-rename) and `toggleAutostart`.
- Renders the sidebar and switches on `activeCategory` to render one of:

  - `GeneralSettings`
  - `ClusterSettings`
  - `FilesSettings`
  - `NotificationsSettings`
  - `DiagnosticsSettings`

Each category component is **presentational**: it receives `settings` and
`setSettings` plus the specific extras it needs, and renders the same markup that
exists today (moved verbatim where possible). New files live in
`src/components/settings/` to keep them grouped:

- `src/components/settings/GeneralSettings.tsx` — props: `settings`, `setSettings`,
  `autostart`, `toggleAutostart`, `isWindows`. Renders: Start on Startup, Device
  Name, Auto Send (+ conditional Send `ShortcutRecorder`), Auto Receive (+
  conditional Receive `ShortcutRecorder`), Windows Firewall toggle
  (`{isWindows && ...}`), and the About/version footer (`v{version}
  ({__COMMIT_HASH__})`) at the bottom of the pane.
- `src/components/settings/ClusterSettings.tsx` — props: `settings`, `setSettings`,
  `provName`, `setProvName`, `provPin`, `setProvPin`, `hasClusterPeers`,
  `onAutoModeClick` (the guarded handler that opens the rename dialog or switches
  to auto). Renders: the Auto/Provisioned toggle, the provisioned name/PIN
  editor (conditional), and the mDNS Advertising toggle.
- `src/components/settings/FilesSettings.tsx` — props: `settings`, `setSettings`,
  `onEnableCompressClick` (opens the compression confirm dialog). Renders the
  File Transfer card contents (allow file transfer, auto-download limit slider,
  compression toggle).
- `src/components/settings/NotificationsSettings.tsx` — props: `settings`,
  `setSettings`. Renders the notification toggles.
- `src/components/settings/DiagnosticsSettings.tsx` — props: `settings`,
  `setSettings`. Renders the Verbose-pairing-logs toggle. (Sub-project 2 adds the
  event-log panel here.)

Shared presentational helpers already used (`Card`, `SectionHeader`, `clsx`,
`ShortcutRecorder`, `Dialog`) are imported by the relevant child components.

### Behavior preserved (must not change)

- The debounced autosave still fires on `settings`/identity changes and still
  calls `save_settings` + the provisioned/auto identity commands. It lives in the
  shell, so it sees all changes regardless of which pane made them.
- The Auto-mode button still routes through the confirm-dialog guard
  (`hasClusterPeers`) — the shell passes the handler down to `ClusterSettings`.
- Provisioned name/PIN validation messages, the compression confirm dialog, the
  conditional shortcut rows, and `toggleAutostart` are unchanged.
- `hasClusterPeers` prop from `App.tsx` is still consumed (now routed into
  `ClusterSettings`).

### Out of scope

- The in-memory diagnostics event log (sub-project 2).
- Any change to what a setting does, the autosave mechanism, or backend commands.
- Collapsing/responsive sidebar behaviour beyond a fixed-width sidebar.

## Testing

- `npm run build` (tsc + vite) passes with no type errors.
- Manual verification:
  - All five categories appear; clicking each shows the right settings.
  - Every setting that existed before is present under its category and still
    persists (toggle something in each category, confirm it saves / survives a
    Settings-tab reopen).
  - Conditional shortcuts: turn Auto Send off → Send shortcut row appears; on →
    hidden. Same for Receive.
  - Mode switch: Provisioned→Auto with a peer present still shows the confirm
    dialog; cancel reverts, confirm renames.
  - Windows firewall toggle appears in General only on Windows.
  - About/version footer shows at the bottom of General.

## File anchors (for the plan)

- `src/components/SettingsView.tsx` — becomes the shell (state + autosave +
  dialogs + sidebar + category switch).
- `src/components/settings/GeneralSettings.tsx` (new)
- `src/components/settings/ClusterSettings.tsx` (new)
- `src/components/settings/FilesSettings.tsx` (new)
- `src/components/settings/NotificationsSettings.tsx` (new)
- `src/components/settings/DiagnosticsSettings.tsx` (new)
- Existing imports reused: `./ui` (`Card`, `SectionHeader`), `./ShortcutRecorder`,
  `./Dialog`, `clsx`, `lucide-react` outline icons.
