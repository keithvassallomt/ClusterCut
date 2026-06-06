import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  Monitor, History, LogOut,
  Settings, Lock, Unlock,
  Puzzle, Loader2, Unplug
} from "lucide-react";
import clsx from "clsx";
import { Card, Button, IconButton } from "./components/ui";
import { Dialog } from "./components/Dialog";
import { DevicesView } from "./components/DevicesView";
import { HistoryView } from "./components/HistoryView";
import { SettingsView } from "./components/SettingsView";
import { ManualSyncFAB, ManualSyncModal } from "./components/ManualSync";
import { IncompatibleModal, ConnectionFailedModal, JoinModal, LeaveModal, AddRemoteModal, PortWarningModal } from "./components/Modals";
import { LegacyPeerBanner, PairingLockoutBanner } from "./components/Banners";
import type {
  Peer, View, NearbyNetwork, ClipboardBlobPreview, ClipboardFormatPreview,
  HistoryItem, AppSettings,
} from "./types";
import { blobFromPayload, formatsFromPayload } from "./lib/protocol";

// Helper for backend logging
// Helper for backend logging
const internalLogToBackend = (level: string | null, msg: string, ...args: any[]) => {
  const formatted = [msg, ...args].map(a =>
    typeof a === 'object' ? JSON.stringify(a, null, 2) : String(a)
  ).join(" ");
  invoke("log_frontend", { message: formatted, level }).catch(_err => {
    // Fallback
  });
};

const logToBackend = (msg: string, ...args: any[]) => internalLogToBackend(null, msg, ...args);

/* --- Main App Component --- */

