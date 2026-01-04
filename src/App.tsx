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
  is_trusted: boolean;
}

function App() {
  const [peers, setPeers] = useState<Peer[]>([]);
  const [clipboardHistory, setClipboardHistory] = useState<string[]>([]);
  const [activeTab, setActiveTab] = useState<"devices" | "history">("devices");

  /* Pairing State */
  const [pairingPeer, setPairingPeer] = useState<Peer | null>(null);
  const [incomingRequest, setIncomingRequest] = useState<{ peer_ip: string; msg: number[] } | null>(null);
  const [pin, setPin] = useState("");
  const [showPairingModal, setShowPairingModal] = useState(false);
  const [pairingStep, setPairingStep] = useState<"init" | "respond">("init");

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

    const unlistenPairing = listen<{ peer_ip: string; msg: number[] }>("pairing-request", (event) => {
        console.log("Received pairing request", event.payload);
        setIncomingRequest(event.payload);
        setPairingStep("respond");
        setShowPairingModal(true);
        // Try to match IP to a known peer for display name
        const peer = peers.find(p => p.ip === event.payload.peer_ip);
        if (peer) setPairingPeer(peer);
    });

    return () => {
      unlistenPeer.then((f) => f());
      unlistenClipboard.then((f) => f());
      unlistenPairing.then((f) => f());
    };
  }, [peers]); // Add peers to dependency to lookup peer on request? Or ref?

  const startPairing = (peer: Peer) => {
      setPairingPeer(peer);
      setPairingStep("init");
      setPin("");
      setShowPairingModal(true);
  };

  const submitPairing = async () => {
      if (!pin) return;
      
      try {
          if (pairingStep === "init" && pairingPeer) {
              await invoke("initiate_pairing", { peerId: pairingPeer.id, pin });
              alert("Pairing Request Sent! Ask the other user to verify.");
              setShowPairingModal(false);
          } else if (pairingStep === "respond") {
              // We need the peer ID. If we found it, great. If not (unknown IP), we might fail.
              // For now assuming we found it via IP or user knows.
              // If incomingRequest doesn't map to peer, we need to handle that.
              
              // Find peer by IP if we haven't already
              let targetId = pairingPeer?.id;
              if (!targetId && incomingRequest) {
                  const p = peers.find(x => x.ip === incomingRequest.peer_ip);
                  if (p) targetId = p.id;
              }

              if (targetId && incomingRequest) {
                  await invoke("respond_to_pairing", { 
                      peerId: targetId, 
                      pin, 
                      requestMsg: incomingRequest.msg 
                  });
                  alert("Pairing Verified! You are now connected.");
                  setShowPairingModal(false);
                  setIncomingRequest(null);
              } else {
                  alert("Could not find peer info for " + incomingRequest?.peer_ip);
              }
          }
      } catch (e) {
          alert("Pairing Failed: " + String(e));
      }
  };

  return (
    <div className="flex flex-col h-screen w-screen bg-neutral-900 text-white overflow-hidden font-sans relative">
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
                                {/* Pairing Button */}
                                <button 
                                    onClick={() => startPairing(peer)}
                                    className="px-3 py-1.5 bg-neutral-700 hover:bg-neutral-600 text-xs rounded-md transition-colors"
                                >
                                    Pair
                                </button>
                                {/* Status Indicator */}
                                {peer.is_trusted && (
                                    <div className="flex items-center gap-1.5 px-2 py-1 bg-emerald-500/10 text-emerald-400 text-xs rounded-md border border-emerald-500/20">
                                        <ShieldCheck size={12} />
                                        <span>Trusted</span>
                                    </div>
                                )}
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

      {/* Pairing Modal */}
      {showPairingModal && (
          <div className="absolute inset-0 bg-black/80 flex items-center justify-center p-4 z-50 backdrop-blur-sm">
              <div className="bg-neutral-800 border border-neutral-700 rounded-xl p-6 w-full max-w-sm shadow-2xl">
                  <h3 className="text-lg font-bold mb-2">
                      {pairingStep === "init" ? "Pair Device" : 
                       pairingStep === "waiting" ? "Waiting..." : "Pairing Request"}
                  </h3>
                  
                  {pairingStep === "waiting" ? (
                      <div className="text-center py-4">
                          <p className="text-neutral-400 text-sm mb-4">
                              Request sent to {pairingPeer?.hostname}.<br/>
                              Please check the other device.
                          </p>
                           <button 
                              onClick={() => setShowPairingModal(false)}
                              className="px-4 py-2 bg-neutral-700 hover:bg-neutral-600 rounded-lg transition-colors text-sm"
                          >
                              Close
                          </button>
                      </div>
                  ) : (
                    <>
                      <p className="text-neutral-400 text-sm mb-4">
                          {pairingStep === "init" 
                            ? `Enter a PIN to pair with ${pairingPeer?.hostname}. Proceed on the other device.` 
                            : `Enter the PIN displayed on the other device (${incomingRequest?.peer_ip}).`
                          }
                      </p>
                      
                      <input 
                          type="text" 
                          placeholder="Enter PIN (e.g. 1234)" 
                          className="w-full bg-neutral-900 border border-neutral-700 rounded-lg px-4 py-3 mb-4 outline-none focus:border-blue-500 font-mono text-center text-xl tracking-widest"
                          value={pin}
                          onChange={e => setPin(e.target.value)}
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
                              {pairingStep === "init" ? "Send Request" : "Verify & Pair"}
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
