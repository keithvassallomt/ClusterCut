import { useState, useEffect } from "react";
import { version } from "../../package.json";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { Monitor, ShieldCheck, Settings, Wifi, Info } from "lucide-react";
import clsx from "clsx";
import { SectionHeader, Card } from "./ui";
import { ShortcutRecorder } from "./ShortcutRecorder";
import { Dialog } from "./Dialog";
import type { AppSettings } from "../types";

export function SettingsView({
  onSettingsRefreshed
}: {
  onSettingsRefreshed?: () => void;
}) {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [initialSettings, setInitialSettings] = useState<AppSettings | null>(null); // For mode switch detection

  const [networkName, setNetworkName] = useState("");
  const [networkPin, setNetworkPin] = useState("");

  const [provName, setProvName] = useState("");
  const [provPin, setProvPin] = useState("");

  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [autostart, setAutostart] = useState(false);
  const [compressDialogOpen, setCompressDialogOpen] = useState(false);

  useEffect(() => {
    // Check if backend handles state (Flatpak) or native fallback
    invoke<boolean | null>("get_autostart_state").then(res => {
      if (res !== null) {
        setAutostart(res);
      } else {
        isEnabled().then(setAutostart);
      }
    });
  }, []);

  const toggleAutostart = async () => {
    try {
      const handled = await invoke<boolean>("configure_autostart", { enable: !autostart });
      if (handled) {
        setAutostart(!autostart);
        return;
      }

      if (autostart) {
        await disable();
        setAutostart(false);
      } else {
        await enable();
        setAutostart(true);
      }
    } catch (e) {
      console.error("Failed to toggle autostart:", e);
      alert("Failed to toggle autostart: " + e);
    }
  };

  // Load Settings
  useEffect(() => {
    Promise.all([
      invoke<AppSettings>("get_settings"),
      invoke<string>("get_network_name"),
      invoke<string>("get_network_pin")
    ]).then(([s, n, p]) => {
      setSettings(s);
      setInitialSettings(JSON.parse(JSON.stringify(s)));
      setNetworkName(n);
      setNetworkPin(p);
      setProvName(n);
      setProvPin(p);
      setLoading(false);
    });

    const unlisten = listen<AppSettings>("settings-changed", (event) => {
      setSettings(event.payload);
      // We also need to update initialSettings to prevent auto-save loops if we consider this "saved"
      // But auto-save effect depends on comparing `settings` to something?
      // No, auto-save effect runs when `settings` changes.
      // If we update `settings` here, auto-save triggers.
      // Use a flag or ref to avoid re-saving what we just received?
      // Actually, if settings match what backend has, saving it again is harmless (idempotent-ish).
      // To be safe, we can update initialSettings too.
      setInitialSettings(JSON.parse(JSON.stringify(event.payload)));
    });

    return () => { unlisten.then(f => f()); };
  }, []);

  // Autosave Effect
  useEffect(() => {
    if (loading || !settings || !initialSettings) return;

    // Only save when the user has actually changed something. Without this
    // guard, post-save state syncs (deep-cloned `initialSettings`, the
    // `settings-changed` listener) produce new object references that retrigger
    // this effect, causing an endless save loop while the Settings tab is open.
    const settingsDirty = JSON.stringify(settings) !== JSON.stringify(initialSettings);
    const identityDirty = provName !== networkName || provPin !== networkPin;
    if (!settingsDirty && !identityDirty) return;

    const savePayload = {
      settings: { ...settings },
      provName,
      provPin,
      currentMode: settings.cluster_mode
    };

    const save = async () => {
      setSaving(true);
      try {
        // 1. Save General Settings
        await invoke("save_settings", { settings: savePayload.settings });

        // 2. Handle Identity Logic
        // If mode is Provisioned
        if (savePayload.currentMode === "provisioned") {
          // Validation
          const isNameValid = !savePayload.provName.trim().includes(" ") && savePayload.provName.length > 0;
          const isPinValid = savePayload.provPin.length >= 6;
          // Check change against CURRENT ACTIVE network name/pin
          // We use refs or closure state. Here we use state `networkName`.
          // Note: networkName state might be stale in closure?
          // No, useEffect re-runs if deps change. `networkName` is not in deps.
          // But `provName` IS in deps.
          // We need `networkName` in deps? Or access it safely.
          // Let's rely on the fact that if we successfully change identity, we update `networkName`.

          // Better approach for identity:
          // Only act if `provName/Pin` differs from `initialName/InitialPin` (which track active state).

          // Actually, let's use the local state directly, but we need to capture it.
          // We'll rely on `networkName` being consistent with `initialName` usually.

          if (isNameValid && isPinValid) {
            // Check if changed from ACTIVE
            // functionality: If I change name, I want to apply it.
            // But I need to know what the current active is. `networkName`.
            // But I can't access `networkName` efficiently inside this closure if I don't dep it.
            // But if I dep it, I re-trigger. That's fine.

            // ACTUALLY: The `networkName` state is updated ONLY when we reload from backend.
            // So it is the "Active" one.
            if (savePayload.provName !== networkName || savePayload.provPin !== networkPin) {
              console.log("Applying new Identity...");
              await invoke("set_network_identity", { name: savePayload.provName, pin: savePayload.provPin });

              // Update Active State
              const n = savePayload.provName;
              const p = savePayload.provPin;
              setNetworkName(n);
              setNetworkPin(p);
            }
          }
        }
        // If mode switched FROM Provisioned TO Auto
        else if (initialSettings?.cluster_mode === "provisioned" && savePayload.currentMode === "auto") {
          console.log("Resetting Identity to Auto...");
          await invoke("regenerate_network_identity");
          const n = await invoke<string>("get_network_name");
          const p = await invoke<string>("get_network_pin");
          setNetworkName(n);
          setNetworkPin(p);
          setProvName(n);
          setProvPin(p);
        }

        // Sync Initial Settings to Current (to track mode changes)
        setInitialSettings(JSON.parse(JSON.stringify(savePayload.settings)));

        if (onSettingsRefreshed) onSettingsRefreshed();

      } catch (e) {
        console.error("Autosave failed", e);
        // showMessage("Error", "Autosave failed", "neutral"); // Optional
      } finally {
        setSaving(false);
      }
    };

    const timer = setTimeout(save, 800);
    return () => clearTimeout(timer);
  }, [settings, provName, provPin, networkName, networkPin, initialSettings]);
  // Added networkName/Pin/initialSettings to deps to correct closures.

  if (loading || !settings) return <div className="p-10 text-center text-zinc-500">Loading settings...</div>;

  return (
    <div className="flex h-full flex-col gap-4 overflow-y-auto pb-4">
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

      {/* Cluster Mode */}
      <Card className="p-4">
        <SectionHeader
          icon={<ShieldCheck className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Cluster Mode"
          subtitle="Manage how this device connects."
        />
        <div className="mt-4 flex flex-col gap-4 px-1">
          <div className="flex items-center gap-4 rounded-xl bg-zinc-900/5 p-1 dark:bg-white/5">
            <button
              className={clsx(
                "flex-1 rounded-lg py-1.5 text-sm font-medium transition",
                settings.cluster_mode === "auto"
                  ? "bg-white text-zinc-900 shadow-sm dark:bg-zinc-800 dark:text-zinc-50"
                  : "text-zinc-600 hover:bg-zinc-900/5 dark:text-zinc-400"
              )}
              onClick={() => setSettings({ ...settings, cluster_mode: "auto" })}
            >
              Autogenerated
            </button>
            <button
              className={clsx(
                "flex-1 rounded-lg py-1.5 text-sm font-medium transition",
                settings.cluster_mode === "provisioned"
                  ? "bg-white text-zinc-900 shadow-sm dark:bg-zinc-800 dark:text-zinc-50"
                  : "text-zinc-600 hover:bg-zinc-900/5 dark:text-zinc-400"
              )}
              onClick={() => setSettings({ ...settings, cluster_mode: "provisioned" })}
            >
              Provisioned
            </button>
          </div>

          {settings.cluster_mode === "provisioned" && (
            <div className="flex flex-col gap-3 rounded-2xl border border-zinc-900/10 bg-white/50 p-4 dark:border-white/10 dark:bg-white/5">
              <div className="flex flex-col gap-1">
                <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">Cluster Name (No Spaces)</label>
                <input
                  className="h-10 rounded-xl border border-zinc-900/10 bg-white px-3 text-sm text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-zinc-950 dark:text-zinc-50"
                  value={provName}
                  onChange={(e) => setProvName(e.target.value.replace(/\s/g, ""))}
                />
                {provName.trim().includes(" ") && <span className="text-[10px] text-rose-500">Spaces not allowed.</span>}
              </div>
              <div className="flex flex-col gap-1">
                <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">My Cluster PIN (Min 6 chars)</label>
                <input
                  className="h-10 rounded-xl border border-zinc-900/10 bg-white px-3 font-mono text-sm text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-zinc-950 dark:text-zinc-50"
                  value={provPin}
                  onChange={(e) => setProvPin(e.target.value.trim())}
                />
                {provPin.length > 0 && provPin.length < 6 && <span className="text-[10px] text-rose-500">PIN must be at least 6 characters.</span>}
                <span className="text-[11px] text-zinc-500 dark:text-zinc-400">
                  This PIN is local to this device — it's what other devices enter to pair <em>with</em> this one. Each device in a cluster keeps its own PIN; pairing doesn't share it.
                </span>
              </div>
            </div>
          )}

          {settings.cluster_mode === "auto" && (
            <div className="text-xs text-zinc-500">
              Cluster identity is randomly generated. To reset, use "Leave & Reset" in the header.
            </div>
          )}
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

      {/* File Transfer */}
      <Card className="p-4">
        <SectionHeader
          icon={<div className="h-5 w-5 flex items-center justify-center"><svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z" /><polyline points="14 2 14 8 20 8" /></svg></div>}
          title="File Transfer"
          subtitle="Manage how files are shared."
        />
        <div className="mt-4 px-1 space-y-4">
          <div className="flex items-center justify-between">
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Allow File Transfer</div>
              <div className="text-xs text-zinc-500">Send and receive files with clipboard.</div>
            </div>
            <button
              onClick={() => setSettings({ ...settings, enable_file_transfer: !settings.enable_file_transfer })}
              className={clsx("relative h-6 w-11 rounded-full transition-colors", settings.enable_file_transfer ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
            >
              <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.enable_file_transfer ? "translate-x-6" : "translate-x-1")} />
            </button>
          </div>

          {settings.enable_file_transfer && (
            <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-3 dark:border-white/10 dark:bg-white/5">
              <div className="flex flex-col gap-2">
                <div className="flex items-center justify-between">
                  <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Auto-Download Limit</div>
                  <div className="text-xs font-mono text-zinc-500">
                    {(settings.max_auto_download_size / 1024 / 1024).toFixed(0)} MB
                  </div>
                </div>
                <input
                  type="range"
                  min="0"
                  max="500"
                  step="10"
                  value={(settings.max_auto_download_size / 1024 / 1024) || 0}
                  onChange={(e) => {
                    const val = parseInt(e.target.value) * 1024 * 1024;
                    setSettings({ ...settings, max_auto_download_size: val });
                  }}
                  className="h-2 w-full cursor-pointer appearance-none rounded-lg bg-zinc-200 accent-emerald-500 dark:bg-zinc-700"
                />
                <div className="text-[10px] text-zinc-500">
                  Files larger than this must be manually downloaded.
                </div>
              </div>
            </div>
          )}

          {settings.enable_file_transfer && (
            <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-3 dark:border-white/10 dark:bg-white/5">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Compress File Transfers</div>
                  <div className="text-xs text-zinc-500">
                    Speeds up transfers of large, compressible files (text, code, logs, datasets) on slower links. Files that are already compressed (images, video, archives, etc.) are skipped automatically.
                  </div>
                </div>
                <button
                  onClick={() => {
                    if (!settings.compress_file_transfers) {
                      setCompressDialogOpen(true);
                    } else {
                      setSettings({ ...settings, compress_file_transfers: false });
                    }
                  }}
                  className={clsx("relative h-6 w-11 shrink-0 rounded-full transition-colors", settings.compress_file_transfers ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
                >
                  <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.compress_file_transfers ? "translate-x-6" : "translate-x-1")} />
                </button>
              </div>
            </div>
          )}
        </div>
      </Card>

      {/* Notifications */}
      <Card className="p-4">
        <SectionHeader
          icon={<Info className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Notifications"
          subtitle="Choose what to see."
        />
        <div className="mt-4 px-1 space-y-3">
          {[
            { label: "Device Joins", key: "device_join" as const },
            { label: "Device Leaves", key: "device_leave" as const },
            { label: "Data Sent", key: "data_sent" as const },
            { label: "Data Received", key: "data_received" as const },
          ].map(item => (
            <div key={item.key} className="flex items-center justify-between">
              <div className="text-sm text-zinc-700 dark:text-zinc-300">{item.label}</div>
              <button
                onClick={() => setSettings({
                  ...settings,
                  notifications: { ...settings.notifications, [item.key]: !settings.notifications[item.key] }
                })}
                className={clsx("relative h-5 w-9 rounded-full transition-colors", settings.notifications[item.key] ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
              >
                <span className={clsx("block h-3 w-3 transform rounded-full bg-white shadow-sm transition-transform", settings.notifications[item.key] ? "translate-x-5" : "translate-x-1")} />
              </button>
            </div>
          ))}

          {/* Large File Notification (Root Setting) */}
          <div className="flex items-center justify-between">
            <div className="text-sm text-zinc-700 dark:text-zinc-300">Large File Transfers</div>
            <button
              onClick={() => setSettings({
                ...settings,
                notify_large_files: !settings.notify_large_files
              })}
              className={clsx("relative h-5 w-9 rounded-full transition-colors", settings.notify_large_files ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
            >
              <span className={clsx("block h-3 w-3 transform rounded-full bg-white shadow-sm transition-transform", settings.notify_large_files ? "translate-x-5" : "translate-x-1")} />
            </button>
          </div>
        </div>
      </Card>

      {/* Diagnostics — opt-in verbose logging for the pairing channel. Per
          WIRE-PROTOCOL-0.3.1 §H7, the responder logs only a generic
          "pairing failure" line when this is off so observers can't tell a
          wrong-PIN attempt from any other framing/decrypt error. */}
      <Card className="p-4">
        <SectionHeader
          icon={<ShieldCheck className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Diagnostics"
          subtitle="Logging controls for the pairing channel."
        />
        <div className="mt-4 space-y-4 px-1">
          <div className="flex items-start justify-between gap-4">
            <div className="flex-1">
              <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200">
                Verbose pairing logs
              </div>
              <div className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
                Write detailed pairing-channel diagnostics to the log instead of a generic "pairing failed" line. Off by default to avoid leaking whether a failure was a wrong-PIN attempt or a different protocol error.
              </div>
            </div>
            <button
              onClick={() => setSettings({
                ...settings,
                pairing_debug_logs: !settings.pairing_debug_logs,
              })}
              className={clsx(
                "relative h-6 w-11 shrink-0 rounded-full transition-colors",
                settings.pairing_debug_logs ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700"
              )}
            >
              <span
                className={clsx(
                  "block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform",
                  settings.pairing_debug_logs ? "translate-x-6" : "translate-x-1"
                )}
              />
            </button>
          </div>
        </div>
      </Card>

      {/* Footer Status */}
      <div className="flex flex-col items-center justify-center gap-2 pt-2 pb-4 opacity-50">
        <span className={clsx("text-[10px] font-medium transition-opacity", saving ? "opacity-100 text-zinc-500" : "opacity-0 duration-1000")}>
          Saving changes...
        </span>
        <div className="text-[10px] text-zinc-400">
          ClusterCut v{version} ({__COMMIT_HASH__})
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

    </div>
  );
}
