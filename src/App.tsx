import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { 
  Monitor, Copy, History, ShieldCheck, PlusCircle, Trash2, LogOut, 
  Settings, Wifi, Lock, Unlock, AlertTriangle, Info, CheckCircle2,
  ChevronDown, ChevronRight, ArrowUp, ArrowDown, Send, Download
} from "lucide-react";
import clsx from "clsx";
import { ShortcutRecorder } from "./components/ShortcutRecorder";

/* --- Types --- */

interface Peer {
  id: string;
  ip: string;
  hostname: string;
  port: number;
  last_seen: number;
  is_trusted: boolean;
  is_manual?: boolean;
  network_name?: string;
  platform?: string; // Backend doesn't send this yet, will mock or infer
}

type View = "devices" | "history" | "settings";

type NearbyNetwork = {
  networkName: string;
  devices: { id: string; hostname?: string; status: "online" | "offline" }[];
};

type HistoryItem = {
    id: string;
    origin: "local" | "remote";
    device: string; // The sender's hostname
    ts: number; // Unix timestamp in seconds
    text: string;
    files?: { name: string; size: number; }[];
    sender_id?: string;
};

// Simple Time Ago Helper
function timeAgo(ts: number): string {
    const now = Math.floor(Date.now() / 1000);
    const diff = now - ts;
    if (diff < 10) return "Just now";
    if (diff < 60) return `${diff}s ago`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
    if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
    return new Date(ts * 1000).toLocaleDateString();
}

