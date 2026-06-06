# Settings Sidebar Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reorganize the single-scroll Settings view into a macOS-style sidebar (5 categories) with a content pane, splitting the 600-line file into a shell + five focused category components — with zero settings-behavior changes.

**Architecture:** `SettingsView` becomes a shell owning all shared state, the autosave effect, and the dialogs; it renders a left sidebar (category list) and the selected category's component. Five new presentational components under `src/components/settings/` hold the existing markup, moved verbatim and parameterized by props.

**Tech Stack:** React + TypeScript, Tailwind, lucide-react (outline icons), clsx.

**Reference spec:** `docs/superpowers/specs/2026-06-06-settings-sidebar-refactor-design.md`

**Build command:** `npm run build`

---

## Category → content mapping (from the current SettingsView.tsx)

- **General** ← "General" card (Start on Startup) + "Device Settings" card (Device Name) + "Synchronization" card (Auto Send/Receive + conditional shortcuts) + the **Windows Firewall** toggle (from the "Network" card) + the version footer.
- **Cluster** ← "Cluster Mode" card (Auto/Provisioned + name/PIN editor) + the **mDNS Advertising** toggle (from the "Network" card).
- **Files** ← "File Transfer" card.
- **Notifications** ← "Notifications" card.
- **Diagnostics** ← "Diagnostics" card (Verbose pairing logs).

The "Network" card (lines 469–505) is split: mDNS→Cluster, Firewall→General; the card wrapper itself is dropped.

---

## File Structure

- `src/components/settings/GeneralSettings.tsx` (new)
- `src/components/settings/ClusterSettings.tsx` (new)
- `src/components/settings/FilesSettings.tsx` (new)
- `src/components/settings/NotificationsSettings.tsx` (new)
- `src/components/settings/DiagnosticsSettings.tsx` (new)
- `src/components/SettingsView.tsx` (becomes the shell)

---

## Task 1: Create the five category components

Each component is presentational: it renders the existing markup (moved verbatim from `SettingsView.tsx`) wrapped in the existing `<Card>`, parameterized by props. Create all five. They won't be wired up until Task 2, so the build will show them as unused modules (fine).

**Files:**
- Create all five files listed above.

- [ ] **Step 1: `GeneralSettings.tsx`**

Create `src/components/settings/GeneralSettings.tsx`. Props and structure:

```tsx
import { version } from "../../../package.json";
import clsx from "clsx";
import { SectionHeader, Card } from "../ui";
import { ShortcutRecorder } from "../ShortcutRecorder";
import type { AppSettings } from "../../types";

// The firewall toggle only has an effect on Windows, where
// configure_windows_firewall() exists. Match ShortcutRecorder's userAgent check.
const isWindows = navigator.userAgent.toLowerCase().includes("win");

export function GeneralSettings({
  settings,
  setSettings,
  autostart,
  toggleAutostart,
}: {
  settings: AppSettings;
  setSettings: (s: AppSettings) => void;
  autostart: boolean;
  toggleAutostart: () => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {/* MOVE HERE, verbatim: the "General" <Card> block (current lines 203–223) */}
      {/* MOVE HERE, verbatim: the "Device Identity" <Card> block (current lines 226–244) */}
      {/* MOVE HERE, verbatim: the "Synchronization" <Card> block (current lines 323–391),
          which already includes the conditional Send/Receive ShortcutRecorder rows */}
      {/* MOVE HERE: the Windows Firewall toggle. Take the `{isWindows && ( ... )}`
          block from the current "Network" card (current lines 490–503) and wrap it
          in its own <Card> so it reads as a General row, e.g.:

          {isWindows && (
            <Card className="p-4">
              <div className="flex items-center justify-between">
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Configure Windows Firewall</div>
                  <div className="text-xs text-zinc-500">Add the inbound/outbound rule on startup (may prompt for admin). Turn off if your firewall is managed externally.</div>
                </div>
                <button
                  onClick={() => setSettings({ ...settings, configure_firewall: !settings.configure_firewall })}
                  className={clsx("relative h-6 w-11 shrink-0 rounded-full transition-colors", settings.configure_firewall ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
                >
                  <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.configure_firewall ? "translate-x-6" : "translate-x-1")} />
                </button>
              </div>
            </Card>
          )}
      */}

      {/* Version footer (moved from the old footer, version line only) */}
      <div className="pt-2 pb-2 text-center text-[10px] text-zinc-400">
        ClusterCut v{version} ({__COMMIT_HASH__})
      </div>
    </div>
  );
}
```

