import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { Monitor, Copy, History, ShieldCheck } from "lucide-react";
import clsx from "clsx";

interface Peer {
  id: string;
  ip: string;
  hostname: string;
  port: number;
  last_seen: number;
}

function App() {
  const [peers, setPeers] = useState<Peer[]>([]);
  const [clipboardHistory, setClipboardHistory] = useState<string[]>([]);
  const [activeTab, setActiveTab] = useState<"devices" | "history">("devices");

  useEffect(() => {
    // Initial fetch
    invoke<Record<string, Peer>>("get_peers").then((peerMap) => {
        setPeers(Object.values(peerMap));
    });

    const unlistenPeer = listen<Peer>("peer-update", (event) => {
      setPeers((prev) => {
        const exists = prev.find((p) => p.id === event.payload.id);
        if (exists) return prev.map((p) => (p.id === event.payload.id ? event.payload : p));
        return [...prev, event.payload];
      });
    });

    const unlistenClipboard = listen<string>("clipboard-change", (event) => {
      setClipboardHistory((prev) => [event.payload, ...prev].slice(0, 10)); // Keep last 10
    });

    return () => {
      unlistenPeer.then((f) => f());
      unlistenClipboard.then((f) => f());
    };
  }, []);

  return (
    <div className="flex flex-col h-screen w-screen bg-neutral-900 text-white overflow-hidden font-sans">
      {/* Header */}
      <header className="flex items-center justify-between px-4 py-3 bg-neutral-800 border-b border-neutral-700 select-none drag-region">
        <div className="flex items-center gap-2">
            <div className="w-3 h-3 rounded-full bg-emerald-500 shadow-[0_0_8px_rgba(16,185,129,0.5)]"></div>
            <h1 className="font-bold text-lg tracking-tight">UCP</h1>
        </div>
        <div className="flex gap-1 bg-neutral-700/50 p-1 rounded-lg">
            <button 
                onClick={() => setActiveTab("devices")}
                className={clsx("p-1.5 rounded-md transition-all", activeTab === "devices" ? "bg-neutral-600 shadow-sm" : "hover:bg-neutral-700/50")}
            >
                <Monitor size={18} className={activeTab === "devices" ? "text-white" : "text-neutral-400"} />
            </button>
            <button 
                onClick={() => setActiveTab("history")}
                className={clsx("p-1.5 rounded-md transition-all", activeTab === "history" ? "bg-neutral-600 shadow-sm" : "hover:bg-neutral-700/50")}
            >
                <History size={18} className={activeTab === "history" ? "text-white" : "text-neutral-400"} />
            </button>
        </div>
      </header>

      {/* Content */}
      <main className="flex-1 overflow-y-auto p-4">
        {activeTab === "devices" && (
            <div className="space-y-3">
                <h2 className="text-xs font-semibold text-neutral-500 uppercase tracking-wider mb-2">Nearby Devices</h2>
                
                {peers.length === 0 ? (
                    <div className="flex flex-col items-center justify-center h-48 text-neutral-500 border border-dashed border-neutral-700 rounded-xl">
                        <Monitor size={32} className="mb-2 opacity-50" />
                        <p className="text-sm">Scanning for peers...</p>
                    </div>
                ) : (
                    peers.map((peer) => (
                        <div key={peer.id} className="group flex items-center justify-between p-3 bg-neutral-800/50 hover:bg-neutral-800 border border-neutral-700/50 hover:border-neutral-600 rounded-xl transition-all cursor-default">
                            <div className="flex items-center gap-3">
                                <div className="w-10 h-10 flex items-center justify-center bg-blue-500/10 text-blue-400 rounded-lg group-hover:bg-blue-500 group-hover:text-white transition-colors">
                                    <Monitor size={20} />
                                </div>
                                <div>
                                    <div className="font-medium text-sm text-neutral-200">{peer.hostname}</div>
                                    <div className="text-xs text-neutral-500 font-mono">{peer.ip}</div>
                                </div>
                            </div>
                            <div className="flex items-center gap-2">
                                {/* Status Indicator */}
                                <div className="flex items-center gap-1.5 px-2 py-1 bg-emerald-500/10 text-emerald-400 text-xs rounded-md border border-emerald-500/20">
                                    <ShieldCheck size={12} />
                                    <span>Trusted</span>
                                </div>
                            </div>
                        </div>
                    ))
                )}
            </div>
        )}

        {activeTab === "history" && (
             <div className="space-y-3">
                <h2 className="text-xs font-semibold text-neutral-500 uppercase tracking-wider mb-2">Clipboard History</h2>
                {clipboardHistory.length === 0 ? (
                    <div className="text-center py-10 text-neutral-500 text-sm">History is empty</div>
                ) : (
                    clipboardHistory.map((item, i) => (
                        <div key={i} className="flex gap-3 p-3 bg-neutral-800/50 border border-neutral-700/50 rounded-xl">
                            <Copy size={16} className="text-neutral-500 mt-1 shrink-0" />
                            <p className="text-sm text-neutral-300 font-mono break-all line-clamp-3">{item}</p>
                        </div>
                    ))
                )}
             </div>
        )}
      </main>

      {/* Footer Status */}
      <footer className="px-4 py-2 bg-neutral-900 border-t border-neutral-800 text-xs text-neutral-500 flex justify-between items-center">
        <div className="flex items-center gap-2">
            <div className="w-2 h-2 rounded-full bg-emerald-500 animate-pulse"></div>
            <span>Discovery Active</span>
        </div>
        <span>v0.1.0</span>
      </footer>
    </div>
  );
}

export default App;
