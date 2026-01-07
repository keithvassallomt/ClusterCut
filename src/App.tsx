import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { Monitor, Copy, History, ShieldCheck, PlusCircle, Trash2, LogOut } from "lucide-react";
import clsx from "clsx";

interface Peer {
  id: string;
  ip: string;
  hostname: string;
  port: number;
  last_seen: number;
  is_trusted: boolean;
  is_manual?: boolean;
  network_name?: string;
}

function App() {
  const [peers, setPeers] = useState<Peer[]>([]);
  const peersRef = useRef<Peer[]>([]); // Ref to access peers inside stable listeners
  
  const [clipboardHistory, setClipboardHistory] = useState<string[]>([]);
  const [activeTab, setActiveTab] = useState<"devices" | "history">("devices");
  const [myNetworkName, setMyNetworkName] = useState("Loading...");

  const [networkPin, setNetworkPin] = useState("...");

  /* Pairing State */
  const [pairingPeer, setPairingPeer] = useState<Peer | null>(null);
  const [pin, setPin] = useState("");
  const [showPairingModal, setShowPairingModal] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);

  // Keep ref in sync
  useEffect(() => {
      peersRef.current = peers;
  }, [peers]);

  useEffect(() => {
    // Initial fetch
    invoke<Record<string, Peer>>("get_peers").then((peerMap) => {
        setPeers(Object.values(peerMap));
    });
    
    // Fetch Network Name & PIN
    invoke<string>("get_network_name").then(name => setMyNetworkName(name));
    invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
  }, []);

  // Ensure PIN matches the displayed Network Name
  useEffect(() => {
    invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
  }, [myNetworkName]);

  useEffect(() => {
    const unlistenPeer = listen<Peer>("peer-update", (event) => {
      console.log("Peer Update Received:", event.payload);
      // If we just paired, re-fetch network name/pin as it might have changed!
      if (event.payload.is_trusted) {
          invoke<string>("get_network_name").then(name => setMyNetworkName(name));
          invoke<string>("get_network_pin").then(pin => setNetworkPin(pin));
      }

      setPeers((prev) => {
        const exists = prev.find((p) => p.id === event.payload.id);
        if (exists) return prev.map((p) => (p.id === event.payload.id ? event.payload : p));
        return [...prev, event.payload];
      });
    });

    const unlistenClipboard = listen<string>("clipboard-change", (event) => {
      setClipboardHistory((prev) => [event.payload, ...prev].slice(0, 10)); // Keep last 10
    });
    
    const unlistenRemove = listen<string>("peer-remove", (event) => {
        setPeers((prev) => prev.filter(p => p.id !== event.payload));
    });

    const unlistenReset = listen("network-reset", () => {
        alert("You have been removed from the network.\nThe application will now reset.");
        window.location.reload();
    });

    return () => {
      unlistenPeer.then((f) => f());
      unlistenClipboard.then((f) => f());
      unlistenRemove.then((f) => f());
      unlistenReset.then((f) => f());
    };
  }, []); // Stable listener!

  const startPairing = (peer: Peer) => {
      setPairingPeer(peer);
      setPin("");
      setIsConnecting(false);
      setShowPairingModal(true);
  };

  const addManualPeer = async () => {
      const input = prompt("Enter Peer Address (IP:PORT)", "");
      if (!input) return;
      
      try {
          await invoke("add_manual_peer", { ip: input });
      } catch (e) {
          alert("Failed to add peer: " + String(e));
      }
  };

  const deletePeer = async (id: string) => {
      if (!confirm("Are you sure you want to forget this device?")) return;
      try {
          await invoke("delete_peer", { peerId: id });
          // Optimistic update
          setPeers((prev) => prev.filter(p => p.id !== id));
      } catch (e) {
          alert("Failed to delete peer: " + String(e));
      }
  };

  const submitPairing = async () => {
      if (!pin || !pairingPeer) return;
      setIsConnecting(true);
      
      try {
          await invoke("start_pairing", { peerId: pairingPeer.id, pin });
          // Note: Backend processes response automatically. 
          // We can assume if no error thrown, request sent.
          // Wait for peer update?
          setTimeout(() => {
               setShowPairingModal(false);
               setIsConnecting(false);
          }, 2000);
      } catch (e) {
          alert("Pairing Failed: " + String(e));
          setIsConnecting(false);
      }
  };



  const handleLeaveNetwork = async () => {
    console.log("Leave Network Button Clicked");
    if (!confirm("Are you sure you want to leave this network? This will reset your device identity.")) {
        console.log("User cancelled leave network");
        return;
    }
    
    console.log("User confirmed leave network. Invoking command...");
    try {
        await invoke("leave_network");
        console.log("Command invoked successfully.");
        // network-reset event will reload the page
    } catch (e) {
        console.error("Failed to leave network:", e);
        alert("Failed to leave network: " + e);
    }
  };

  // derived state for UI
  const myPeers = peers.filter(p => p.is_trusted);
  const untrustedPeers = peers.filter(p => !p.is_trusted);
  
  // Group untrusted peers by network name
  const otherNetworks: Record<string, Peer[]> = {};
  const unknownPeers: Peer[] = [];

  untrustedPeers.forEach(p => {
      // Don't show my own network in "Nearby Networks"
      if (p.network_name && p.network_name === myNetworkName) {
           return;
      }

      if (p.network_name) {
          if (!otherNetworks[p.network_name]) otherNetworks[p.network_name] = [];
          otherNetworks[p.network_name].push(p);
      } else {
          unknownPeers.push(p);
      }
  });

  return (
    <div className="flex flex-col h-screen w-screen bg-neutral-900 text-white overflow-hidden font-sans relative">
      {/* Header */}
      <header className="flex items-center justify-between px-4 py-3 bg-neutral-800 border-b border-neutral-700 select-none drag-region">
        <div className="flex items-center gap-2">
            <div className="w-3 h-3 rounded-full bg-emerald-500 shadow-[0_0_8px_rgba(16,185,129,0.5)]"></div>
            <h1 className="font-bold text-lg tracking-tight">UCP</h1>
        </div>
        <div className="flex gap-1 bg-neutral-700/50 p-1 rounded-lg no-drag">
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
            <div className="w-px h-6 bg-neutral-600/50 mx-1"></div>
            <button 
                onClick={handleLeaveNetwork}
                className="p-1.5 rounded-md hover:bg-red-500/20 text-neutral-400 hover:text-red-400 transition-colors"
                title="Leave Network"
            >
                <LogOut size={18} />
            </button>
        </div>
      </header>

      {/* Content */}
      <main className="flex-1 overflow-y-auto p-4">
        {activeTab === "devices" && (

            <div className="space-y-6">
                {/* My Network Section */}
                <div>
                    <div className="flex items-center justify-between mb-2">
                        <div className="flex flex-col">
                            <h2 className="text-xs font-semibold text-neutral-500 uppercase tracking-wider">
                                My Network: <span className="text-blue-400">{myNetworkName}</span>
                            </h2>
                            <span className="text-[10px] text-neutral-600 font-mono tracking-widest mt-0.5">PIN: {networkPin}</span>
                        </div>
                        <button onClick={addManualPeer} className="text-neutral-500 hover:text-white transition-colors" title="Add Manual Peer">
                            <PlusCircle size={16} />
                        </button>
                    </div>
                    <div className="space-y-2">
                        {myPeers.length === 0 ? (
                            <div className="p-4 rounded-xl border border-dashed border-neutral-700 text-center text-neutral-500 text-sm">
                                You are the only device in this network.
                            </div>
                        ) : (
                            myPeers.map(peer => (
                                <div key={peer.id} className="group flex items-center justify-between p-3 bg-neutral-800/50 hover:bg-neutral-800 border border-neutral-700/50 hover:border-emerald-500/30 rounded-xl transition-all">
                                    <div className="flex items-center gap-3">
                                        <div className="w-10 h-10 flex items-center justify-center bg-emerald-500/10 text-emerald-400 rounded-lg">
                                            <ShieldCheck size={20} />
                                        </div>
                                        <div>
                                            <div className="font-medium text-sm text-neutral-200">{peer.hostname}</div>
                                            <div className="text-xs text-neutral-500 font-mono">{peer.ip}</div>
                                        </div>
                                    </div>
                                    <button
                                         onClick={(e) => { e.stopPropagation(); deletePeer(peer.id); }}
                                         className="p-1.5 text-neutral-500 hover:text-red-400 bg-neutral-700/50 hover:bg-neutral-700 rounded-md transition-colors opacity-0 group-hover:opacity-100"
                                         title="Forget Device"
                                     >
                                        <Trash2 size={16} />
                                    </button>
                                </div>
                            ))
                        )}
                    </div>
                </div>

                {/* Other Networks Section */}
                {Object.keys(otherNetworks).length > 0 && (
                    <div>
                        <h2 className="text-xs font-semibold text-neutral-500 uppercase tracking-wider mb-2">Nearby Networks</h2>
                        <div className="grid gap-3">
                            {Object.entries(otherNetworks).map(([netName, netPeers]) => (
                                <div key={netName} className="p-4 bg-neutral-800 border border-neutral-700 rounded-xl flex items-center justify-between">
                                    <div>
                                        <h3 className="font-bold text-white text-lg">{netName}</h3>
                                        <p className="text-sm text-neutral-400">
                                            {netPeers.length} device{netPeers.length !== 1 ? 's' : ''} available
                                        </p>
                                        <div className="flex -space-x-2 mt-2">
                                            {netPeers.slice(0, 3).map(p => (
                                                <div key={p.id} className="w-8 h-8 rounded-full bg-neutral-700 border-2 border-neutral-800 flex items-center justify-center text-neutral-400" title={p.hostname}>
                                                    <Monitor size={14} />
                                                </div>
                                            ))}
                                            {netPeers.length > 3 && (
                                                <div className="w-6 h-6 rounded-full bg-neutral-700 border border-neutral-800 flex items-center justify-center text-[10px] text-neutral-300">
                                                    +{netPeers.length - 3}
                                                </div>
                                            )}
                                        </div>
                                    </div>
                                    <button 
                                        onClick={() => startPairing(netPeers[0])} // Use first peer as gateway
                                        className="px-4 py-2 bg-blue-600 hover:bg-blue-500 text-white rounded-lg font-medium text-sm transition-colors"
                                    >
                                        Join
                                    </button>
                                </div>
                            ))}
                        </div>
                    </div>
                )}

                {/* Unknown / Direct Section */}
                {unknownPeers.length > 0 && (
                     <div>
                        <h2 className="text-xs font-semibold text-neutral-500 uppercase tracking-wider mb-2">Unidentified Devices</h2>
                        <div className="space-y-2">
                            {unknownPeers.map(peer => (
                                <div key={peer.id} className="flex items-center justify-between p-3 bg-neutral-800/30 border border-neutral-700/30 rounded-xl">
                                    <div className="flex items-center gap-3">
                                        <div className="w-10 h-10 flex items-center justify-center bg-neutral-700 text-neutral-400 rounded-lg">
                                            <Monitor size={20} />
                                        </div>
                                        <div>
                                            <div className="font-medium text-sm text-neutral-200">{peer.hostname}</div>
                                            <div className="text-xs text-neutral-500 font-mono">{peer.ip}</div>
                                        </div>
                                    </div>
                                    <button 
                                        onClick={() => startPairing(peer)}
                                        className="px-3 py-1.5 bg-neutral-700 hover:bg-neutral-600 text-xs rounded-md transition-colors"
                                    >
                                        Connect
                                    </button>
                                </div>
                            ))}
                        </div>
                     </div>
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

      {/* Pairing Modal */}
      {showPairingModal && (
          <div className="absolute inset-0 bg-black/80 flex items-center justify-center p-4 z-50 backdrop-blur-sm">
              <div className="bg-neutral-800 border border-neutral-700 rounded-xl p-6 w-full max-w-sm shadow-2xl">
                  <h3 className="text-lg font-bold mb-2">
                      {isConnecting ? "Connecting..." : "Join Network"}
                  </h3>
                  
                  {isConnecting ? (
                      <div className="text-center py-4">
                          <div className="w-8 h-8 border-2 border-blue-500 border-t-transparent rounded-full animate-spin mx-auto mb-4"></div>
                          <p className="text-neutral-400 text-sm mb-4">
                              Verifying PIN with {pairingPeer?.hostname}...
                          </p>
                      </div>
                  ) : (
                    <>
                      <p className="text-neutral-400 text-sm mb-4">
                          Enter the PIN displayed on <strong>{pairingPeer?.hostname}</strong> or any other device in that network.
                      </p>
                      
                      <input 
                          type="text" 
                          placeholder="ABC123" 
                          className="w-full bg-neutral-900 border border-neutral-700 rounded-lg px-4 py-3 mb-4 outline-none focus:border-blue-500 font-mono text-center text-xl tracking-widest uppercase"
                          value={pin}
                          onChange={e => setPin(e.target.value.toUpperCase())}
                      />
                      
                      <div className="flex gap-2">
                          <button 
                              onClick={() => setShowPairingModal(false)}
                              className="flex-1 px-4 py-2 bg-neutral-700 hover:bg-neutral-600 rounded-lg transition-colors"
                          >
                              Cancel
                          </button>
                          <button 
                              onClick={submitPairing}
                              className="flex-1 px-4 py-2 bg-blue-600 hover:bg-blue-500 rounded-lg transition-colors font-medium"
                          >
                              Join Network
                          </button>
                      </div>
                    </>
                  )}
              </div>
          </div>
      )}
    </div>
  );
}

export default App;