When moving the three `<Card>` blocks, change nothing inside them except: they reference `settings`, `setSettings`, `autostart`, `toggleAutostart`, `clsx`, `ShortcutRecorder`, `SectionHeader`, `Card` — all now props or imports in this file. The "General" card still uses the `Settings` lucide icon in its `SectionHeader`; import what each moved card's `SectionHeader` icon needs from `lucide-react` (the "General" card uses `Settings`; "Device Settings" uses `Monitor`; "Synchronization" uses `Wifi`). Add those to the lucide import.

- [ ] **Step 2: `ClusterSettings.tsx`**

Create `src/components/settings/ClusterSettings.tsx`:

```tsx
import clsx from "clsx";
import { ShieldCheck, Wifi } from "lucide-react";
import { SectionHeader, Card } from "../ui";
import type { AppSettings } from "../../types";

export function ClusterSettings({
  settings,
  setSettings,
  provName,
  setProvName,
  provPin,
  setProvPin,
  onAutoModeClick,
}: {
  settings: AppSettings;
  setSettings: (s: AppSettings) => void;
  provName: string;
  setProvName: (v: string) => void;
  provPin: string;
  setProvPin: (v: string) => void;
  onAutoModeClick: () => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {/* MOVE HERE, verbatim: the "Cluster Mode" <Card> block (current lines 247–320),
          with ONE change: the Autogenerated button's onClick becomes `onClick={onAutoModeClick}`
          (the shell owns the hasClusterPeers guard + dialog). */}

      {/* MOVE HERE: the mDNS Advertising row from the old "Network" card (current
          lines 477–488), wrapped in its own <Card>:

          <Card className="p-4">
            <SectionHeader
              icon={<Wifi className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
              title="Discovery"
              subtitle="How this device is found."
            />
            <div className="mt-4 px-1">
              <div className="flex items-center justify-between">
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">mDNS Advertising</div>
                  <div className="text-xs text-zinc-500">Let other devices discover this one automatically. Turn off to stay hidden and connect only via Add Remote.</div>
                </div>
                <button
                  onClick={() => setSettings({ ...settings, mdns_advertising: !settings.mdns_advertising })}
                  className={clsx("relative h-6 w-11 shrink-0 rounded-full transition-colors", settings.mdns_advertising ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
                >
                  <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.mdns_advertising ? "translate-x-6" : "translate-x-1")} />
                </button>
              </div>
            </div>
          </Card>
      */}
    </div>
  );
}
```

`ShieldCheck` is the icon used by the "Cluster Mode" card's `SectionHeader`; `Wifi` by the new Discovery card. Keep both imports.

- [ ] **Step 3: `FilesSettings.tsx`**

Create `src/components/settings/FilesSettings.tsx`:

```tsx
import clsx from "clsx";
import { SectionHeader, Card } from "../ui";
import type { AppSettings } from "../../types";

export function FilesSettings({
  settings,
  setSettings,
  onEnableCompressClick,
}: {
  settings: AppSettings;
  setSettings: (s: AppSettings) => void;
  onEnableCompressClick: () => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {/* MOVE HERE, verbatim: the "File Transfer" <Card> block (current lines 394–467),
          with ONE change in the compression toggle's onClick: replace the inline
              if (!settings.compress_file_transfers) { setCompressDialogOpen(true); }
              else { setSettings({ ...settings, compress_file_transfers: false }); }
          with:
              if (!settings.compress_file_transfers) { onEnableCompressClick(); }
              else { setSettings({ ...settings, compress_file_transfers: false }); }
      */}
    </div>
  );
}
```

