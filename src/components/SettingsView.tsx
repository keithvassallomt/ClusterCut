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

export function SettingsView({
  onSettingsRefreshed,
  hasClusterPeers = false,
}: {
  onSettingsRefreshed?: () => void;
  hasClusterPeers?: boolean;
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
  const [autoRenameDialogOpen, setAutoRenameDialogOpen] = useState(false);
  const [activeCategory, setActiveCategory] = useState<SettingsCategory>("general");

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
