import { version } from "../../../package.json";
import clsx from "clsx";
import { Settings, Monitor, Wifi } from "lucide-react";
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
            <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">
              History storage limit (MB)
            </label>
            <input
              type="number"
              min={0}
              className="h-10 w-40 rounded-xl border border-zinc-900/10 bg-white px-3 text-sm text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-white/5 dark:text-zinc-50"
              value={Math.round(settings.history_store_max_bytes / (1024 * 1024))}
              onChange={(e) => {
                const mb = Math.max(0, parseInt(e.target.value || "0", 10));
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

          {!settings.auto_send && (
            <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-3 dark:border-white/10 dark:bg-white/5">
              <div className="flex flex-col gap-2">
                <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Global Shortcut (Send)</div>
                <ShortcutRecorder
                  value={settings.shortcut_send}
                  onChange={(val) => setSettings({ ...settings, shortcut_send: val })}
                  placeholder="No shortcut set"
                />
                <div className="text-[10px] text-zinc-500">
                  Keyboard shortcut to manually broadcast clipboard.
                </div>
              </div>
            </div>
          )}

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

          {!settings.auto_receive && (
            <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-3 dark:border-white/10 dark:bg-white/5">
              <div className="flex flex-col gap-2">
                <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Global Shortcut (Receive)</div>
                <ShortcutRecorder
                  value={settings.shortcut_receive}
                  onChange={(val) => setSettings({ ...settings, shortcut_receive: val })}
                  placeholder="No shortcut set"
                />
                <div className="text-[10px] text-zinc-500">
                  Keyboard shortcut to apply pending clipboard data.
                </div>
              </div>
            </div>
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
