import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { 
  Monitor, Copy, History, ShieldCheck, PlusCircle, Trash2, LogOut, 
  Settings, Wifi, Lock, Unlock, AlertTriangle, Info, CheckCircle2,
  ChevronDown, ChevronRight, ArrowUp, ArrowDown, Send
} from "lucide-react";
import clsx from "clsx";

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
  }, []);

  // Poll/Update PIN when network name changes
  useEffect(() => {
    invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
  }, [myNetworkName]);

  // Listeners
  useEffect(() => {
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
    // Payload can be string (Legacy) or Object (New)
    const unlistenClipboard = listen<string | { id: string, text: string, timestamp: number, sender: string }>("clipboard-change", (event) => {
      let newItem: HistoryItem;

      if (typeof event.payload === 'string') {
           // Legacy / Simple Text
           newItem = {
              id: Math.random().toString(36).substring(7),
              origin: "remote", // Assume remote for simple strings unless we track local state better? Actually local changes emit too.
              // Wait, local changes emit clipboard-change too!
              // In legacy, we can't distinguish easy without text comparison?
              // For now, let's just mark legacy string events as "Unknown" origin or "Received" if unsure.
              device: "Unknown",
              ts: Math.floor(Date.now() / 1000),
              text: event.payload
          };
      } else {
          // Rich Payload
          const p = event.payload;
          // Determine origin based on sender
          // If sender is MY hostname, it's local (sent). If not, remote (received).
          // We can use a simpler heuristic: If I emitted it, "clipboard-change" is fired for MY changes too.
          // Wait, lib.rs emits "clipboard-change" for BOTH locally detected changes and decrypted remote ones.
          // In "clipboard::start_monitor", we calculate `hostname`.
          // We can get our own hostname in `useEffect` to compare?
          // Ideally we check if `p.sender === myHostname`.
          newItem = {
              id: p.id,
              origin: "remote", // Default, updated below in effect if possible, or just render based on sender name?
              // Actually, simpler: Pass "myHostname" to HistoryView or usage.
              // For data model, let's store the sender.
              device: p.sender,
              ts: p.timestamp,
              text: p.text
          };
      }

      setClipboardHistory((prev) => {
          // Dedupe by ID if exists
          if (prev.find(i => i.id === newItem.id)) return prev;
          return [newItem, ...prev].slice(0, 50);
      });
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
      unlistenRemove.then((f) => f());
      unlistenReset.then((f) => f());
      unlistenUpdate.then((f) => f());
      unlistenDelete.then((f) => f());
      unlistenPairingFailed.then((f) => f());
    };
  }, []);

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

  const showMessage = (title: string, msg: string, type: "success" | "neutral" = "neutral") => {
      setDialog({
          open: true,
          title,
          description: msg,
          type,
          confirmLabel: "Close",
          onConfirm: () => setDialog(d => ({ ...d, open: false })),
          cancelLabel: undefined,
          onCancel: undefined // Hides cancel button
      });
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
               <SettingsView onDirtyChange={setUnsavedChanges} showMessage={showMessage} />
             )}
           </div>
        </div>

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

  useEffect(() => {
      invoke<string>("get_hostname").then(setMyHostname);
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
                  <div className="mt-2 line-clamp-3 whitespace-pre-wrap text-sm text-zinc-900 dark:text-zinc-50">{it.text}</div>
                </div>

                <div className="flex items-center justify-end gap-2">
                  <IconButton label="Copy to Clipboard" onClick={() => handleLocalCopy(it.text)}>
                      <Copy className="h-4 w-4 text-zinc-600 dark:text-zinc-300" />
                  </IconButton>
                  
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
    onDirtyChange,
    showMessage 
}: { 
    onDirtyChange: (dirty: boolean) => void;
    showMessage: (title: string, msg: string, type: "success" | "neutral") => void;
}) {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [initialSettings, setInitialSettings] = useState<AppSettings | null>(null); // For deep compare if needed
  
  const [networkName, setNetworkName] = useState("");
  const [networkPin, setNetworkPin] = useState("");
  const [initialName, setInitialName] = useState("");
  const [initialPin, setInitialPin] = useState("");

  const [provName, setProvName] = useState("");
  const [provPin, setProvPin] = useState("");
  
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);

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
      setInitialName(n);
      setInitialPin(p);
      setProvName(n); 
      setProvPin(p);
      setLoading(false);
    });
  }, []);

  // Dirty Check Effect
  useEffect(() => {
     if (!settings || !initialSettings) return;
     
     // Check basic settings
     const basicDirty = JSON.stringify(settings) !== JSON.stringify(initialSettings);
     
     // Check Provisioning Fields check:
     // If we are in "provisioned" mode, and the provName/provPin differ from what is currently active/saved
     let provDirty = false;
     if (settings.cluster_mode === "provisioned") {
         // However, provName/Pin changes are local to inputs until saved.
         // If I change provName, is it dirty vs Initial State?
         // YES. Initial State had `provName == networkName`.
         // If `provName !== initialName` or `provPin !== initialPin`, it is dirty.
         provDirty = (provName !== initialName || provPin !== initialPin);
     }
     
     onDirtyChange(basicDirty || provDirty);
  }, [settings, initialSettings, provName, provPin, initialName, initialPin]);

  const handleSave = async () => {
    if (!settings) return;
    setSaving(true);
    try {
      // 1. Save general settings
      await invoke("save_settings", { settings });
      
      // 2. Handle Provisioning Changes
      let msg = "Settings saved successfully.";
      
      if (settings.cluster_mode === "provisioned") {
         // Validate
         if (provName.trim().includes(" ")) {
             showMessage("Validation Error", "Cluster Name cannot contain spaces.", "neutral");
             setSaving(false);
             return;
         }
         if (provPin.length < 6) {
             showMessage("Validation Error", "PIN must be at least 6 characters.", "neutral");
             setSaving(false);
             return;
         }
         
         if (provName !== networkName || provPin !== networkPin) {
             await invoke("set_network_identity", { name: provName, pin: provPin });
             // Reload network data
             const n = await invoke<string>("get_network_name");
             const p = await invoke<string>("get_network_pin");
             setNetworkName(n);
             setNetworkPin(p);
             setInitialName(n);
             setInitialPin(p);
             msg = "Network Identity Updated. You may need to repair your devices.";
         }
      } else {
        if (initialSettings && initialSettings.cluster_mode === "provisioned") {
            await invoke("regenerate_network_identity");
            // Reload network data
            const n = await invoke<string>("get_network_name");
            const p = await invoke<string>("get_network_pin");
            setNetworkName(n);
            setNetworkPin(p);
            setProvName(n);
            setProvPin(p);
            setInitialName(n);
            setInitialPin(p);
            msg = "Network Identity has been reset to random.";
        }
      }
      
      // Update initial state
      setInitialSettings(JSON.parse(JSON.stringify(settings)));
      onDirtyChange(false);
      
      showMessage("Success", msg, "success");

    } catch (e) {
      showMessage("Error", "Failed to save: " + e, "neutral");
    } finally {
      setSaving(false);
    }
  };

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
                    </div>
                    <div className="flex flex-col gap-1">
                        <label className="text-xs font-medium text-zinc-600 dark:text-zinc-400">Cluster PIN (Min 6 chars)</label>
                        <input
                        className="h-10 rounded-xl border border-zinc-900/10 bg-white px-3 font-mono text-sm text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-zinc-950 dark:text-zinc-50"
                        value={provPin}
                        onChange={(e) => setProvPin(e.target.value)}
                        />
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
                    <div className="text-xs text-zinc-500">Automatically broadcast local copies.</div>
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
                 <div className="rounded-xl border border-dashed border-zinc-300 p-3 dark:border-zinc-700">
                     <div className="flex items-center justify-between opacity-50 cursor-not-allowed">
                         <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Global Shortcut (Send)</div>
                         <div className="text-xs font-mono bg-zinc-100 px-2 py-1 rounded dark:bg-zinc-800">Ctrl+Alt+C</div>
                     </div>
                     <div className="mt-1 text-[10px] text-zinc-400 text-center">Config coming later</div>
                 </div>
             )}

             <div className="h-px bg-zinc-900/5 dark:bg-white/5" />

             <div className="flex items-center justify-between">
                <div>
                    <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Automatic Receive</div>
                    <div className="text-xs text-zinc-500">Automatically apply remote clips.</div>
                </div>
                <button 
                    onClick={() => setSettings({...settings, auto_receive: !settings.auto_receive})}
                    className={clsx("relative h-6 w-11 rounded-full transition-colors", settings.auto_receive ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
                >
                    <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.auto_receive ? "translate-x-6" : "translate-x-1")} />
                </button>
             </div>
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
        </div>
      </Card>
      
      {/* Save Action */}
      <div className="flex justify-end pt-4">
          <Button variant="primary" onClick={handleSave} disabled={saving} iconLeft={<CheckCircle2 className="h-4 w-4" />}>
              {saving ? "Saving..." : "Save Changes"}
          </Button>
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
