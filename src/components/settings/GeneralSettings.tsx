import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { version } from "../../../package.json";
import clsx from "clsx";
import { Settings, Monitor, Wifi } from "lucide-react";
import { SectionHeader, Card, Button } from "../ui";
import { GlobalShortcutField } from "./GlobalShortcutField";
import type { AppSettings, HistoryRetention, ShortcutStatus } from "../../types";

// The firewall toggle only has an effect on Windows, where
// configure_windows_firewall() exists. Match ShortcutRecorder's userAgent check.
const isWindows = navigator.userAgent.toLowerCase().includes("win");

const HISTORY_RETENTION: { value: HistoryRetention; label: string }[] = [
  { value: "off", label: "Off" },
  { value: "15m", label: "15 minutes" },
  { value: "1h", label: "1 hour" },
  { value: "1d", label: "1 day" },
  { value: "1w", label: "1 week" },
  { value: "until_quit", label: "Until ClusterCut quits" },
];

const HISTORY_MAX_ITEMS = [10, 25, 50, 100, 250, 500];

const selectClass =
  "h-10 w-56 rounded-xl border border-zinc-900/10 bg-white px-3 text-sm text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-zinc-900 dark:text-zinc-50";

// The manual shortcuts per direction. `id` is the backend's ManualAction ID.
type ShortcutDef = {
  id: string;
  dbusMethod: string;
  field: "shortcut_copy_send" | "shortcut_cut_send" | "shortcut_send" | "shortcut_receive_paste" | "shortcut_receive";
  label: string;
  hint: string;
};

const SEND_SHORTCUTS: ShortcutDef[] = [
  { id: "copy-send", dbusMethod: "CopySend", field: "shortcut_copy_send", label: "Copy & Send", hint: "Copies the selection in the focused app and sends it." },
  { id: "cut-send", dbusMethod: "CutSend", field: "shortcut_cut_send", label: "Cut & Send", hint: "Cuts the selection in the focused app and sends it." },
  { id: "send-clipboard", dbusMethod: "ManualSend", field: "shortcut_send", label: "Send Clipboard", hint: "Sends what's already on the clipboard." },
];