export default function App() {
  /* Logic & State from Old App */
  const [peers, setPeers] = useState<Peer[]>([]);
  const peersRef = useRef<Peer[]>([]);

  const [clipboardHistory, setClipboardHistory] = useState<HistoryItem[]>([]);
  const [activeView, setActiveView] = useState<View>("devices");
  const [showExtensionDialog, setShowExtensionDialog] = useState(false);
  const [clipboardRequiresExtension, setClipboardRequiresExtension] = useState(false);


  const [myNetworkName, setMyNetworkName] = useState("Loading...");
  const [myHostname, setMyHostname] = useState("Loading...");
  const [networkPin, setNetworkPin] = useState("...");

  // Legacy-peer banner: surfaced when known_peers.json contains entries
  // without a stored cert fingerprint (i.e. paired before mTLS landed).
  // Those peers are unreachable under v0.3 and need to be re-paired.
  const [legacyPeers, setLegacyPeers] = useState<{ id: string; hostname: string }[]>([]);

  // Pairing-lockout banner: surfaced when the backend's pairing listener
  // tripped its global AEAD-failure threshold (WIRE-PROTOCOL-0.3.1 §H1).
  // The listener refuses inbound pairing attempts until the user clicks
  // "Re-enable pairing", which calls `rearm_pairing` on the backend.
  const [pairingLockedOut, setPairingLockedOut] = useState(false);

  // Issue #16: user-controlled pause for inbound pairing. Persists across
  // restart via AppSettings.pairing_accept_enabled.
  const [pairingAccepted, setPairingAccepted] = useState(true);

  // Incompatibility modal: fires when the backend's `peer-incompatible`
  // event arrives (a clipboard send failed and the destination peer's
  // mDNS-advertised proto version is missing or below the minimum).
  // Deduped per peer ID for the session so retrying doesn't re-pop.
  const [incompatibleModal, setIncompatibleModal] = useState<{
    open: boolean;
    hostname: string;
  }>({ open: false, hostname: "" });
  const incompatibleShownRef = useRef<Set<string>>(new Set());

  const [unsavedChanges, setUnsavedChanges] = useState(false);
  const [dialog, setDialog] = useState<{
    open: boolean;
    title: string;
    description: string;
    onConfirm: () => void;
    onCancel?: () => void;
    confirmLabel?: string;
    cancelLabel?: string;
    type?: "neutral" | "danger" | "success";
  }>({ open: false, title: "", description: "", onConfirm: () => { } });

  /* Modal State */
  const [joinOpen, setJoinOpen] = useState(false);
  const [joinTarget, setJoinTarget] = useState<string>("");
  const [joinPin, setJoinPin] = useState("");
  const [joinBusy, setJoinBusy] = useState(false);
  const [pairingPeerId, setPairingPeerId] = useState<string | null>(null);
  // Set when the join modal is opened from "Add Remote Peer" — the user-typed
  // IP[:port] is passed straight to start_pairing as the address override
  // because no mDNS-discovered peer record exists yet.
  const [pairingPeerAddr, setPairingPeerAddr] = useState<string | null>(null);

  const [leaveOpen, setLeaveOpen] = useState(false);

  const [addManualOpen, setAddManualOpen] = useState(false);
  const [manualIp, setManualIp] = useState("");
  const [manualBusy, setManualBusy] = useState(false);

  const [joinError, setJoinError] = useState("");
  const [expandedNetworks, setExpandedNetworks] = useState<Set<string>>(new Set());

  /* Port Warning State */
  const [showPortWarning, setShowPortWarning] = useState(false);
  const [currentPort, setCurrentPort] = useState(4654);

  // Manual Sync State
  // Manual Sync State
  const [manualSyncOpen, setManualSyncOpen] = useState(false);
  const [pendingReceive, setPendingReceive] = useState<{ text: string, sender: string, timestamp: number, blob?: ClipboardBlobPreview, formats?: ClipboardFormatPreview[] } | null>(null);
  const [localClipboard, setLocalClipboard] = useState(""); // Current local
  const [lastSentClipboard, setLastSentClipboard] = useState(""); // Last successfully sent
  const [lastReceivedClipboard, setLastReceivedClipboard] = useState(""); // Last received from cluster

  // We need to know if Auto-Send is ON/OFF to decide if we show "Pending Send"
  const [isAutoSend, setIsAutoSend] = useState(true); // Default assumption, updated bySettings
  const isAutoSendRef = useRef(isAutoSend);
  useEffect(() => { isAutoSendRef.current = isAutoSend; }, [isAutoSend]);

  // Rule 2: If local matches last received, not a candidate for sending.
  const hasPendingSend = !isAutoSend
    && localClipboard !== lastSentClipboard
    && localClipboard !== lastReceivedClipboard
    && localClipboard.length > 0;

  // Rule 3: If pending receive matches local (already have it or I sent it), not a candidate.
  // Image-only payloads have empty text; treat them as pending if a blob is present.
  const hasPendingReceive = !!pendingReceive
    && (
      (pendingReceive.text !== "" && pendingReceive.text !== localClipboard)
      || !!pendingReceive.blob
    );

  const toggleNetwork = (name: string) => {
    setExpandedNetworks(prev => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  // Keep ref in sync
  useEffect(() => {
    peersRef.current = peers;
  }, [peers]);

  const handleNotificationClick = async (targetView: string = "history") => {
    logToBackend(`Handling Notification Click in Frontend. Target View: ${targetView}`);
    try {
      const win = getCurrentWindow();
      await win.unminimize();
      await win.show();
      await win.setFocus();
      logToBackend(`Setting active view to ${targetView}`);
      setActiveView(targetView as any); // Cast to any to avoid strict type issues if view strings differ slightly
    } catch (e) {
      console.error("Failed to focus window:", e);
      logToBackend("Failed to focus window:", e);
    }
  };

  // ...



  const [settings, setSettings] = useState<AppSettings | null>(null);

  // Deep Link & Notification Action Handler
  useEffect(() => {
    let unlistenDeepLink: any;

    const handleArgs = (args: string[]) => {
      console.log("Checking args for deep link:", args);
      const urlStr = args.find(a => a.startsWith("clustercut://"));
      if (urlStr) {
        console.log("Found Deep Link URL:", urlStr);
        logToBackend("Deep Link Detected:", urlStr);
        if (urlStr.includes("action/show") || urlStr.includes("action/download")) {
          console.log("Action matched! Parsing view/action from URL...");
          logToBackend("Action matched, checking for view/action param.");

          let targetView = "history";
          try {
            const parsed = new URL(urlStr);

            // 1. Download Action
            if (urlStr.includes("action/download")) {
              const msgId = parsed.searchParams.get("msg_id");
              const peerId = parsed.searchParams.get("peer_id");
              const countStr = parsed.searchParams.get("file_count");

              if (msgId && peerId && countStr) {
                const count = parseInt(countStr);
                logToBackend(`Auto-download triggered via Notification: ${count} files.`);

                // Trigger downloads
                for (let i = 0; i < count; i++) {
                  invoke("request_file", { fileId: msgId, fileIndex: i, peerId: peerId }).catch(e => {
                    console.error("Failed to auto-download:", e);
                    logToBackend("Failed to auto-download:", e);
                  });
                }
              }
              targetView = "history";
            }
            // 2. Show Action
            else {
              const v = parsed.searchParams.get("view");
              if (v) targetView = v;
            }
          } catch (e) {
            console.error("Failed to parse URL:", e);
          }

          setActiveView(targetView as any);
          handleNotificationClick(targetView);
        } else {
          // Generic open
          setActiveView("history"); // Default behavior for deep link? or Devices?
        }
      }
    };

    const setupListener = async () => {
      // 1. Check Cold Start Args
      try {
        const currentArgs = await invoke<string[]>("get_launch_args");
        handleArgs(currentArgs);
      } catch (e) {
        console.error("Failed to get launch args:", e);
      }

      // 2. Listen for Runtime Deep Links (Single Instance)
      unlistenDeepLink = await listen<string[]>("deep-link", (event) => {
        console.log("Deep Link Event Received:", event);
        handleArgs(event.payload);
      });
    };

    setupListener();



    // Keep macOS plugin listener for fallback? 
    // User specifically wanted native Windows actions.
    // We can keep the plugin import for macOS only if we detect OS at runtime or just let it fail on Windows?
    // Since we removed plugin from Windows build, dynamic import might fail on Windows, which is fine (catch block).

    return () => {
      if (unlistenDeepLink) unlistenDeepLink();
    };
  }, []);

  /* Connection Failure Logic */
  const [isConnectionFailed, setIsConnectionFailed] = useState(false);
  const [connectionCheckDismissed, setConnectionCheckDismissed] = useState(false);
  const [retryCount, setRetryCount] = useState(0);
  const [hasManualPeers, setHasManualPeers] = useState(false);

  // Show the "trouble connecting?" modal only when we have manual peers AND
  // none of them are on a directly-reachable subnet. If a manual peer is local,
  // "no peers online" just means peers are offline, not a connection problem.
  useEffect(() => {
    invoke<boolean>("expects_remote_manual_peers").then(expects => {
      logToBackend("Computed expectsRemoteManualPeers:", expects);
      setHasManualPeers(expects);
    }).catch(e => logToBackend("Error fetching expects_remote_manual_peers:", e));
  }, [settings, retryCount]); // Re-check if settings change or we retry

  useEffect(() => {
    if (!settings) return;

    // Check ONLY if we have explicit "Manual" peers (which implies Remote/VPN).
    const shouldCheck = hasManualPeers;

    // Show connecting state if we are checking, have no peers, and haven't failed yet.
    // We use a small delay to avoid flashing if connection is instant (though 0 peers usually implies waiting).
    // Actually, usually immediate.

    logToBackend("Connection Check: Mode =", settings.cluster_mode, "HasManual =", hasManualPeers, "Should Check =", shouldCheck, "Peers =", peers.length);

    if (shouldCheck && !connectionCheckDismissed) {
      if (peers.length > 0) {
        logToBackend("Connection Check: Peers found. Clearing failure state.");
        setIsConnectionFailed(false);
        setConnectionCheckDismissed(false); // Reset dismissal on success
        return;
      }

      logToBackend("Connection Check: No peers. Starting timer...");
      const timer = setTimeout(() => {
        setIsConnectionFailed(true);
        logToBackend("Connection Check: Timeout reached. Showing modal.");
      }, 15000); // 15s

      return () => clearTimeout(timer);
    } else {
      setIsConnectionFailed(false);
    }
  }, [settings, peers.length, retryCount, hasManualPeers, connectionCheckDismissed]);

  const handleRetryConnection = async () => {
    setIsConnectionFailed(false);
    setConnectionCheckDismissed(false);
    setRetryCount(c => c + 1);
    await invoke("retry_connection");
  };

  const handleConnectionFailureLeave = async () => {
    setIsConnectionFailed(false);
    try {
      await invoke("leave_network");
    } catch (e) { logToBackend("Error leaving network:", e); }
  };


  /* Data Fetching */
  const fetchSettings = () => {
    invoke<AppSettings>("get_settings").then(s => {
      setSettings(s);
      setIsAutoSend(s.auto_send);
    });
  };

  // GNOME Extension Check
  // Use a ref to ensure we only show the dialog once per session if not ignored permanently
  const hasCheckedExtension = useRef(false);

  useEffect(() => {
    if (!hasCheckedExtension.current) {
      hasCheckedExtension.current = true;

      invoke<{ is_gnome: boolean, is_installed: boolean, clipboard_requires_extension: boolean }>('check_gnome_extension_status')
        .then(status => {
          if (status.clipboard_requires_extension) {
            // On GNOME Wayland without extension: always show, ignore the "don't ask" setting
            setClipboardRequiresExtension(true);
            setShowExtensionDialog(true);
          } else if (status.is_gnome && !status.is_installed && settings?.ignore_extension_missing === false) {
            setShowExtensionDialog(true);
          }
        })
        .catch(e => console.error("Failed to check extension status:", e));
    }
  }, [settings]);

  const handleInstallExtension = () => {
    const extUrl = "https://extensions.gnome.org/extension/9341/clustercut/";
    openUrl(extUrl).catch((e: unknown) => {
        console.error("Failed to open URL via plugin-opener:", e);
        // Fallback to window.open (might be blocked by CSP or Tauri config)
        window.open(extUrl, "_blank");
    });
    setShowExtensionDialog(false);
  };

  const handleIgnoreExtension = async () => {
    if (settings) {
      const newSettings = { ...settings, ignore_extension_missing: true };
      await invoke("save_settings", { settings: newSettings });
      setSettings(newSettings);
      setShowExtensionDialog(false);
    }
  };



  // Initial Data Fetch
  useEffect(() => {
    // 1. Peers
    invoke<Record<string, Peer>>("get_peers").then((peerMap) => {
      setPeers(Object.values(peerMap));
    });

    // 2. Metadata
    invoke<string>("get_network_name").then(name => setMyNetworkName(name));
    invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
    invoke<string>("get_hostname").then(h => setMyHostname(h));

    // 2b. Legacy-peer banner: only non-empty after a v0.2 → v0.3 upgrade
    // where stored peers lack pinned cert fingerprints.
    invoke<{ id: string; hostname: string }[]>("get_legacy_peers")
      .then(setLegacyPeers)
      .catch(() => {});

    // 2c. Pairing-lockout banner — initial fetch. The backend retains
    // the locked-out state across the session (and across a UI reload
    // within the same process lifetime), so we surface it on mount.
    invoke<boolean>("is_pairing_locked_out")
      .then(setPairingLockedOut)
      .catch(() => {});

    // Issue #16: initial fetch for the user-controlled pairing toggle.
    invoke<boolean>("get_pairing_accept")
      .then(setPairingAccepted)
      .catch(() => {});

    // 3. Settings
    fetchSettings();

    // 4. Port Check
    invoke<number>("get_listening_port").then(port => {
      if (port !== 4654) {
        setCurrentPort(port);
        setShowPortWarning(true);
      }
    });
  }, []);

  // Poll/Update PIN when network name changes
  useEffect(() => {
    invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
  }, [myNetworkName]);

  // Listeners
  useEffect(() => {
    if (!myHostname) return; // Wait for identity to prevent false "remote" detection

    const unlistenPeer = listen<Peer>("peer-update", (event) => {
      setPeers((prev) => {
        const exists = prev.find((p) => p.id === event.payload.id);
        if (exists) return prev.map((p) => (p.id === event.payload.id ? event.payload : p));
        return [...prev, event.payload];
      });
    });

    // Pairing completion is signalled by a dedicated event from the backend
    // rather than overloading `peer-update` — mDNS rediscovery of an
    // already-known peer also fires `peer-update` with is_trusted=true and
    // would otherwise race the PIN dialog closed before the user can submit.
    const unlistenPairingSuccess = listen<string>("pairing-success", () => {
      setJoinOpen(false);
      setJoinBusy(false);
      invoke<string>("get_network_name").then(name => setMyNetworkName(name));
      invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
    });

    // Incompatible-peer modal: backend emits this when a user-triggered
    // send (clipboard) fails AND the destination peer's mDNS-advertised
    // proto version is missing or older than this build's minimum.
    const unlistenIncompatible = listen<{ id: string; hostname: string }>(
      "peer-incompatible",
      (event) => {
        const { id, hostname } = event.payload;
        if (incompatibleShownRef.current.has(id)) return;
        incompatibleShownRef.current.add(id);
        setIncompatibleModal({ open: true, hostname });
        // If the join-cluster modal is mid-flight (pre-flight version
        // check fired by start_pairing), close it so the upgrade prompt
        // isn't stacked on top of a spinner / inline error.
        setJoinOpen(false);
        setJoinBusy(false);
      },
    );

    // Pairing lockout (WIRE-PROTOCOL-0.3.1 §H1). The backend already fires
    // an urgent OS notification when the threshold trips; this listener
    // also raises an in-app banner so the lockout is visible the next time
    // the user looks at the window.
    const unlistenPairingLocked = listen<void>("pairing-locked-out", () => {
      setPairingLockedOut(true);
    });
    const unlistenPairingRearmed = listen<void>("pairing-rearmed", () => {
      setPairingLockedOut(false);
    });

    // Issue #16: keep state in sync with any other surface that might toggle
    // the flag (tray menu in the future, etc.).
    const unlistenPairingAcceptChanged = listen<boolean>("pairing-accept-changed", (event) => {
      setPairingAccepted(event.payload);
    });

    // Listen for Monitor Updates (When Auto-Send is OFF)
    const unlistenMonitor = listen<any>("clipboard-monitor-update", (event) => {
      console.log("Monitor Update (Auto-Send OFF):", event.payload);
      const p = event.payload;
      // This event ONLY comes from local backend monitoring

      const newItem: HistoryItem = {
        id: p.id,
        origin: "local",
        device: "Me", // It's always me for monitor updates
        sender_id: p.sender_id,
        ts: p.timestamp,
        text: p.text || "",
        files: p.files,
        blob: blobFromPayload(p.blob),
        formats: formatsFromPayload(p.formats),
      };

      // Update Local State but NOT 'lastSentClipboard'
      if (newItem.text) {
        setLocalClipboard(newItem.text);
        // Do NOT set lastSentClipboard here, because we haven't sent it yet!
        // This discrepancy (local > lastSent) will trigger the FAB.
      }
    });

    // Listen for Clipboard Changes
    const unlistenClipboard = listen<any>("clipboard-change", (event) => {
      console.log("Clipboard Changed Event:", event.payload);

      const p = event.payload;
      const isLocal = p.sender === "self" || p.sender === myHostname;

      // Construct History Item immediately
      const newItem: HistoryItem = {
        id: p.id,
        origin: isLocal ? "local" : "remote",
        device: p.sender,
        sender_id: p.sender_id,
        ts: p.timestamp,
        text: p.text || "",
        files: p.files,
        blob: blobFromPayload(p.blob),
        formats: formatsFromPayload(p.formats),
      };

      // Update Local Clipboard State
      if (isLocal) {
        // If it has text, update local view
        if (newItem.text) setLocalClipboard(newItem.text);
        // If local change event -> it is committed (Auto or Manual).
        if (newItem.text) setLastSentClipboard(newItem.text);
      } else {
        // Remote sender
        if (newItem.text) {
          setLocalClipboard(newItem.text);
          setLastReceivedClipboard(newItem.text);
        }
      }

      // Update History
      setClipboardHistory((prev) => {
        // Dedupe by ID — discard the freshly-allocated blob URL to avoid leak.
        if (prev.find(i => i.id === newItem.id)) {
          if (newItem.blob?.object_url) URL.revokeObjectURL(newItem.blob.object_url);
          return prev;
        }
        const next = [newItem, ...prev];
        if (next.length > 50) {
          next.slice(50).forEach(item => {
            if (item.blob?.object_url) URL.revokeObjectURL(item.blob.object_url);
          });
        }
        return next.slice(0, 50);
      });
    });

    const unlistenPending = listen<any>("clipboard-pending", (event) => {
      const p = event.payload;
      // Replacing an existing pending entry — revoke its blob URL first if any.
      setPendingReceive(prev => {
        if (prev?.blob?.object_url) URL.revokeObjectURL(prev.blob.object_url);
        return {
          text: p.text || "",
          sender: p.sender,
          timestamp: p.timestamp,
          blob: blobFromPayload(p.blob),
          formats: formatsFromPayload(p.formats),
        };
      });
    });

    const unlistenDelete = listen<string>("history-delete", (event) => {
      const idToDelete = event.payload;
      setClipboardHistory((prev) => {
        const dropped = prev.find(i => i.id === idToDelete);
        if (dropped?.blob?.object_url) URL.revokeObjectURL(dropped.blob.object_url);
        return prev.filter(i => i.id !== idToDelete);
      });
    });

    const unlistenRemove = listen<string>("peer-remove", (event) => {
      setPeers((prev) => prev.filter(p => p.id !== event.payload));
    });

    const unlistenReset = listen("network-reset", () => {
      // Reload app to reset state
      window.location.reload();
    });

    const unlistenUpdate = listen("network-update", () => {
      invoke<string>("get_network_name").then(name => setMyNetworkName(name));
      invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
    });

    const unlistenPairingFailed = listen<string>("pairing-failed", (event) => {
      // Show error in the join modal
      setJoinError(event.payload);
      setJoinBusy(false);
    });





    // Linux (Custom notify-rust) & macOS (user-notify)
    const unlistenNotification = listen<any>("notification-clicked", (event) => {
      console.log("Custom notification clicked event:", event);
      logToBackend("Frontend received notification-clicked event:", event);
      const view = event.payload?.view || "history";
      handleNotificationClick(view);
    });

    const unlistenSettingsChanged = listen<AppSettings>("settings-changed", (event) => {
      setIsAutoSend(event.payload.auto_send);
    });

    return () => {
      unlistenPeer.then((f) => f());
      unlistenClipboard.then((f) => f());
      unlistenMonitor.then((f) => f());

      unlistenPending.then((f) => f());
      unlistenRemove.then((f) => f());
      unlistenReset.then((f) => f());
      unlistenUpdate.then((f) => f());
      unlistenDelete.then((f) => f());
      unlistenPairingFailed.then((f) => f());
      unlistenPairingSuccess.then((f) => f());
      unlistenIncompatible.then((f) => f());
      unlistenNotification.then((f) => f());
      unlistenSettingsChanged.then((f) => f());
      unlistenPairingLocked.then((f) => f());
      unlistenPairingRearmed.then((f) => f());
      unlistenPairingAcceptChanged.then((f) => f());
    };
  }, [myHostname]); // Re-bind if hostname loads (needed for sender check)

  /* Handlers */

  const startJoinFlow = (networkName: string, targetPeerId: string) => {
    setJoinTarget(networkName);
    setPairingPeerId(targetPeerId);
    setPairingPeerAddr(null);
    setJoinPin("");
    setJoinError("");
    setJoinBusy(false);
    setJoinOpen(true);
  };

  // Manual-remote variant: no mDNS-observed peer exists yet, so we pass the
  // user-typed address straight through to start_pairing as the override.
  const startManualPairFlow = (addr: string) => {
    setJoinTarget(addr);
    setPairingPeerId(null);
    setPairingPeerAddr(addr);
    setJoinPin("");
    setJoinError("");
    setJoinBusy(false);
    setJoinOpen(true);
  };

  const submitJoin = async () => {
    if (!joinPin) return;
    if (!pairingPeerId && !pairingPeerAddr) return;
    setJoinBusy(true);
    setJoinError("");

    try {
      await invoke("start_pairing", {
        peerId: pairingPeerId ?? "",
        pin: joinPin,
        peerAddr: pairingPeerAddr,
      });
      // Note: Backend handles the rest. We wait for peer-update event to close modal.
      // Timeout safety
      setTimeout(() => {
        setJoinBusy(false);
      }, 5000);
    } catch (e) {
      setJoinError(String(e));
      setJoinBusy(false);
    }
  };

  const handleViewChange = (view: View) => {
    if (view === activeView) return;

    if (unsavedChanges && activeView === "settings") {
      setDialog({
        open: true,
        title: "Unsaved Changes",
        description: "You have unsaved changes in Settings. Switching tabs will discard them. Are you sure?",
        type: "danger",
        confirmLabel: "Discard Changes",
        onConfirm: () => {
          setUnsavedChanges(false);
          setActiveView(view);
          setDialog(d => ({ ...d, open: false }));
        },
        onCancel: () => setDialog(d => ({ ...d, open: false }))
      });
    } else {
      setActiveView(view);
    }
  };



  const confirmLeaveNetwork = async () => {
    setLeaveOpen(false);
    try {
      await invoke("leave_network");
    } catch (e) {
      alert("Failed to leave network: " + e);
    }
  };

  const deletePeer = async (id: string) => {
    if (!confirm("Kick/Ban this device from the network?")) return;
    try {
      await invoke("delete_peer", { peerId: id });
      setPeers((prev) => prev.filter(p => p.id !== id));
    } catch (e) {
      alert("Failed to delete peer: " + String(e));
    }
  };


  const submitManualPeer = async () => {
    if (!manualIp) return;
    const input = manualIp.trim();
    // CIDR scans existing peers on a subnet and still relies on pinned
    // fingerprints — it's a re-discovery tool, not a first-pair entry point.
    // A single IP[:port] is the "Add Remote Peer" first-pair case: hand it
    // to the existing PIN modal, which will run start_pairing over the
    // plaintext-TCP pairing channel against the typed address.
    if (input.includes("/")) {
      setManualBusy(true);
      try {
        await invoke("add_manual_peer", { ip: input });
        setAddManualOpen(false);
        setManualIp("");
      } catch (e) {
        alert("Failed: " + e);
      } finally {
        setManualBusy(false);
      }
    } else {
      // Single IP: try a direct connect to an already-paired peer first
      // (issue #18). Only fall back to the PIN/pairing modal if we don't
      // recognise this address.
      setManualBusy(true);
      try {
        const outcome = await invoke<"connected" | "needs_pairing">("add_remote_peer", { ip: input });
        setAddManualOpen(false);
        setManualIp("");
        if (outcome === "needs_pairing") {
          startManualPairFlow(input);
        }
      } catch (e) {
        alert("Failed: " + e);
      } finally {
        setManualBusy(false);
      }
    }
  };

  /* Derived State */
  const myPeers = peers.filter(p => p.is_trusted);
  const untrustedPeers = peers.filter(p => !p.is_trusted);
  const isConnected = true; // Always "connected" to local discovery at least. Or use myPeers.length > 0 if that implies connection.

  // Group untrusted by network name
  const nearbyNetworks: NearbyNetwork[] = [];
  const grouped: Record<string, Peer[]> = {};

  untrustedPeers.forEach(p => {
    // Skip own network
    // if (p.network_name === myNetworkName) return;

    const name = p.network_name || "Unidentified";
    if (!grouped[name]) grouped[name] = [];
    grouped[name].push(p);
  });

  Object.entries(grouped).forEach(([name, devices]) => {
    nearbyNetworks.push({
      networkName: name,
      devices: devices.map(d => ({
        id: d.id,
        hostname: d.hostname,
        // Map backend 'last_seen' to status? current backend removes ancient peers so assume online if present
        status: "online",
        incompatible: !d.compatible,
      }))
    });
  });

  /* Theme Setup */
  /* Theme Setup */
  /* Theme Setup */
  useEffect(() => {
    let active = true;
    let cleanupListener: (() => void) | undefined;

    const applySystemTheme = () => {
      if (window.matchMedia('(prefers-color-scheme: dark)').matches) {
        document.documentElement.classList.add("dark");
      } else {
        document.documentElement.classList.remove("dark");
      }
    };

    invoke<string | null>("get_theme_override").then((theme) => {
      if (!active) return;

      if (theme === "light") {
        invoke("log_frontend", { message: "Theme Override Detected: LIGHT. Forcing light mode." });
        document.documentElement.classList.remove("dark");
      } else if (theme === "dark") {
        invoke("log_frontend", { message: "Theme Override Detected: DARK. Forcing dark mode." });
        document.documentElement.classList.add("dark");
      } else {
        invoke("log_frontend", { message: "No Theme Override. Using System Preference." });
        
        // Initial Check: Try backend state first (reliable for Linux), faillback to media query
        invoke<string | null>("get_current_theme").then(current => {
             if (!active) return;
             if (current === "prefer-dark" || current === "dark") {
                 document.documentElement.classList.add("dark");
             } else if (current === "default" || current === "light") {
                 document.documentElement.classList.remove("dark");
             } else {
                 applySystemTheme();
             }
        }).catch(() => applySystemTheme());
        
        // Listen for system changes via CSS (primary mechanism for Windows/macOS)
        const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
        const handler = () => {
           // We might want to respect backend state more? But CSS is usually faster for local.
           // However, on Linux CSS query might not update if not integrated.
           if (active) applySystemTheme();
        };
        mediaQuery.addEventListener("change", handler);
        
        // Listen for Tauri/ClusterCut theme event
        // This handles:
        // 1. Windows/macOS native theme changes (if mapped)
        // 2. Linux manual polling events (now emitting "dark" or "light")
        let unlistenTheme: (() => void) | undefined;

        listen<string>("tauri://theme-changed", (event) => {
            invoke("log_frontend", { message: `Tauri Theme Changed Event: ${event.payload}` });
            const theme = event.payload;
            if (theme === 'dark' || theme === 'prefer-dark') {
                document.documentElement.classList.add("dark");
            } else {
                document.documentElement.classList.remove("dark");
            }
        }).then(u => { unlistenTheme = u; });

        // Set cleanups
        cleanupListener = () => {
            mediaQuery.removeEventListener("change", handler);
            if (unlistenTheme) unlistenTheme();
        };
      }
    }).catch(e => console.error("Failed to get theme override:", e));

    return () => {
      active = false;
      if (cleanupListener) cleanupListener();
    };
  }, []);

  // System preference: we use CSS media queries now.

  return (
    <div className={clsx("min-h-screen w-full bg-zinc-50 dark:bg-zinc-950 bg-[radial-gradient(1200px_circle_at_0%_0%,rgba(16,185,129,0.10),transparent_60%),radial-gradient(1000px_circle_at_100%_0%,rgba(59,130,246,0.10),transparent_55%),radial-gradient(900px_circle_at_50%_100%,rgba(99,102,241,0.10),transparent_50%)] dark:bg-[radial-gradient(1200px_circle_at_0%_0%,rgba(16,185,129,0.12),transparent_60%),radial-gradient(1000px_circle_at_100%_0%,rgba(59,130,246,0.10),transparent_55%),radial-gradient(900px_circle_at_50%_100%,rgba(244,63,94,0.10),transparent_50%)] md:h-screen md:overflow-hidden")}>

      <Dialog {...dialog} />

      {/* Legacy-peer (pre-mTLS) re-pair banner. Pre-v0.3 pairings have no
          stored cert fingerprint; under the new strict-mTLS transport
          they can't receive clipboard data until re-paired via the PIN
          flow on the Devices tab. */}
      {legacyPeers.length > 0 && (
        <LegacyPeerBanner
          peers={legacyPeers}
          onDismiss={() => {
            invoke("dismiss_legacy_peer_banner").catch(() => {});
            setLegacyPeers([]);
          }}
        />
      )}

      {/* Pairing-lockout banner — fires when the responder's global
          AEAD-failure threshold trips per WIRE-PROTOCOL-0.3.1 §H1. The
          pairing TCP listener refuses inbound connections until the user
          explicitly re-arms via the button below. Red (not amber) because
          the cause is genuinely adversarial (or a misbehaving peer), not
          a benign configuration drift. */}
      {pairingLockedOut && (
        <PairingLockoutBanner
          onReEnable={() => {
            invoke("rearm_pairing")
              .then(() => setPairingLockedOut(false))
              .catch((err) => {
                logToBackend("rearm_pairing failed", err);
              });
          }}
        />
      )}

      {/* Incompatible-peer modal — fires when a clipboard send hits a
          peer that's advertising a pre-mTLS protocol version (or none). */}
      <IncompatibleModal
        open={incompatibleModal.open}
        hostname={incompatibleModal.hostname}
        onClose={() => setIncompatibleModal({ open: false, hostname: "" })}
      />

      {/* Connection Failure Modal */}
      <ConnectionFailedModal
        open={isConnectionFailed}
        onRetry={handleRetryConnection}
        onLeave={handleConnectionFailureLeave}
        onExit={() => invoke("exit_app")}
        onDoNothing={() => {
          setIsConnectionFailed(false);
          setConnectionCheckDismissed(true);
        }}
      />

      {/* GNOME Extension Dialog */}
      {showExtensionDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm p-4">
          <Card className={`max-w-md w-full p-6 space-y-4 shadow-2xl ${clipboardRequiresExtension ? 'border-amber-500/40' : 'border-indigo-500/20'}`}>
            <div className={`flex items-center gap-3 ${clipboardRequiresExtension ? 'text-amber-500' : 'text-indigo-500'}`}>
              <div className={`p-3 rounded-full ${clipboardRequiresExtension ? 'bg-amber-500/10' : 'bg-indigo-500/10'}`}>
                <Puzzle className="w-8 h-8" />
              </div>
              <h2 className="text-xl font-semibold text-slate-900 dark:text-white">
                {clipboardRequiresExtension ? 'Extension Required for Clipboard Sync' : 'Enable GNOME Integration'}
              </h2>
            </div>

            {clipboardRequiresExtension ? (
              <>
                <p className="text-slate-600 dark:text-zinc-400">
                  <strong>Clipboard synchronisation will not work</strong> without the ClusterCut GNOME extension.
                </p>
                <p className="text-slate-600 dark:text-zinc-400 text-sm">
                  GNOME on Wayland does not allow background apps to access the clipboard. The extension runs inside the compositor and bridges clipboard changes to ClusterCut. Without it, clipboard monitoring is disabled.
                </p>
              </>
            ) : (
              <>
                <p className="text-slate-600 dark:text-zinc-400">
                  It looks like you are running GNOME, but the <strong>ClusterCut Extension</strong> is not installed.
                </p>
                <p className="text-slate-600 dark:text-zinc-400 text-sm">
                  Installing the extension allows you to control ClusterCut directly from the Quick Settings menu.
                </p>
              </>
            )}

            {!clipboardRequiresExtension && (
              <div className="flex items-center space-x-2 pt-2">
                <input
                  type="checkbox"
                  id="dontAsk"
                  className="w-4 h-4 rounded border-slate-300 dark:border-zinc-700 text-indigo-600 focus:ring-indigo-500 bg-transparent"
                  onChange={(e) => {
                    if (e.target.checked) {
                      handleIgnoreExtension();
                    }
                  }}
                />
                <label htmlFor="dontAsk" className="text-sm text-slate-500 dark:text-zinc-500 select-none cursor-pointer">
                  Don't ask me again
                </label>
              </div>
            )}

            <div className="flex justify-end gap-3 pt-2">
              {!clipboardRequiresExtension && (
                <Button
                  variant="default"
                  onClick={() => setShowExtensionDialog(false)}
                >
                  No Thanks
                </Button>
              )}
              <Button
                variant="primary"
                onClick={handleInstallExtension}
              >
                Install Extension
              </Button>
              {clipboardRequiresExtension && (
                <Button
                  variant="default"
                  onClick={() => setShowExtensionDialog(false)}
                >
                  Continue Without Clipboard
                </Button>
              )}
            </div>
          </Card>
        </div>
      )}

      <div className="mx-auto flex min-h-screen w-full max-w-6xl flex-col px-4 py-6 md:h-full md:min-h-0 md:px-6">
        {/* Custom titlebar drag region */}
        <div className="drag-region h-[10px] w-full rounded-t-3xl" />

        {/* Header */}
        <header className="flex items-center justify-between mb-4 shrink-0 px-2">
          <div className="flex items-center gap-3">
            <img src="/logo.png" alt="Logo" className="h-10 w-10 drop-shadow-sm" />
            <h1 className="text-xl font-bold tracking-tight text-zinc-900 dark:text-zinc-50">
              ClusterCut
            </h1>
          </div>

          <div className="flex items-center gap-2">
            <IconButton
              label="Devices"
              active={activeView === "devices"}
              onClick={() => handleViewChange("devices")}
            >
              <Monitor className="h-5 w-5" />
            </IconButton>

            <IconButton
              label="History"
              active={activeView === "history"}
              onClick={() => handleViewChange("history")}
            >
              <History className="h-5 w-5" />
            </IconButton>

            <IconButton
              label="Settings"
              active={activeView === "settings"}
              onClick={() => handleViewChange("settings")}
            >
              <Settings className="h-5 w-5" />
            </IconButton>

            <div className="mx-2 h-6 w-px bg-zinc-200 dark:bg-zinc-700" />

            {/* Issue #16: pairing-accept toggle. Tri-state visual:
                  green Unlock  = accepting
                  gray  Lock    = user-paused
                  rose  Lock    = abuse-locked-out (non-interactive; the
                                  red banner remains the rearm path) */}
            {(() => {
              const pairingState = pairingLockedOut
                ? "locked"
                : pairingAccepted
                  ? "accepting"
                  : "paused";
              const label =
                pairingState === "locked"
                  ? "Pairing locked — too many failed attempts"
                  : pairingState === "accepting"
                    ? "Pairing accepted"
                    : "Pairing paused";
              return (
                <IconButton
                  label={label}
                  disabled={pairingState === "locked"}
                  onClick={() => {
                    if (pairingState === "locked") return;
                    const next = !pairingAccepted;
                    setPairingAccepted(next);
                    invoke("set_pairing_accept", { enabled: next }).catch((err) => {
                      logToBackend("set_pairing_accept failed", err);
                      setPairingAccepted(!next);
                    });
                  }}
                >
                  {pairingState === "accepting" ? (
                    <Unlock className="h-5 w-5 text-emerald-500" />
                  ) : pairingState === "paused" ? (
                    <Lock className="h-5 w-5 text-zinc-400" />
                  ) : (
                    <Lock className="h-5 w-5 text-rose-500" />
                  )}
                </IconButton>
              );
            })()}

            <IconButton
              danger
              onClick={() => setLeaveOpen(true)}
              label="Leave Cluster"
            >
              <Unplug className="h-5 w-5" />
            </IconButton>

            <IconButton
              onClick={() => invoke("exit_app")}
              label="Exit App"
            >
              <LogOut className="h-5 w-5" />
            </IconButton>
          </div>
        </header>

        {/* Content */}
        <div className="flex-1 min-h-0 overflow-hidden rounded-3xl border border-zinc-200 bg-white/50 shadow-sm backdrop-blur-xl dark:border-white/5 dark:bg-zinc-900/50">
          <div className="no-drag h-full">
            {activeView === "devices" ? (
              <DevicesView
                isConnected={isConnected}
                myNetworkName={myNetworkName}
                myHostname={myHostname}
                networkPin={networkPin}
                peers={myPeers}
                nearby={nearbyNetworks}
                expandedNetworks={expandedNetworks}
                toggleNetwork={toggleNetwork}
                onJoin={(netName) => {
                  const group = grouped[netName];
                  if (group && group.length > 0) {
                    startJoinFlow(netName, group[0].id);
                  }
                }}
                onDeletePeer={deletePeer}
                onAddManual={() => setAddManualOpen(true)}
              />
            ) : activeView === "history" ? (
              <HistoryView
                items={clipboardHistory}
                onClearHistory={() => {
                  setDialog({
                    open: true,
                    title: "Clear clipboard history?",
                    description: "This removes every entry from this device's history view. Other devices in the cluster keep their own history.",
                    type: "danger",
                    confirmLabel: "Clear History",
                    onConfirm: () => {
                      setClipboardHistory([]);
                      setDialog(d => ({ ...d, open: false }));
                    },
                    onCancel: () => setDialog(d => ({ ...d, open: false })),
                  });
                }}
              />
            ) : (
              <SettingsView onSettingsRefreshed={fetchSettings} hasClusterPeers={myPeers.length > 0} />
            )}
          </div>
        </div>

        <ManualSyncFAB
          hasPendingSend={hasPendingSend}
          hasPendingReceive={hasPendingReceive}
          onClick={() => setManualSyncOpen(true)}
        />

        <ManualSyncModal
          open={manualSyncOpen}
          onClose={() => setManualSyncOpen(false)}
          localContent={localClipboard}
          remoteContent={pendingReceive}
          onSend={async () => {
            try {
              await invoke("send_clipboard", { text: localClipboard });
              // Store strict equality check for "Last Sent" to avoid re-triggering pending send
              setLastSentClipboard(localClipboard);
              setManualSyncOpen(false);
            } catch (e) {
              alert("Failed to send: " + e);
            }
          }}
          onReceive={async () => {
            try {
              await invoke("confirm_pending_clipboard");
              setPendingReceive(null);
              setManualSyncOpen(false);
            } catch (e) {
              alert("Failed to confirm: " + e);
            }
          }}
        />

        {/* Modals */}
        <JoinModal
          open={joinOpen}
          joinTarget={joinTarget}
          joinPin={joinPin}
          joinError={joinError}
          joinBusy={joinBusy}
          onPinChange={(value) => setJoinPin(value)}
          onClearError={() => setJoinError("")}
          onSubmit={submitJoin}
          onClose={() => setJoinOpen(false)}
        />

        <LeaveModal
          open={leaveOpen}
          onConfirm={confirmLeaveNetwork}
          onClose={() => setLeaveOpen(false)}
        />

        <AddRemoteModal
          open={addManualOpen}
          manualIp={manualIp}
          manualBusy={manualBusy}
          onIpChange={(value) => setManualIp(value)}
          onSubmit={submitManualPeer}
          onClose={() => setAddManualOpen(false)}
        />
      </div>

      {/* Reconnecting Overlay */}
      {settings && hasManualPeers && peers.length === 0 && !isConnectionFailed && !connectionCheckDismissed && (
        <div className="fixed inset-0 z-[60] flex flex-col items-center justify-center bg-white/80 backdrop-blur-sm dark:bg-zinc-950/80">
          <Loader2 className="h-12 w-12 animate-spin text-indigo-500 mb-4" />
          <div className="text-xl font-medium text-zinc-900 dark:text-zinc-50">Connecting to remote cluster...</div>
        </div>
      )}
      {/* Port Warning Modal */}
      <PortWarningModal
        open={showPortWarning}
        currentPort={currentPort}
        onClose={() => setShowPortWarning(false)}
      />

    </div>
  );
}