function formatBytes(bytes: number, decimals = 1) {
    if (!+bytes) return '0 Bytes';
    const k = 1024;
    const dm = decimals < 0 ? 0 : decimals;
    const sizes = ['Bytes', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB', 'ZB', 'YB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return `${parseFloat((bytes / Math.pow(k, i)).toFixed(dm))} ${sizes[i]}`;
}


interface NotificationSettings {
  device_join: boolean;
  device_leave: boolean;
  data_sent: boolean;
  data_received: boolean;
}

interface AppSettings {
  custom_device_name: string | null;
  cluster_mode: "auto" | "provisioned";
  auto_send: boolean;
  auto_receive: boolean;
  notifications: NotificationSettings;
  shortcut_send: string | null;
  shortcut_receive: string | null;
  enable_file_transfer: boolean;
  max_auto_download_size: number;
  notify_large_files: boolean;
}

/* --- Helper Components (from Design) --- */
// ... (Badge, SectionHeader, Card omitted as they are fine, just fixing Button props below)

function Badge({
  tone = "neutral",
  children,
}: {
  tone?: "neutral" | "good" | "warn" | "bad";
  children: React.ReactNode;
}) {
  const classes =
    tone === "good"
      ? "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300 border-emerald-500/25"
      : tone === "warn"
      ? "bg-amber-500/15 text-amber-700 dark:text-amber-300 border-amber-500/25"
      : tone === "bad"
      ? "bg-rose-500/15 text-rose-700 dark:text-rose-300 border-rose-500/25"
      : "bg-zinc-500/10 text-zinc-700 dark:text-zinc-300 border-zinc-500/20";

  return (
    <span className={clsx("inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium", classes)}>
      {children}
    </span>
  );
}

function SectionHeader({
  icon,
  title,
  subtitle,
  right,
}: {
  icon: React.ReactNode;
  title: string;
  subtitle?: string;
  right?: React.ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-3">
      <div className="flex items-start gap-3">
        <div className="mt-0.5 inline-flex h-9 w-9 items-center justify-center rounded-xl bg-zinc-900/5 dark:bg-white/5">
          {icon}
        </div>
        <div>
          <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-50">{title}</div>
          {subtitle ? <div className="text-xs text-zinc-600 dark:text-zinc-400">{subtitle}</div> : null}
        </div>
      </div>
      {right ? <div className="flex items-center gap-2">{right}</div> : null}
    </div>
  );
}

function Card({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <div
      className={clsx(
        "rounded-2xl border border-zinc-900/10 bg-white/70 shadow-sm backdrop-blur dark:border-white/10 dark:bg-zinc-900/40",
        className
      )}
    >
      {children}
    </div>
  );
}

function Button({
  variant = "default",
  size = "md",
  iconLeft,
  iconRight,
  children,
  className,
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "default" | "primary" | "ghost" | "danger";
  size?: "sm" | "md";
  iconLeft?: React.ReactNode;
  iconRight?: React.ReactNode;
}) {
  const base =
    "inline-flex select-none items-center justify-center gap-2 rounded-xl font-medium transition focus:outline-none focus:ring-2 focus:ring-emerald-500/40 disabled:opacity-50 disabled:cursor-not-allowed";
  const sizes = size === "sm" ? "h-9 px-3 text-sm" : "h-11 px-4 text-sm";
  const variants =
    variant === "primary"
      ? "bg-emerald-600 text-white hover:bg-emerald-700"
      : variant === "danger"
      ? "bg-rose-600 text-white hover:bg-rose-700"
      : variant === "ghost"
      ? "bg-transparent hover:bg-zinc-900/5 dark:hover:bg-white/5 text-zinc-800 dark:text-zinc-100"
      : "bg-zinc-900/5 hover:bg-zinc-900/10 text-zinc-900 dark:bg-white/5 dark:hover:bg-white/10 dark:text-zinc-50";

  return (
    <button className={clsx(base, sizes, variants, "min-w-[44px]", className)} {...props}>
      {iconLeft}
      <span>{children}</span>
      {iconRight}
    </button>
  );
}

function IconButton({
  label,
  onClick,
  children,
  variant = "ghost",
  active = false,
  danger = false
}: {
  label: string;
  onClick?: () => void;
  children: React.ReactNode;
  variant?: "ghost" | "default";
  active?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={clsx(
        "group relative flex h-10 w-10 items-center justify-center rounded-xl transition focus:outline-none focus:ring-2 focus:ring-emerald-500/40 no-drag",
        active 
          ? "bg-white text-zinc-900 shadow-sm dark:bg-zinc-800 dark:text-zinc-50"
          : danger 
            ? "text-red-500 hover:bg-red-50 dark:hover:bg-red-900/20"
            : "text-zinc-500 hover:bg-zinc-900/5 hover:text-zinc-900 dark:text-zinc-400 dark:hover:bg-white/5 dark:hover:text-zinc-50",
         variant === "default" && !active && !danger && "bg-zinc-900/5 dark:bg-white/5"
      )}
    >
      {children}
      
      {/* Tooltip */}
      <span className="pointer-events-none absolute top-full mt-2 hidden whitespace-nowrap rounded-lg bg-zinc-900 px-2 py-1 text-xs font-medium text-white opacity-0 shadow-lg transition group-hover:block group-hover:opacity-100 dark:bg-white dark:text-zinc-900 z-50">
        {label}
      </span>
    </button>
  );
}

function Field({
  label,
  value,
  mono = false,
  action,
}: {
  label: string;
  value: string;
  mono?: boolean;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-3 rounded-2xl border border-zinc-900/10 bg-white/60 p-4 dark:border-white/10 dark:bg-white/5">
      <div className="min-w-0">
        <div className="text-xs font-medium text-zinc-600 dark:text-zinc-400">{label}</div>
        <div className={clsx("mt-1 truncate text-sm font-semibold text-zinc-900 dark:text-zinc-50", mono && "font-mono tracking-wide")}>
          {value}
        </div>
      </div>
      {action ? <div className="shrink-0">{action}</div> : null}
    </div>
  );
}

function Modal({
  open,
  title,
  subtitle,
  children,
  footer,
  onClose,
}: {
  open: boolean;
  title: string;
  subtitle?: string;
  children: React.ReactNode;
  footer: React.ReactNode;
  onClose: () => void;
}) {
  if (!open) return null;

  return (
    <div className="no-drag fixed inset-0 z-50 flex items-end justify-center bg-black/40 p-4 backdrop-blur-sm md:items-center">
      <div className="w-full max-w-lg overflow-hidden rounded-3xl border border-white/10 bg-white shadow-2xl dark:bg-zinc-950">
        <div className="flex items-start justify-between gap-3 p-5">
          <div>
            <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-50">{title}</div>
            {subtitle ? <div className="mt-1 text-xs text-zinc-600 dark:text-zinc-400">{subtitle}</div> : null}
          </div>
          <IconButton label="Close" onClick={onClose}>
            <span className="text-xl leading-none text-zinc-500">×</span>
          </IconButton>
        </div>
        <div className="px-5 pb-5">{children}</div>
        <div className="flex flex-col gap-2 border-t border-zinc-900/10 bg-zinc-50 p-4 dark:border-white/10 dark:bg-zinc-900/30 md:flex-row md:items-center md:justify-end">
          {footer}
        </div>
      </div>
    </div>
  );
}

/* --- Main App Component --- */

export default function App() {
  /* Logic & State from Old App */
  const [peers, setPeers] = useState<Peer[]>([]);
  const peersRef = useRef<Peer[]>([]);
  
  const [clipboardHistory, setClipboardHistory] = useState<HistoryItem[]>([]);
  const [activeView, setActiveView] = useState<View>("devices");
  const [myNetworkName, setMyNetworkName] = useState("Loading...");
  const [myHostname, setMyHostname] = useState("Loading...");
  const [networkPin, setNetworkPin] = useState("...");

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
  }>({ open: false, title: "", description: "", onConfirm: () => {} });

  /* Modal State */
  const [joinOpen, setJoinOpen] = useState(false);
  const [joinTarget, setJoinTarget] = useState<string>("");
  const [joinPin, setJoinPin] = useState("");
  const [joinBusy, setJoinBusy] = useState(false);
  const [pairingPeerId, setPairingPeerId] = useState<string | null>(null);

  const [leaveOpen, setLeaveOpen] = useState(false);
  
  const [addManualOpen, setAddManualOpen] = useState(false);
  const [manualIp, setManualIp] = useState("");
  const [manualBusy, setManualBusy] = useState(false);
  
  const [joinError, setJoinError] = useState("");
  const [expandedNetworks, setExpandedNetworks] = useState<Set<string>>(new Set());
  
  // Manual Sync State
  // Manual Sync State
  const [manualSyncOpen, setManualSyncOpen] = useState(false);
  const [pendingReceive, setPendingReceive] = useState<{ text: string, sender: string, timestamp: number } | null>(null);
  const [localClipboard, setLocalClipboard] = useState(""); // Current local
  const [lastSentClipboard, setLastSentClipboard] = useState(""); // Last successfully sent
  const [lastReceivedClipboard, setLastReceivedClipboard] = useState(""); // Last received from cluster

  // We need to know if Auto-Send is ON/OFF to decide if we show "Pending Send"
  const [isAutoSend, setIsAutoSend] = useState(true); // Default assumption, updated by Settings
  
  // Rule 2: If local matches last received, not a candidate for sending.
  const hasPendingSend = !isAutoSend 
      && localClipboard !== lastSentClipboard 
      && localClipboard !== lastReceivedClipboard
      && localClipboard.length > 0;

  // Rule 3: If pending receive matches local (already have it or I sent it), not a candidate.
  const hasPendingReceive = !!pendingReceive 
      && pendingReceive.text !== localClipboard;
      
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

  /* Data Fetching */
  const fetchSettings = () => {
      invoke<AppSettings>("get_settings").then(s => {
          setIsAutoSend(s.auto_send);
      });
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
    
    // 3. Settings
    fetchSettings();
  }, []);

  // Poll/Update PIN when network name changes
  useEffect(() => {
    invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
  }, [myNetworkName]);

  // Listeners
  useEffect(() => {
    if (!myHostname) return; // Wait for identity to prevent false "remote" detection

    const unlistenPeer = listen<Peer>("peer-update", (event) => {
      // If we just paired (trusted), refresh metadata
      if (event.payload.is_trusted) {
          invoke<string>("get_network_name").then(name => setMyNetworkName(name));
          invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
          setJoinOpen(false); // Close modal on success
      }

      setPeers((prev) => {
        const exists = prev.find((p) => p.id === event.payload.id);
        if (exists) return prev.map((p) => (p.id === event.payload.id ? event.payload : p));
        return [...prev, event.payload];
      });
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
          files: p.files
      };

      // Update Local Clipboard State
      if (isLocal) {
           // If it has text, update local view
           if (newItem.text) setLocalClipboard(newItem.text);
           if (isAutoSend) {
               if (newItem.text) setLastSentClipboard(newItem.text);
           }
      } else {
           // Remote sender
           if (newItem.text) {
               setLocalClipboard(newItem.text);
               setLastReceivedClipboard(newItem.text);
           }
      }
      
      // Update History
      setClipboardHistory((prev) => {
          // Dedupe by ID
          if (prev.find(i => i.id === newItem.id)) return prev;
          return [newItem, ...prev].slice(0, 50);
      });
    });

    const unlistenPending = listen<{ id: string, text: string, timestamp: number, sender: string }>("clipboard-pending", (event) => {
        setPendingReceive(event.payload);
        // Maybe open modal automatically? Or just show FAB?
        // User requested FAB.
    });

    const unlistenDelete = listen<string>("history-delete", (event) => {
        const idToDelete = event.payload;
        setClipboardHistory((prev) => prev.filter(i => i.id !== idToDelete));
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

    return () => {
      unlistenPeer.then((f) => f());
      unlistenClipboard.then((f) => f());
      unlistenPending.then((f) => f());
      unlistenRemove.then((f) => f());
      unlistenReset.then((f) => f());
      unlistenUpdate.then((f) => f());
      unlistenDelete.then((f) => f());
      unlistenPairingFailed.then((f) => f());
    };
  }, [myHostname]); // Re-bind if hostname loads (needed for sender check)

  /* Handlers */

  const startJoinFlow = (networkName: string, targetPeerId: string) => {
      setJoinTarget(networkName);
      setPairingPeerId(targetPeerId);
      setJoinPin("");
      setJoinError("");
      setJoinBusy(false);
      setJoinOpen(true);
  };

  const submitJoin = async () => {
      if (!joinPin || !pairingPeerId) return;
      setJoinBusy(true);
      setJoinError("");
      
      try {
          await invoke("start_pairing", { peerId: pairingPeerId, pin: joinPin });
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
      setManualBusy(true);
      try {
          await invoke("add_manual_peer", { ip: manualIp });
          setAddManualOpen(false);
          setManualIp("");
      } catch (e) {
          alert("Failed: " + e);
      } finally {
          setManualBusy(false);
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
              status: "online" 
          }))
      });
  });

  /* Theme Setup */
  // TODO: System preference detection
  const rootThemeClass = "dark"; // Defaulting to dark for now, or check OS

  return (
    <div className={clsx(rootThemeClass, "min-h-screen w-full bg-[radial-gradient(1200px_circle_at_0%_0%,rgba(16,185,129,0.10),transparent_60%),radial-gradient(1000px_circle_at_100%_0%,rgba(59,130,246,0.10),transparent_55%),radial-gradient(900px_circle_at_50%_100%,rgba(99,102,241,0.10),transparent_50%)] dark:bg-[radial-gradient(1200px_circle_at_0%_0%,rgba(16,185,129,0.12),transparent_60%),radial-gradient(1000px_circle_at_100%_0%,rgba(59,130,246,0.10),transparent_55%),radial-gradient(900px_circle_at_50%_100%,rgba(244,63,94,0.10),transparent_50%)] md:h-screen md:overflow-hidden")}>
      
      <Dialog {...dialog} />

      <div className="mx-auto flex min-h-screen w-full max-w-6xl flex-col px-4 py-6 md:h-full md:min-h-0 md:px-6">
        {/* Custom titlebar drag region */}
        <div className="drag-region h-[10px] w-full rounded-t-3xl" />

        {/* Header */}
        <header className="flex items-center justify-between mb-4 shrink-0 px-2">
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-gradient-to-br from-emerald-400 to-blue-500 shadow-lg shadow-emerald-500/20">
              <Wifi className="h-5 w-5 text-white" />
            </div>
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
             
            <IconButton
                label="Leave & Reset"
                danger
                onClick={() => setLeaveOpen(true)}
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
               <HistoryView items={clipboardHistory} />
             ) : (
               <SettingsView onSettingsRefreshed={fetchSettings} />
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
        <Modal
          open={joinOpen}
          onClose={() => setJoinOpen(false)}
          title={`Join “${joinTarget}”`}
          subtitle="Enter the 6-character Network PIN shown on any device in that cluster."
          footer={
            <>
              <Button variant="ghost" onClick={() => setJoinOpen(false)}>
                Cancel
              </Button>
              <Button variant="primary" onClick={submitJoin} disabled={joinBusy || joinPin.trim().length < 6} iconLeft={<PlusCircle className="h-4 w-4" />}>
                {joinBusy ? "Joining…" : "Join network"}
              </Button>
            </>
          }
        >
          <div className="space-y-3">
            <div className="rounded-2xl border border-zinc-900/10 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/5">
              <div className="text-xs font-medium text-zinc-600 dark:text-zinc-400">Cluster PIN</div>
              <input
                className={clsx(
                    "mt-2 h-12 w-full rounded-2xl border bg-white px-4 font-mono text-lg tracking-[0.25em] text-zinc-900 outline-none focus:ring-2 dark:bg-zinc-950 dark:text-zinc-50",
                    joinError
                        ? "border-rose-500 focus:ring-rose-500/40 dark:border-rose-500/50"
                        : "border-zinc-900/10 focus:ring-emerald-500/40 dark:border-white/10"
                )}
                placeholder="••••••"
                value={joinPin}
                onChange={(e) => {
                    setJoinPin(e.target.value.trim());
                    setJoinError("");
                }}
                onKeyDown={(e) => e.key === "Enter" && submitJoin()}
                autoFocus
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
              />
              {joinError && (
                  <div className="mt-2 text-sm font-medium text-rose-600 dark:text-rose-400">
                      {joinError}
                  </div>
              )}
            </div>
          </div>
        </Modal>

        <Modal
          open={leaveOpen}
          onClose={() => setLeaveOpen(false)}
          title="Leave network?"
          subtitle="This wipes this device’s identity, keys, and trusted peers (factory reset)."
          footer={
            <>
              <Button variant="ghost" onClick={() => setLeaveOpen(false)}>
                Cancel
              </Button>
              <Button variant="danger" onClick={confirmLeaveNetwork} iconLeft={<AlertTriangle className="h-4 w-4" />}>
                Leave & reset
              </Button>
            </>
          }
        >
          <div className="space-y-3">
            <div className="rounded-2xl border border-rose-500/20 bg-rose-500/10 p-4 text-sm text-rose-800 dark:text-rose-200">
               Action is irreversible. You will need a PIN to rejoin.
            </div>
          </div>
        </Modal>

        <Modal
          open={addManualOpen}
          onClose={() => setAddManualOpen(false)}
          title="Add Remote Peer"
          subtitle="Enter an IP address or CIDR range (e.g. 192.168.1.0/24) to scan."
          footer={
            <>
              <Button variant="ghost" onClick={() => setAddManualOpen(false)}>
                Cancel
              </Button>
              <Button variant="primary" onClick={submitManualPeer} disabled={manualBusy || !manualIp} iconLeft={<PlusCircle className="h-4 w-4" />}>
                {manualBusy ? "Scanning..." : "Add"}
              </Button>
            </>
          }
        >
          <div className="space-y-3">
             <div className="rounded-2xl border border-zinc-900/10 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/5">
               <div className="text-xs font-medium text-zinc-600 dark:text-zinc-400">IP Address / CIDR</div>
               <input
                 className="mt-2 h-12 w-full rounded-2xl border border-zinc-900/10 bg-white px-4 text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-zinc-950 dark:text-zinc-50"
                 placeholder="e.g. 10.8.0.5 or 192.168.1.0/24"
                 value={manualIp}
                 onChange={(e) => setManualIp(e.target.value)}
                 autoFocus
                 onKeyDown={(e) => e.key === "Enter" && submitManualPeer()}
               />
               <div className="mt-2 text-xs text-zinc-500">
                  Target must be running ClusterCut on the default port (4654).
               </div>
             </div>
          </div>
        </Modal>
      </div>
    </div>
  );
}