The File Transfer card's `SectionHeader` uses an inline `<svg>` icon (not a lucide import), so no new lucide import is needed here.

- [ ] **Step 4: `NotificationsSettings.tsx`**

Create `src/components/settings/NotificationsSettings.tsx`:

```tsx
import clsx from "clsx";
import { Info } from "lucide-react";
import { SectionHeader, Card } from "../ui";
import type { AppSettings } from "../../types";

export function NotificationsSettings({
  settings,
  setSettings,
}: {
  settings: AppSettings;
  setSettings: (s: AppSettings) => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {/* MOVE HERE, verbatim: the "Notifications" <Card> block (current lines 508–549). */}
    </div>
  );
}
```

- [ ] **Step 5: `DiagnosticsSettings.tsx`**

Create `src/components/settings/DiagnosticsSettings.tsx`:

```tsx
import clsx from "clsx";
import { ShieldCheck } from "lucide-react";
import { SectionHeader, Card } from "../ui";
import type { AppSettings } from "../../types";

export function DiagnosticsSettings({
  settings,
  setSettings,
}: {
  settings: AppSettings;
  setSettings: (s: AppSettings) => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {/* MOVE HERE, verbatim: the "Diagnostics" <Card> block (current lines 555–590). */}
    </div>
  );
}
```

- [ ] **Step 6: Verify the new files type-check**