const RECEIVE_SHORTCUTS: ShortcutDef[] = [
  { id: "receive-paste", dbusMethod: "ReceivePaste", field: "shortcut_receive_paste", label: "Receive & Paste", hint: "Applies the received content and pastes it into the focused app." },
  { id: "receive-only", dbusMethod: "ManualReceive", field: "shortcut_receive", label: "Receive Only", hint: "Puts the received content on the clipboard without pasting." },
];

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
  // How global shortcuts are bound here (plugin, Wayland portal, Hyprland binds).
  const [shortcutStatus, setShortcutStatus] = useState<ShortcutStatus | null>(null);
  const refreshShortcutStatus = () => invoke<ShortcutStatus>("get_shortcut_status").then(setShortcutStatus);
  useEffect(() => {
    refreshShortcutStatus();
    const unlisten = listen("shortcut-status-changed", refreshShortcutStatus);
    return () => { unlisten.then(f => f()); };
  }, []);

  // How the one-step shortcuts press keys here, and what's in their way.
  const manualMode = !settings.auto_send || !settings.auto_receive;
  const keys = shortcutStatus?.keys;
  // KDE asks once and macOS needs Accessibility; offer to get that out of the way.
  const showKeyPermission = manualMode && keys?.permission === false;
  const keyPermissionText =
    keys?.backend === "mac_os"
      ? "Copy & Send, Cut & Send and Receive & Paste press keys for you, which macOS only lets apps do once they're allowed under System Settings > Privacy & Security > Accessibility."
      : "Copy & Send, Cut & Send and Receive & Paste press keys for you, which your desktop asks you to allow once. Allow it now, or when you first use one.";
  const keysUnavailable =
    manualMode && keys?.backend === "none"
      ? keys.needs_extension_update
        ? "Copy & Send, Cut & Send and Receive & Paste need version 4.2 or newer of the ClusterCut GNOME extension."
        : "This desktop doesn't let ClusterCut press keys, so Copy & Send, Cut & Send and Receive & Paste don't work here. Send Clipboard and Receive Only do."
      : null;
  const [keyPermissionError, setKeyPermissionError] = useState<string | null>(null);
  const allowKeyPresses = () => {
    setKeyPermissionError(null);
    // macOS grants in System Settings without telling us, so look again afterwards.
    invoke("allow_key_presses")
      .catch((e) => setKeyPermissionError(String(e)))
      .finally(refreshShortcutStatus);
  };

  const showHyprlandToggle =
    !!shortcutStatus?.hyprland &&
    shortcutStatus.portal?.state === "bound" &&
    (!settings.auto_send || !settings.auto_receive);

  const shortcutGroup = (title: string, shortcuts: ShortcutDef[]) => (
    <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-3 dark:border-white/10 dark:bg-white/5">
      <div className="flex flex-col gap-4">
        <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">{title}</div>
        {shortcuts.map((s) => (
          <div key={s.id} className="flex flex-col gap-2">
            <div className="text-xs font-medium text-zinc-700 dark:text-zinc-300">{s.label}</div>
            <GlobalShortcutField
              shortcutId={s.id}
              dbusMethod={s.dbusMethod}
              value={settings[s.field]}
              onChange={(val) => setSettings({ ...settings, [s.field]: val })}
              status={shortcutStatus}
              hyprlandBindKeys={settings.hyprland_bind_keys}
            />
            <div className="text-[10px] text-zinc-500">{s.hint}</div>
          </div>
        ))}
      </div>
    </div>
  );

  return (
    <div className="flex flex-col gap-4">
      {/* General Settings */}
      <Card className="p-4">
        <SectionHeader
          icon={<Settings className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="General"
          subtitle="Application preferences."
        />
        <div className="mt-4 px-1">
          <div className="flex items-center justify-between">
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Start on Startup</div>
              <div className="text-xs text-zinc-500">Launch automatically when you log in.</div>
            </div>
            <button
              onClick={toggleAutostart}
              className={clsx("relative h-6 w-11 rounded-full transition-colors", autostart ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
            >
              <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", autostart ? "translate-x-6" : "translate-x-1")} />
            </button>
          </div>
          <div className="mt-4 flex flex-col gap-1">
            <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">Keep History</label>
            <select
              className={selectClass}
              value={settings.history_retention}
              onChange={(e) => setSettings({ ...settings, history_retention: e.target.value as HistoryRetention })}
            >
              {HISTORY_RETENTION.map((r) => (
                <option key={r.value} value={r.value}>{r.label}</option>
              ))}
            </select>
            <div className="text-[10px] text-zinc-500">
              How long copied and received items stay in History. History is never saved to disk, so it never outlives ClusterCut.
            </div>
          </div>
          {settings.history_retention !== "off" && (
            <div className="mt-4 flex flex-col gap-1">
              <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">Keep At Most</label>
              <select
                className={selectClass}
                value={settings.history_max_items}
                onChange={(e) => setSettings({ ...settings, history_max_items: parseInt(e.target.value, 10) })}
              >
                {/* A value set by hand in settings.json still shows. */}
                {[...new Set([...HISTORY_MAX_ITEMS, settings.history_max_items])].sort((a, b) => a - b).map((n) => (
                  <option key={n} value={n}>{n} items</option>
                ))}
              </select>
              <div className="text-[10px] text-zinc-500">The oldest go first.</div>
            </div>
          )}
          <div className="mt-4 flex flex-col gap-1">
            <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">
              History storage limit (MB)
            </label>
            <input
              type="number"
              min={1}
              step={1}
              className="h-10 w-40 rounded-xl border border-zinc-900/10 bg-white px-3 text-sm text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-white/5 dark:text-zinc-50"
              value={Math.round(settings.history_store_max_bytes / (1024 * 1024))}
              onChange={(e) => {
                if (e.target.value === "") return;
                const mb = Math.max(1, parseInt(e.target.value, 10) || 1);
                setSettings({ ...settings, history_store_max_bytes: mb * 1024 * 1024 });
              }}
            />
            <div className="text-[10px] text-zinc-500">
              How much copied text &amp; image content History keeps for re-copying. Files don&apos;t count.
            </div>
          </div>
        </div>
      </Card>

      {/* Device Identity */}
      <Card className="p-4">
        <SectionHeader
          icon={<Monitor className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Device Settings"
          subtitle="Identity and Discovery."
        />
        <div className="mt-4 px-1">
          <div className="flex flex-col gap-1">
            <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">Device Name</label>
            <input
              className="h-10 rounded-xl border border-zinc-900/10 bg-white px-3 text-sm text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-white/5 dark:text-zinc-50"
              placeholder="Default: Hostname"
              value={settings.custom_device_name || ""}
              onChange={(e) => setSettings({ ...settings, custom_device_name: e.target.value || null })}
            />
            <div className="text-[10px] text-zinc-500">Visible to other devices in the cluster.</div>
          </div>
        </div>
      </Card>

      {/* Synchronization */}
      <Card className="p-4">
        <SectionHeader
          icon={<Wifi className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Synchronization"
          subtitle="Control clipboard flow."
        />
        <div className="mt-4 px-1 space-y-4">
          <div className="flex items-center justify-between">
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Automatic Send</div>
              <div className="text-xs text-zinc-500">Automatically send local clipboard to the cluster.</div>
            </div>
            {/* Simple Toggle Switch */}
            <button
              onClick={() => setSettings({ ...settings, auto_send: !settings.auto_send })}
              className={clsx("relative h-6 w-11 rounded-full transition-colors", settings.auto_send ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
            >
              <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.auto_send ? "translate-x-6" : "translate-x-1")} />
            </button>
          </div>

          {!settings.auto_send && shortcutGroup("Global Shortcuts (Send)", SEND_SHORTCUTS)}

          <div className="h-px bg-zinc-900/5 dark:bg-white/5" />

          <div className="flex items-center justify-between">
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Automatic Receive</div>
              <div className="text-xs text-zinc-500">Automatically overwrite local clipboard with data from the cluster.</div>
            </div>
            <button
              onClick={() => setSettings({ ...settings, auto_receive: !settings.auto_receive })}
              className={clsx("relative h-6 w-11 rounded-full transition-colors", settings.auto_receive ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
            >
              <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.auto_receive ? "translate-x-6" : "translate-x-1")} />
            </button>
          </div>

          {!settings.auto_receive && shortcutGroup("Global Shortcuts (Receive)", RECEIVE_SHORTCUTS)}

          {(!settings.auto_send || !settings.auto_receive) && (
            <>
              <div className="h-px bg-zinc-900/5 dark:bg-white/5" />
              <div className="flex items-center justify-between gap-4">
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Keep My Clipboard</div>
                  <div className="text-xs text-zinc-500">
                    Copy & Send, Cut & Send and Receive & Paste put back what was on your clipboard before.
                  </div>
                </div>
                <button
                  onClick={() => setSettings({ ...settings, keep_local_clipboard: !settings.keep_local_clipboard })}
                  className={clsx("relative h-6 w-11 shrink-0 rounded-full transition-colors", settings.keep_local_clipboard ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
                >
                  <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.keep_local_clipboard ? "translate-x-6" : "translate-x-1")} />
                </button>
              </div>
            </>
          )}

          {showKeyPermission && (
            <>
              <div className="h-px bg-zinc-900/5 dark:bg-white/5" />
              <div className="flex items-center justify-between gap-4">
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Allow Key Presses</div>
                  <div className="text-xs text-zinc-500">{keyPermissionText}</div>
                  {keyPermissionError && <div className="mt-1 text-[10px] text-amber-600 dark:text-amber-400">{keyPermissionError}</div>}
                </div>
                <Button size="sm" onClick={allowKeyPresses}>Allow</Button>
              </div>
            </>
          )}

          {keysUnavailable && (
            <>
              <div className="h-px bg-zinc-900/5 dark:bg-white/5" />
              <div className="text-xs text-amber-600 dark:text-amber-400">{keysUnavailable}</div>
            </>
          )}

          {showHyprlandToggle && (
            <>
              <div className="h-px bg-zinc-900/5 dark:bg-white/5" />
              <div className="flex items-center justify-between gap-4">
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Bind Keys in Hyprland</div>
                  <div className="text-xs text-zinc-500">
                    Hyprland doesn't assign keys to app shortcuts. Let ClusterCut add its binds while it runs, without editing your config.
                  </div>
                </div>
                <button
                  onClick={() => setSettings({ ...settings, hyprland_bind_keys: !settings.hyprland_bind_keys })}
                  className={clsx("relative h-6 w-11 shrink-0 rounded-full transition-colors", settings.hyprland_bind_keys ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
                >
                  <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.hyprland_bind_keys ? "translate-x-6" : "translate-x-1")} />
                </button>
              </div>
            </>
          )}
        </div>
      </Card>

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
      <div className="pt-2 pb-2 text-center text-[10px] text-zinc-400">
        ClusterCut v{version} ({__COMMIT_HASH__})
      </div>
    </div>
  );
}
