import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { 
  Monitor, Copy, History, ShieldCheck, PlusCircle, Trash2, LogOut, 
  Settings, Wifi, Lock, Unlock, AlertTriangle, Info, CheckCircle2 
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
  devices: { id: string; status: "online" | "offline" }[];
};

type HistoryItem = {
    id: string;
    origin: "local" | "remote";
    device: string;
    ts: string;
    text: string;
};

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
}: {
  label: string;
  onClick?: () => void;
  children: React.ReactNode;
  variant?: "ghost" | "default";
}) {
  return (
    <button
      aria-label={label}
      onClick={onClick}
      className={clsx(
        "no-drag inline-flex h-11 w-11 items-center justify-center rounded-xl transition focus:outline-none focus:ring-2 focus:ring-emerald-500/40",
        variant === "default"
          ? "bg-zinc-900/5 hover:bg-zinc-900/10 dark:bg-white/5 dark:hover:bg-white/10"
          : "hover:bg-zinc-900/5 dark:hover:bg-white/5"
      )}
    >
      {children}
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
  const [myDeviceId, setMyDeviceId] = useState("Loading..."); // TODO: Fetch in backend
  const [networkPin, setNetworkPin] = useState("...");

  /* Modal State */
  const [joinOpen, setJoinOpen] = useState(false);
  const [joinTarget, setJoinTarget] = useState<string>("");
  const [joinPin, setJoinPin] = useState("");
  const [joinBusy, setJoinBusy] = useState(false);
  const [pairingPeerId, setPairingPeerId] = useState<string | null>(null);

  const [leaveOpen, setLeaveOpen] = useState(false);

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
    invoke<string>("get_device_id").then(id => setMyDeviceId(id));
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

    const unlistenClipboard = listen<string>("clipboard-change", (event) => {
      const newItem: HistoryItem = {
          id: Math.random().toString(36).substring(7),
          origin: "remote", // TODO: Distinguish local/remote from event? local changes handled by watcher?
          device: "Remote Peer", // TODO: Event should include sender ID
          ts: "Just now",
          text: event.payload
      };
      setClipboardHistory((prev) => [newItem, ...prev].slice(0, 50));
    });
    
    const unlistenRemove = listen<string>("peer-remove", (event) => {
        setPeers((prev) => prev.filter(p => p.id !== event.payload));
    });

    const unlistenReset = listen("network-reset", () => {
        // Reload app to reset state
        window.location.reload();
    });

    return () => {
      unlistenPeer.then((f) => f());
      unlistenClipboard.then((f) => f());
      unlistenRemove.then((f) => f());
      unlistenReset.then((f) => f());
    };
  }, []);

  /* Handlers */

  const startJoinFlow = (networkName: string, targetPeerId: string) => {
      setJoinTarget(networkName);
      setPairingPeerId(targetPeerId);
      setJoinPin("");
      setJoinBusy(false);
      setJoinOpen(true);
  };

  const submitJoin = async () => {
      if (!joinPin || !pairingPeerId) return;
      setJoinBusy(true);
      
      try {
          await invoke("start_pairing", { peerId: pairingPeerId, pin: joinPin });
          // Note: Backend handles the rest. We wait for peer-update event to close modal.
          // Timeout safety
          setTimeout(() => {
              setJoinBusy(false);
          }, 5000);
      } catch (e) {
          alert("Pairing request failed: " + String(e));
          setJoinBusy(false);
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

  const copyToClipboard = (text: string) => {
      // In Tauri we can write to clipboard via backend or frontend API
      // Using generic navigator.clipboard for simplicity if allowed context
      navigator.clipboard.writeText(text);
  };

  /* Derived State */
  const myPeers = peers.filter(p => p.is_trusted);
  const untrustedPeers = peers.filter(p => !p.is_trusted);
  const isConnected = myPeers.length > 0; // Heuristic: If we have trusted peers, we are in a 'real' cluster

  // Group untrusted by network name
  const nearbyNetworks: NearbyNetwork[] = [];
  const grouped: Record<string, Peer[]> = {};
  
  untrustedPeers.forEach(p => {
      // Skip own network
      if (p.network_name === myNetworkName) return;
      
      const name = p.network_name || "Unidentified";
      if (!grouped[name]) grouped[name] = [];
      grouped[name].push(p);
  });

  Object.entries(grouped).forEach(([name, devices]) => {
      nearbyNetworks.push({
          networkName: name,
          devices: devices.map(d => ({ 
              id: d.id, 
              // Map backend 'last_seen' to status? current backend removes ancient peers so assume online if present
              status: "online" 
          }))
      });
  });

  /* Theme Setup */
  // TODO: System preference detection
  const rootThemeClass = "dark"; // Defaulting to dark for now, or check OS

  return (
    <div className={clsx(rootThemeClass, "min-h-screen w-full bg-[radial-gradient(1200px_circle_at_0%_0%,rgba(16,185,129,0.10),transparent_60%),radial-gradient(1000px_circle_at_100%_0%,rgba(59,130,246,0.10),transparent_55%),radial-gradient(900px_circle_at_50%_100%,rgba(99,102,241,0.10),transparent_50%)] dark:bg-[radial-gradient(1200px_circle_at_0%_0%,rgba(16,185,129,0.12),transparent_60%),radial-gradient(1000px_circle_at_100%_0%,rgba(59,130,246,0.10),transparent_55%),radial-gradient(900px_circle_at_50%_100%,rgba(244,63,94,0.10),transparent_50%)]")}>
      <div className="mx-auto flex min-h-screen w-full max-w-6xl flex-col px-4 py-6 md:px-6">
        {/* Custom titlebar drag region */}
        <div className="drag-region h-[30px] w-full rounded-t-3xl" />

        {/* Header */}
        <Card className="no-drag overflow-hidden">
          <div className="flex flex-col gap-3 p-4 md:flex-row md:items-center md:justify-between">
            <div className="flex items-center gap-3">
              <div className="flex h-11 w-11 items-center justify-center rounded-2xl bg-emerald-600 text-white shadow-sm">
                <ShieldCheck className="h-5 w-5" />
              </div>
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2">
                  <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-50">UCP</div>
                  {isConnected ? (
                    <Badge tone="good">
                      <span className="inline-flex h-2 w-2 rounded-full bg-emerald-500" />
                      Connected
                    </Badge>
                  ) : (
                    <Badge tone="warn">
                      <span className="inline-flex h-2 w-2 rounded-full bg-amber-500" />
                      Singleton / Ready
                    </Badge>
                  )}
                </div>
                <div className="mt-0.5 text-xs text-zinc-600 dark:text-zinc-400">
                  {isConnected 
                      ? `Secure Cluster: ${myNetworkName}` 
                      : "Share your PIN to link devices."}
                </div>
              </div>
            </div>

            <div className="flex flex-wrap items-center gap-2 md:justify-end">
              <div className="inline-flex rounded-2xl border border-zinc-900/10 bg-white/60 p-1 dark:border-white/10 dark:bg-white/5">
                <button
                  className={clsx(
                    "no-drag inline-flex h-10 items-center gap-2 rounded-xl px-3 text-sm font-medium transition",
                    activeView === "devices"
                      ? "bg-white text-zinc-900 shadow-sm dark:bg-zinc-950 dark:text-zinc-50"
                      : "text-zinc-600 hover:bg-zinc-900/5 dark:text-zinc-300 dark:hover:bg-white/5"
                  )}
                  onClick={() => setActiveView("devices")}
                >
                  <MonitorIcon />
                  Devices
                </button>
                <button
                  className={clsx(
                    "no-drag inline-flex h-10 items-center gap-2 rounded-xl px-3 text-sm font-medium transition",
                    activeView === "history"
                      ? "bg-white text-zinc-900 shadow-sm dark:bg-zinc-950 dark:text-zinc-50"
                      : "text-zinc-600 hover:bg-zinc-900/5 dark:text-zinc-300 dark:hover:bg-white/5"
                  )}
                  onClick={() => setActiveView("history")}
                >
                  <History className="h-4 w-4" />
                  History
                </button>
              </div>

              <IconButton label="Settings" onClick={() => setActiveView("settings")} variant="default">
                <Settings className="h-5 w-5 text-zinc-700 dark:text-zinc-200" />
              </IconButton>

              <Button
                variant="danger"
                iconLeft={<LogOut className="h-4 w-4" />}
                onClick={() => setLeaveOpen(true)}
                className="no-drag"
              >
                Leave
              </Button>
            </div>
          </div>
        </Card>

        {/* Content */}
        <div className="mt-5">
           {/* In the design, this was a grid with sidebar. For simplicity we can just render the Main Panel full width for now, or match the sidebar structure if desired. Let's keep it simple full width since the sidebar was mostly Demo Controls */}
           <div className="no-drag">
             {activeView === "devices" ? (
               <DevicesView
                 isConnected={isConnected}
                 myNetworkName={myNetworkName}
                 myDeviceId={myDeviceId}
                 networkPin={networkPin}
                 peers={myPeers}
                 nearby={nearbyNetworks}
                 onJoin={(netName) => {
                     // Find a peer in that network to join
                     const group = grouped[netName];
                     if (group && group.length > 0) {
                         startJoinFlow(netName, group[0].id);
                     }
                 }}
                 onDeletePeer={deletePeer}
               />
             ) : activeView === "history" ? (
               <HistoryView items={clipboardHistory} onCopy={copyToClipboard} />
             ) : (
               <SettingsView />
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
              <div className="text-xs font-medium text-zinc-600 dark:text-zinc-400">Network PIN</div>
              <input
                className="mt-2 h-12 w-full rounded-2xl border border-zinc-900/10 bg-white px-4 font-mono text-lg tracking-[0.25em] text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-zinc-950 dark:text-zinc-50"
                placeholder="••••••"
                value={joinPin}
                onChange={(e) => setJoinPin(e.target.value.toUpperCase().replace(/[^A-Z0-9]/g, "").slice(0, 6))}
                autoFocus
              />
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
      </div>
    </div>
  );
}

/* --- Views --- */

function DevicesView({
  isConnected,
  myNetworkName,
  myDeviceId,
  networkPin,
  peers,
  nearby,
  onJoin,
  onDeletePeer,
}: {
  isConnected: boolean;
  myNetworkName: string;
  myDeviceId: string;
  networkPin: string;
  peers: Peer[];
  nearby: NearbyNetwork[];
  onJoin: (networkName: string) => void;
  onDeletePeer: (id: string) => void;
}) {

  return (
    <div className="space-y-5">
      {/* My device / identity */}
      <Card className="p-5">
        <SectionHeader
          icon={<ShieldCheck className="h-5 w-5 text-emerald-600 dark:text-emerald-400" />}
          title="My device"
          subtitle="Share your PIN to admit a new device into your secure cluster."
          right={
            <Badge tone={isConnected ? "good" : "warn"}>
              {isConnected ? (
                <>
                  <Lock className="h-3.5 w-3.5" /> Trusted cluster
                </>
              ) : (
                <>
                  <Unlock className="h-3.5 w-3.5" /> Not paired
                </>
              )}
            </Badge>
          }
        />

        <div className="mt-4 grid grid-cols-1 gap-3 md:grid-cols-3">
          <Field label="My Network Name" value={myNetworkName} mono action={<CopyMini text={myNetworkName} />} />
          <Field label="My Device ID" value={myDeviceId} mono action={<CopyMini text={myDeviceId} />} />
          <Field
            label="Network PIN"
            value={networkPin}
            mono
            action={
              <Button variant="primary" size="sm" iconLeft={<Copy className="h-4 w-4" />} onClick={() => navigator.clipboard.writeText(networkPin)}>
                Copy
              </Button>
            }
          />
        </div>
      </Card>

      {/* Trusted peers */}
      <Card className="p-5">
        <SectionHeader
          icon={<Lock className="h-5 w-5 text-emerald-600 dark:text-emerald-400" />}
          title="Trusted peers"
          subtitle="Devices in your secure cluster."
          right={
            <Badge tone="good">
              <CheckCircle2 className="h-3.5 w-3.5" /> Safe zone
            </Badge>
          }
        />

        {peers.length === 0 ? (
          <div className="mt-4 rounded-2xl border border-zinc-900/10 bg-zinc-50 p-4 text-sm text-zinc-700 dark:border-white/10 dark:bg-white/5 dark:text-zinc-300">
            No other devices in this cluster.
          </div>
        ) : (
          <div className="mt-4 space-y-2">
            {peers.map((p) => (
              <div
                key={p.id}
                className="flex flex-col gap-3 rounded-2xl border border-zinc-900/10 bg-white/60 p-4 dark:border-white/10 dark:bg-white/5 md:flex-row md:items-center md:justify-between"
              >
                <div className="flex items-center gap-3">
                  <div className={clsx("flex h-10 w-10 items-center justify-center rounded-2xl", "bg-emerald-500/15")}>
                    <Wifi className="h-5 w-5 text-emerald-600 dark:text-emerald-300" />
                  </div>
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-50">{p.hostname || p.id}</div>
                      <Badge tone="good">online</Badge>
                    </div>
                    <div className="mt-1 text-xs text-zinc-600 dark:text-zinc-400">{p.ip}</div>
                  </div>
                </div>

                <div className="flex items-center justify-end gap-2">
                  <Button size="sm" variant="ghost" iconLeft={<Copy className="h-4 w-4" />} onClick={() => navigator.clipboard.writeText(p.id)}>
                    Copy ID
                  </Button>
                  <IconButton label="Kick / Ban" onClick={() => onDeletePeer(p.id)}>
                    <Trash2 className="h-5 w-5 text-rose-600" />
                  </IconButton>
                </div>
              </div>
            ))}
          </div>
        )}
      </Card>

      {/* Nearby networks */}
      {nearby.length > 0 && (
          <Card className="p-5">
            <SectionHeader
              icon={<Unlock className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
              title="Nearby networks"
              subtitle="Other UCP clusters on your LAN."
            />

            <div className="mt-4 grid grid-cols-1 gap-3 lg:grid-cols-2">
              {nearby.map((n) => (
                <div key={n.networkName} className="rounded-2xl border border-zinc-900/10 bg-white/60 p-4 dark:border-white/10 dark:bg-white/5">
                  <div className="flex items-start justify-between gap-3">
                    <div>
                      <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-50">{n.networkName}</div>
                      <div className="mt-1 flex flex-wrap gap-2 text-xs text-zinc-600 dark:text-zinc-400">
                        {n.devices.map((d) => (
                          <span key={d.id} className="inline-flex items-center gap-1">
                            <span className="inline-flex h-2 w-2 rounded-full bg-emerald-500" />
                            {d.id.substring(0,8)}...
                          </span>
                        ))}
                      </div>
                    </div>

                    <Button
                      variant="primary"
                      iconLeft={<PlusCircle className="h-4 w-4" />}
                      onClick={() => onJoin(n.networkName)}
                      className="no-drag"
                    >
                      Join
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          </Card>
      )}
    </div>
  );
}

function HistoryView({ items, onCopy }: { items: HistoryItem[]; onCopy: (txt: string) => void }) {
  return (
    <div className="space-y-5">
      <Card className="p-5">
        <SectionHeader
          icon={<Copy className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Clipboard history"
          subtitle="Recent entries."
        //   right={<Button variant="danger" size="sm" iconLeft={<Trash2 className="h-4 w-4" />}>Clear</Button>}
        />

        <div className="mt-4 space-y-2">
          {items.map((it) => (
            <div
              key={it.id}
              className="rounded-2xl border border-zinc-900/10 bg-white/60 p-4 dark:border-white/10 dark:bg-white/5"
            >
              <div className="flex flex-col gap-3 md:flex-row md:items-start md:justify-between">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge tone={it.origin === "remote" ? "good" : "neutral"}>
                      {it.origin === "remote" ? (
                        <>
                          <Lock className="h-3.5 w-3.5" /> {it.device}
                        </>
                      ) : (
                        <>
                          <Info className="h-3.5 w-3.5" /> Local copy
                        </>
                      )}
                    </Badge>
                    <span className="text-xs text-zinc-500 dark:text-zinc-400">{it.ts}</span>
                  </div>
                  <div className="mt-2 line-clamp-3 whitespace-pre-wrap text-sm text-zinc-900 dark:text-zinc-50">{it.text}</div>
                </div>

                <div className="flex items-center justify-end gap-2">
                  <Button size="sm" variant="primary" iconLeft={<Copy className="h-4 w-4" />} onClick={() => onCopy(it.text)}>
                    Copy
                  </Button>
                </div>
              </div>
            </div>
          ))}
        </div>
      </Card>
    </div>
  );
}

function SettingsView() {
  return (
    <div className="p-4 text-center text-zinc-500">
        Settings are coming soon.
    </div>
  );
}


/* --- Tiny Icons --- */

function MonitorIcon() {
  return <Monitor className="h-4 w-4 text-zinc-600 dark:text-zinc-300" />;
}

function CopyMini({ text }: { text: string }) {
  return (
    <IconButton label="Copy" onClick={() => navigator.clipboard.writeText(text)} variant="default">
      <Copy className="h-5 w-5 text-zinc-700 dark:text-zinc-200" />
    </IconButton>
  );
}