Run: `npm run build`
Expected: builds. (The new components aren't imported yet; TS allows unused modules. The old `SettingsView.tsx` is still intact and still rendering, so the app is unchanged at this point. If you already removed blocks from SettingsView, the build will fail — do NOT touch SettingsView in this task; only create the new files.)

- [ ] **Step 7: Commit**

```bash
git add src/components/settings/
git commit -m "feat: extract Settings category components (sidebar refactor, part 1)"
```

---

## Task 2: Convert SettingsView into the shell (sidebar + category switch)

**Files:**
- Modify: `src/components/SettingsView.tsx`

- [ ] **Step 1: Replace the imports + add category constant**

At the top of `src/components/SettingsView.tsx`, replace the current component-specific imports. Keep the hooks/tauri/state imports; swap the section imports for the new components and sidebar icons:

```tsx
import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { Settings, Network, FileText, Bell, Activity } from "lucide-react";
import clsx from "clsx";
import { Dialog } from "./Dialog";
import type { AppSettings } from "../types";
import { GeneralSettings } from "./settings/GeneralSettings";
import { ClusterSettings } from "./settings/ClusterSettings";
import { FilesSettings } from "./settings/FilesSettings";
import { NotificationsSettings } from "./settings/NotificationsSettings";
import { DiagnosticsSettings } from "./settings/DiagnosticsSettings";

type SettingsCategory = "general" | "cluster" | "files" | "notifications" | "diagnostics";

const CATEGORIES: { id: SettingsCategory; label: string; icon: typeof Settings }[] = [
  { id: "general", label: "General", icon: Settings },
  { id: "cluster", label: "Cluster", icon: Network },
  { id: "files", label: "Files", icon: FileText },
  { id: "notifications", label: "Notifications", icon: Bell },
  { id: "diagnostics", label: "Diagnostics", icon: Activity },
];
```

Note: `version`, `__COMMIT_HASH__`, `SectionHeader`, `Card`, `ShortcutRecorder`, `Monitor`, `ShieldCheck`, `Wifi`, `Info` are no longer used in this file (they moved into the category components) — make sure none remain imported here (no unused-import warnings). The `isWindows` module const also moves out (it now lives in `GeneralSettings.tsx`); delete it from this file.

- [ ] **Step 2: Add `activeCategory` state**

In the component body, alongside the other `useState` calls (near line 37), add:

```tsx
  const [activeCategory, setActiveCategory] = useState<SettingsCategory>("general");
```

Keep ALL existing state, both `useEffect`s (autostart load; settings/identity load + `settings-changed` listener), the autosave `useEffect`, and `toggleAutostart` EXACTLY as they are. Do not change the autosave logic.

- [ ] **Step 3: Add the auto-mode guard handler**

The "Autogenerated" button's guard logic currently lives inline in the Cluster Mode card. Lift it into a handler in the shell (so `ClusterSettings` can call it via the `onAutoModeClick` prop). Add this just before the `if (loading ...) return` line:

```tsx
  const handleAutoModeClick = () => {
    if (!settings) return;
    if (settings.cluster_mode === "provisioned" && hasClusterPeers) {
      // Switching to Auto regenerates the name and renames the cluster for
      // everyone — confirm first (cluster-name convergence).
      setAutoRenameDialogOpen(true);
    } else {
      setSettings({ ...settings, cluster_mode: "auto" });
    }
  };
```

- [ ] **Step 4: Replace the entire `return (...)` JSX with the shell layout**

Replace everything from `return (` (line 200) to the end of the component (the final `);` + `}`) with:

```tsx
  if (loading || !settings) return <div className="p-10 text-center text-zinc-500">Loading settings...</div>;

  return (
    <div className="flex h-full">
      {/* Sidebar */}
      <div className="w-48 flex-shrink-0 border-r border-zinc-900/10 bg-zinc-900/[0.02] p-2 dark:border-white/10 dark:bg-white/[0.02]">
        <nav className="flex flex-col gap-1">
          {CATEGORIES.map(({ id, label, icon: Icon }) => (
            <button
              key={id}
              onClick={() => setActiveCategory(id)}
              className={clsx(
                "flex items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors",
                activeCategory === id
                  ? "bg-emerald-500 text-white"
                  : "text-zinc-700 hover:bg-zinc-900/5 dark:text-zinc-300 dark:hover:bg-white/5"
              )}
            >
              <Icon className="h-[17px] w-[17px]" />
              <span>{label}</span>
            </button>
          ))}
        </nav>
      </div>

      {/* Content pane */}
      <div className="flex flex-1 flex-col overflow-y-auto p-4">
        <div className="flex-1">
          {activeCategory === "general" && (
            <GeneralSettings
              settings={settings}
              setSettings={setSettings}
              autostart={autostart}
              toggleAutostart={toggleAutostart}
            />
          )}
          {activeCategory === "cluster" && (
            <ClusterSettings
              settings={settings}
              setSettings={setSettings}
              provName={provName}
              setProvName={setProvName}
              provPin={provPin}
              setProvPin={setProvPin}
              onAutoModeClick={handleAutoModeClick}
            />
          )}
          {activeCategory === "files" && (
            <FilesSettings
              settings={settings}
              setSettings={setSettings}
              onEnableCompressClick={() => setCompressDialogOpen(true)}
            />
          )}
          {activeCategory === "notifications" && (
            <NotificationsSettings settings={settings} setSettings={setSettings} />
          )}
          {activeCategory === "diagnostics" && (
            <DiagnosticsSettings settings={settings} setSettings={setSettings} />
          )}
        </div>

        {/* Saving indicator (always visible regardless of category) */}
        <div className="flex items-center justify-center pt-2">
          <span className={clsx("text-[10px] font-medium transition-opacity", saving ? "opacity-100 text-zinc-500" : "opacity-0 duration-1000")}>
            Saving changes...
          </span>
        </div>
      </div>

      <Dialog
        open={compressDialogOpen}
        title="Enable file transfer compression?"
        description="This feature is incompatible with ClusterCut 0.2.2 and earlier. Files sent to peers running an older version will arrive corrupt. Make sure all your devices are on 0.2.3 or newer before enabling."
        type="danger"
        confirmLabel="Enable compression"
        onConfirm={() => {
          setSettings({ ...settings, compress_file_transfers: true });
          setCompressDialogOpen(false);
        }}
        onCancel={() => setCompressDialogOpen(false)}
      />

      <Dialog
        open={autoRenameDialogOpen}
        title="Switch to an auto-generated name?"
        description="This will rename the cluster for all connected devices to a new auto-generated name. Continue?"
        type="danger"
        confirmLabel="Rename cluster"
        onConfirm={() => {
          setSettings({ ...settings, cluster_mode: "auto" });
          setAutoRenameDialogOpen(false);
        }}
        onCancel={() => setAutoRenameDialogOpen(false)}
      />
    </div>
  );
}
```

Notes:
- The version footer moved into `GeneralSettings`; the shell keeps only the "Saving changes..." indicator (now always visible, not just on one category).
- Both `<Dialog>`s stay in the shell (unchanged).
- `setProvName`/`setProvPin` are passed to `ClusterSettings`; confirm their setters exist (they do: `const [provName, setProvName] = useState("")` etc.).

- [ ] **Step 5: Verify build + that nothing is unused**

Run: `npm run build`
Expected: builds with no type errors and no unused-variable/import warnings. If TS complains an import or state value is unused, it means a block didn't get moved or a prop isn't wired — fix by completing the move. All of `settings`, `setSettings`, `provName/provPin` (+setters), `autostart`, `toggleAutostart`, `saving`, `compressDialogOpen`, `autoRenameDialogOpen`, `activeCategory`, `hasClusterPeers` should still be used.

- [ ] **Step 6: Commit**

```bash
git add src/components/SettingsView.tsx
git commit -m "feat: Settings sidebar shell with category panes (sidebar refactor, part 2)"
```

---

## Task 3: Build + manual verification

- [ ] **Step 1: Build**

Run: `npm run build`
Expected: clean build, no type errors.

- [ ] **Step 2: Manual verification (record results)**

Run the app (`npm run tauri dev` or the project's run recipe) and check:
- The Settings tab shows a left sidebar with General / Cluster / Files / Notifications / Diagnostics (outline icons), General selected by default.
- **General:** Start on Startup, Device Name, Auto Send, Auto Receive all present and save. Turning Auto Send OFF reveals the Send shortcut row; ON hides it. Same for Auto Receive. On Windows, the Configure Windows Firewall toggle appears; on Linux/macOS it does not. Version footer at the bottom.
- **Cluster:** Auto/Provisioned toggle works; Provisioned reveals the name/PIN editor with validation; mDNS Advertising toggle present. With a trusted peer present, switching Provisioned→Auto shows the confirm dialog (cancel reverts, confirm renames).
- **Files:** Allow File Transfer, auto-download slider, Compress toggle (enabling shows the compression confirm dialog).
- **Notifications:** all notification toggles + Large File Transfers toggle.
- **Diagnostics:** Verbose pairing logs toggle.
- Changing a setting in any category shows "Saving changes..." and persists (reopen Settings / restart to confirm).

- [ ] **Step 3: Final commit (only if fixups were needed)**

```bash
git add -A
git commit -m "fix: settings refactor verification fixups"
```

---

## Self-Review Notes

- **Spec coverage:** 5 categories + sidebar → Task 2; component split into `src/components/settings/*` → Task 1; category mapping incl. Network-card split (mDNS→Cluster, Firewall→General) → Task 1 Steps 1–2; shortcuts stay conditional in General → moved with the Synchronization card (Task 1 Step 1); behavior preserved (autosave, dialogs, mode-switch guard, identity logic) → shell retains all state/effects/dialogs (Task 2 Steps 2–4); version footer in General, Saving indicator in shell → Tasks 1/2.
- **No placeholders:** new scaffolding (sidebar, shell switch, prop interfaces, the two relocated toggles) is given in full; the large existing JSX blocks are relocated verbatim with precise source line ranges + the few documented per-block edits (auto-mode onClick → `onAutoModeClick`; compress onClick → `onEnableCompressClick`). These are moves of existing code, not unspecified work.
- **Type consistency:** prop interfaces defined in Task 1 match the wiring in Task 2 (`onAutoModeClick`, `onEnableCompressClick`, `setProvName`/`setProvPin`, `autostart`/`toggleAutostart`). `SettingsCategory` union used consistently.