/* --- Views --- */

function DevicesView({
  isConnected,
  myNetworkName,
  myHostname,
  networkPin,
  peers,
  nearby,
  expandedNetworks,
  toggleNetwork,
  onJoin,
  onDeletePeer,
  onAddManual,
}: {
  isConnected: boolean;
  myNetworkName: string;
  myHostname: string;
  networkPin: string;
  peers: Peer[];
  nearby: NearbyNetwork[];
  expandedNetworks: Set<string>;
  toggleNetwork: (name: string) => void;
  onJoin: (networkName: string) => void;
  onDeletePeer: (id: string) => void;
  onAddManual: () => void;
}) {

  return (
    <div className="flex h-full flex-col gap-3">
      {/* My device / identity - Fixed Height */}
      <Card className="shrink-0 p-4">
        <SectionHeader
          icon={<ShieldCheck className="h-5 w-5 text-emerald-600 dark:text-emerald-400" />}
          title={`My Device (${myHostname})`}
          subtitle="Share your PIN to admit a new device into your secure cluster."
          right={
            <Badge tone={isConnected ? "good" : "warn"}>
              {isConnected ? (
                <>
                  <Lock className="h-3.5 w-3.5" /> Checked
                </>
              ) : (
                <>
                  <Unlock className="h-3.5 w-3.5" /> No Peers
                </>
              )}
            </Badge>
          }
        />

        <div className="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2">
          <Field label="My Cluster" value={myNetworkName} mono action={<CopyMini text={myNetworkName} />} />
          <Field
            label="Cluster PIN"
            value={networkPin}
            mono
            action={
              <IconButton label="Copy PIN" onClick={() => navigator.clipboard.writeText(networkPin)} variant="default">
                 <Copy className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />
              </IconButton>
            }
          />
        </div>
      </Card>

      {/* Main Content Area - Scrollable columns */}
      <div className="flex min-h-0 flex-1 flex-col gap-3 md:grid md:grid-cols-2">
        
        {/* Trusted peers */}
        <Card className="flex flex-col overflow-hidden p-0">
          <div className="shrink-0 p-4 pb-2">
            <SectionHeader
              icon={<Lock className="h-5 w-5 text-emerald-600 dark:text-emerald-400" />}
              title="My Cluster"
              subtitle="Trusted devices."
              right={
                <Badge tone="good">
                  <CheckCircle2 className="h-3.5 w-3.5" /> Safe
                </Badge>
              }
            />
          </div>

          <div className="flex-1 overflow-y-auto px-4 pb-4">
            {peers.length === 0 ? (
              <div className="mt-2 rounded-2xl border border-zinc-900/10 bg-zinc-50 p-4 text-sm text-zinc-700 dark:border-white/10 dark:bg-white/5 dark:text-zinc-300">
                No other devices in this cluster.
              </div>
            ) : (
              <div className="mt-2 space-y-2">
                {peers.map((p) => (
                  <div
                    key={p.id}
                    className="relative flex items-center justify-between gap-3 rounded-2xl border border-zinc-900/10 bg-white/60 p-3 pr-4 dark:border-white/10 dark:bg-white/5"
                  >
                    {/* Online Badge - Absolute Top Right with some padding */}
                    <div className="absolute right-2 top-2">
                       <Badge tone="good">online</Badge>
                    </div>

                    <div className="flex items-center gap-3">
                      <div className={clsx("flex h-10 w-10 items-center justify-center rounded-2xl", "bg-emerald-500/15")}>
                        <Wifi className="h-5 w-5 text-emerald-600 dark:text-emerald-300" />
                      </div>
                      <div className="min-w-0">
                        <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-50">{p.hostname || p.id}</div>
                        <div className="text-xs text-zinc-600 dark:text-zinc-400">{p.ip}</div>
                      </div>
                    </div>

                    <div className="mt-4 flex items-center">
                      <IconButton label="Kick / Ban" onClick={() => onDeletePeer(p.id)}>
                        <Trash2 className="h-5 w-5 text-rose-600" />
                      </IconButton>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        </Card>

        {/* Nearby networks */}
        <Card className="flex flex-col overflow-hidden p-0">
          <div className="shrink-0 p-4 pb-2">
             <SectionHeader
                icon={<Unlock className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
                title="Nearby Clusters"
                subtitle="Discovered clusters."
                right={
                  <Button size="sm" iconLeft={<PlusCircle className="h-4 w-4" />} onClick={onAddManual}>
                    Add Remote
                  </Button>
                }
              />
          </div>

          <div className="flex-1 overflow-y-auto px-4 pb-4">
              {nearby.length === 0 ? (
                 <div className="mt-2 text-sm text-zinc-500 p-2 text-center italic">
                    Scanning for nearby devices...
                 </div>
              ) : (
                <div className="mt-2 space-y-3">
                  {nearby.map((n) => {
                    const isExpanded = expandedNetworks.has(n.networkName);
                    return (
                    <div key={n.networkName} className="rounded-2xl border border-zinc-900/10 bg-white/60 p-3 dark:border-white/10 dark:bg-white/5">
                      <div className="flex flex-col gap-3">
                        <div className="flex items-center justify-between">
                            <div className="flex items-center gap-2">
                                <button 
                                    onClick={() => toggleNetwork(n.networkName)}
                                    className="flex h-6 w-6 items-center justify-center rounded-lg text-zinc-500 hover:bg-zinc-900/5 hover:text-zinc-700 dark:text-zinc-400 dark:hover:bg-white/10 dark:hover:text-zinc-200 focus:outline-none focus:ring-2 focus:ring-emerald-500/40"
                                >
                                   {isExpanded ? <ChevronDown className="h-4 w-4"/> : <ChevronRight className="h-4 w-4"/>}
                                </button>
                                <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-50">{n.networkName}</div>
                            </div>
                            <Button
                              variant="primary"
                              size="sm"
                              iconLeft={<PlusCircle className="h-4 w-4" />}
                              onClick={() => onJoin(n.networkName)}
                              className="no-drag"
                            >
                              Join
                            </Button>
                        </div>
                        {isExpanded && (
                            <div className="flex flex-col gap-2 pl-8">
                              {n.devices.map((d) => (
                                <div key={d.id} className="flex items-center gap-2 rounded-xl bg-black/5 p-2 dark:bg-white/5">
                                    <span className="inline-flex h-2 w-2 shrink-0 rounded-full bg-emerald-500" />
                                    <span className="truncate text-xs font-medium text-zinc-700 dark:text-zinc-300">
                                       {d.hostname || d.id}
                                    </span>
                                </div>
                              ))}
                            </div>
                        )}
                      </div>
                    </div>
                  );})}
                </div>
              )}
          </div>
        </Card>
      </div>
    </div>
  );
}

function HistoryView({ items }: { items: HistoryItem[] }) {
  const [myHostname, setMyHostname] = useState<string>("");
  const [progress, setProgress] = useState<Record<string, { transferred: number, total: number }>>({});

  useEffect(() => {
      invoke<string>("get_hostname").then(setMyHostname);
      
      const unlistenPromise = listen<{id: string, fileName: string, total: number, transferred: number}>("file-progress", (e) => {
         setProgress(p => ({ 
             ...p, 
             [e.payload.id]: { transferred: e.payload.transferred, total: e.payload.total } 
         }));
      });

      return () => { unlistenPromise.then(u => u()); };
  }, []);

  const handleSend = async (text: string) => {
      try {
          await invoke("send_clipboard", { text });
          // Note: The backend will emit `clipboard-change` which updates list
      } catch (e) {
          console.error("Failed to send:", e);
          alert("Failed to send: " + e);
      }
  };

  const handleLocalCopy = async (text: string) => {
     try {
         await invoke("set_local_clipboard", { text });
     } catch (e) {
         console.error("Failed to set local clipboard:", e);
     }
  };

  const handleDelete = async (id: string) => {
      try {
           // Delete locally first for instant feedback? 
           // Or wait for event loopback? 
           // Better wait or optimistic?
           // The event `history-delete` is broadcasted. locally we might need to manual delete if backend doesn't loopback history-delete to self? 
           // Messages are usually not looped back to self unless logic handles it.
           // lib.rs `HistoryDelete` handler emits event. But `delete_history_item` ONLY sends to peers. 
           // So we must manually update local state or emit local event in backend.
           // BE logic: `delete_history_item` sends to peers. It does NOT emit local event.
           // So we should delete from UI list directly here too.
           await invoke("delete_history_item", { id });
           // Optimistic Update is fine
      } catch (e) {
          console.error("Failed to delete:", e);
      }
  };

  const handleDownloadAll = async (fileId: string, files: {name: string}[], peerId: string) => {
       try {
           for(let i=0; i<files.length; i++) {
                setProgress(p => ({ ...p, [fileId]: { transferred: 0, total: 100 } })); 
                await invoke("request_file", { fileId, fileIndex: i, peerId });
           }
       } catch (e) {
           alert("Download failed: " + e);
           setProgress(p => { const n = {...p}; delete n[fileId]; return n; });
       }
  };

  return (
    <div className="space-y-5">
      <Card className="p-5">
        <SectionHeader
          icon={<Copy className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Clipboard history"
          subtitle="Recent entries."
        />

        <div className="mt-4 space-y-2">
          {items.map((it) => {
             const isMe = it.device === myHostname || it.device === "localhost" || it.origin === "local"; 
             // Logic check: "origin" in item type is mostly placeholder now if we trust device name.
             // If device name matches myHostname, it is "Sent" (Arrow Up).
             // Else "Received" (Arrow Down).
             
             return (
            <div
              key={it.id}
              className="rounded-2xl border border-zinc-900/10 bg-white/60 p-4 dark:border-white/10 dark:bg-white/5"
            >
              <div className="flex flex-col gap-3 md:flex-row md:items-start md:justify-between">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge tone={isMe ? "neutral" : "good"}>
                      {isMe ? (
                        <>
                          <ArrowUp className="h-3.5 w-3.5" /> Sent
                        </>
                      ) : (
                        <>
                          <ArrowDown className="h-3.5 w-3.5" /> {it.device}
                        </>
                      )}
                    </Badge>
                    <span className="text-xs text-zinc-500 dark:text-zinc-400">{timeAgo(it.ts)}</span>
                  </div>
                  {it.text && <div className="mt-2 line-clamp-3 whitespace-pre-wrap text-sm text-zinc-900 dark:text-zinc-50">{it.text}</div>}
                  
                  {it.files && it.files.length > 0 && (
                      <div className="mt-2 space-y-1">
                          {it.files.map((f, idx) => (
                              <div key={idx} className="flex flex-col gap-2 rounded-lg bg-zinc-50 p-2 text-sm dark:bg-zinc-800">
                                  <div className="flex items-center justify-between">
                                      <div className="flex items-center gap-2 overflow-hidden">
                                          <span className="truncate font-medium text-zinc-700 dark:text-zinc-300">{f.name}</span>
                                          <span className="shrink-0 text-xs text-zinc-500">({formatBytes(f.size)})</span>
                                      </div>
                                  </div>
                                  {progress[it.id] && (
                                      <div className="w-full">
                                          <div className="flex justify-between text-[10px] text-zinc-500 mb-1">
                                               <span>Downloading...</span>
                                               <span>{Math.round((progress[it.id].transferred / progress[it.id].total) * 100)}%</span>
                                          </div>
                                          <div className="h-1.5 w-full overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-700">
                                               <div 
                                                  className="h-full bg-emerald-500 transition-all duration-300 ease-out"
                                                  style={{ width: `${(progress[it.id].transferred / progress[it.id].total) * 100}%` }}
                                               />
                                          </div>
                                      </div>
                                  )}
                              </div>
                          ))}
                      </div>
                  )}
                </div>

                <div className="flex items-center justify-end gap-2">
                  <IconButton label="Copy to Clipboard" onClick={() => handleLocalCopy(it.text)}>
                      <Copy className="h-4 w-4 text-zinc-600 dark:text-zinc-300" />
                  </IconButton>

                  {!isMe && it.files && it.files.length > 0 && it.sender_id && (
                      <IconButton label="Download All" onClick={() => handleDownloadAll(it.id, it.files!, it.sender_id!)}>
                          <Download className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                      </IconButton>
                  )}
                  
                  <IconButton label="Send to Cluster" onClick={() => handleSend(it.text)}>
                      <Send className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                  </IconButton>

                  <IconButton label="Delete Everywhere" onClick={() => handleDelete(it.id)}>
                      <Trash2 className="h-4 w-4 text-rose-600 dark:text-rose-400" />
                  </IconButton>
                </div>
              </div>
            </div>
          );})}
        </div>
      </Card>
    </div>
  );
}

function SettingsView({ 
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
  }, []);

  // Autosave Effect
  useEffect(() => {
    if (loading || !settings) return;

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
                        <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">Cluster PIN (Min 6 chars)</label>
                        <input
                        className="h-10 rounded-xl border border-zinc-900/10 bg-white px-3 font-mono text-sm text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-zinc-950 dark:text-zinc-50"
                        value={provPin}
                        onChange={(e) => setProvPin(e.target.value)}
                        />
                         {provPin.length > 0 && provPin.length < 6 && <span className="text-[10px] text-rose-500">PIN must be at least 6 characters.</span>}
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
                    onClick={() => setSettings({...settings, auto_send: !settings.auto_send})}
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
                    onClick={() => setSettings({...settings, auto_receive: !settings.auto_receive})}
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
          icon={<div className="h-5 w-5 flex items-center justify-center"><svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z"/><polyline points="14 2 14 8 20 8"/></svg></div>}
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
                    onClick={() => setSettings({...settings, enable_file_transfer: !settings.enable_file_transfer})}
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
      
      {/* Footer Status */}
      <div className="flex justify-center pt-2">
           <span className={clsx("text-[10px] font-medium transition-opacity", saving ? "opacity-100 text-zinc-500" : "opacity-0 duration-1000")}>
               Saving changes...
           </span>
      </div>
    </div>
  );
}



function Dialog({ 
  open, 
  title, 
  description, 
  onConfirm, 
  onCancel, 
  confirmLabel = "Confirm", 
  cancelLabel = "Cancel",
  type = "neutral" 
}: { 
  open: boolean; 
  title: string; 
  description: string; 
  onConfirm: () => void; 
  onCancel?: () => void; 
  confirmLabel?: string; 
  cancelLabel?: string;
  type?: "neutral" | "danger" | "success";
}) {
  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4 backdrop-blur-sm">
      <div className="w-full max-w-sm overflow-hidden rounded-2xl bg-white shadow-2xl ring-1 ring-zinc-900/10 dark:bg-zinc-900 dark:ring-white/10">
        <div className="p-6">
          <h3 className="text-lg font-semibold text-zinc-900 dark:text-zinc-50">{title}</h3>
          <p className="mt-2 text-sm text-zinc-500 dark:text-zinc-400">{description}</p>
        </div>
        <div className="flex justify-end gap-3 bg-zinc-50 px-6 py-4 dark:bg-zinc-800/50">
          {onCancel && (
            <Button variant="default" onClick={onCancel}>
              {cancelLabel}
            </Button>
          )}
          <Button 
            variant={type === "danger" ? "danger" : "primary"} 
            onClick={onConfirm}
          >
            {confirmLabel}
          </Button>
        </div>
      </div>
    </div>
  );
}

/* --- Tiny Icons --- */

function CopyMini({ text }: { text: string }) {
  return (
    <IconButton label="Copy" onClick={() => navigator.clipboard.writeText(text)} variant="default">
      <Copy className="h-5 w-5 text-zinc-700 dark:text-zinc-200" />
    </IconButton>
  );
}

/* --- Manual Sync Components --- */

function ManualSyncFAB({ 
    hasPendingSend, 
    hasPendingReceive, 
    onClick 
}: { 
    hasPendingSend: boolean, 
    hasPendingReceive: boolean, 
    onClick: () => void 
}) {
    if (!hasPendingSend && !hasPendingReceive) return null;

    return (
        <button
             onClick={onClick}
             className="fixed bottom-6 right-6 z-50 flex h-14 w-14 items-center justify-center rounded-full bg-emerald-600 text-white shadow-xl shadow-emerald-600/30 transition hover:scale-105 hover:bg-emerald-500 focus:outline-none focus:ring-4 focus:ring-emerald-500/30"
        >
             {hasPendingReceive ? (
                 <ArrowDown className="h-6 w-6" />
             ) : (
                 <Send className="h-6 w-6 pl-0.5" />
             )}
             <span className="absolute -top-1 -right-1 flex h-4 w-4 items-center justify-center rounded-full bg-rose-500 text-[10px] font-bold text-white shadow-sm ring-2 ring-white dark:ring-zinc-900">
                 !
             </span>
        </button>
    );
}

function ManualSyncModal({
    open,
    onClose,
    localContent,
    remoteContent, // ClipboardPayload or string
    onSend,
    onReceive
}: {
    open: boolean;
    onClose: () => void;
    localContent: string;
    remoteContent: { text: string, sender: string, timestamp: number } | null;
    onSend: () => void;
    onReceive: () => void;
}) {
    if (!open) return null;

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4 backdrop-blur-sm">
             <div className="w-full max-w-2xl overflow-hidden rounded-3xl bg-zinc-950 shadow-2xl ring-1 ring-white/10 text-zinc-50">
                 <div className="flex items-center justify-between border-b border-white/10 p-5">
                      <h3 className="text-lg font-semibold">Synchronization</h3>
                      <button onClick={onClose} className="rounded-lg p-1 hover:bg-white/10">
                          <span className="text-xl leading-none text-zinc-400">×</span>
                      </button>
                 </div>
                 
                 <div className="grid grid-cols-1 md:grid-cols-2 divide-y md:divide-y-0 md:divide-x divide-white/10">
                      {/* Send Column */}
                      <div className="flex flex-col p-6 gap-4">
                           <div className="flex items-center gap-2 text-emerald-400">
                               <ArrowUp className="h-5 w-5" />
                               <span className="font-medium">Send Local</span>
                           </div>
                           <div className="flex-1 rounded-xl bg-white/5 p-4 text-sm font-mono text-zinc-300 h-32 overflow-y-auto whitespace-pre-wrap border border-white/5">
                                {localContent || <span className="text-zinc-600 italic">Clipboard empty</span>}
                           </div>
                           <Button variant="primary" onClick={onSend} disabled={!localContent} iconLeft={<Send className="h-4 w-4" />}>
                                Broadcast to Cluster
                           </Button>
                      </div>

                      {/* Receive Column */}
                      <div className="flex flex-col p-6 gap-4">
                           <div className="flex items-center gap-2 text-blue-400">
                               <ArrowDown className="h-5 w-5" />
                               <span className="font-medium">Receive Remote</span>
                           </div>
                           <div className="flex-1 rounded-xl bg-white/5 p-4 text-sm font-mono text-zinc-300 h-32 overflow-y-auto whitespace-pre-wrap border border-white/5 relative">
                                {remoteContent ? (
                                    <>
                                        {remoteContent.text}
                                        <div className="absolute bottom-2 right-2 flex gap-2">
                                            <span className="text-[10px] bg-white/10 px-2 py-0.5 rounded text-zinc-400">
                                                From: {remoteContent.sender}
                                            </span>
                                        </div>
                                    </>
                                ) : (
                                    <span className="text-zinc-600 italic">No pending data</span>
                                )}
                           </div>
                           <Button variant="primary" onClick={onReceive} disabled={!remoteContent} iconLeft={<Copy className="h-4 w-4" />}>
                                Apply to Clipboard
                           </Button>
                      </div>
                 </div>
             </div>
        </div>
    );
}
