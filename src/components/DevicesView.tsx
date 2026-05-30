import { useState } from "react";
import {
  ShieldCheck, Lock, Unlock, AlertTriangle, CheckCircle2,
  ChevronDown, ChevronRight, PlusCircle, Trash2, Wifi,
  Eye, EyeOff, Copy,
} from "lucide-react";
import clsx from "clsx";
import { Badge, SectionHeader, Card, Button, IconButton, Field } from "./ui";
import type { Peer, NearbyNetwork } from "../types";
import { isPeerProtocolCompatible } from "../lib/protocol";

function CopyMini({ text }: { text: string }) {
  return (
    <IconButton label="Copy" onClick={() => navigator.clipboard.writeText(text)} variant="default">
      <Copy className="h-5 w-5 text-zinc-700 dark:text-zinc-200" />
    </IconButton>
  );
}

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
  const [showPin, setShowPin] = useState(false);

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
            label="My Cluster PIN"
            value={showPin ? networkPin : "•".repeat(Math.max(networkPin.length, 6))}
            mono
            action={
              <div className="flex items-center gap-1">
                <IconButton
                  label={showPin ? "Hide PIN" : "Show PIN"}
                  onClick={() => setShowPin(v => !v)}
                  variant="default"
                >
                  {showPin
                    ? <EyeOff className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />
                    : <Eye className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
                </IconButton>
                <IconButton label="Copy PIN" onClick={() => navigator.clipboard.writeText(networkPin)} variant="default">
                  <Copy className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />
                </IconButton>
              </div>
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
                        <div className="flex items-center gap-1.5 text-sm font-semibold text-zinc-900 dark:text-zinc-50">
                          <span className="truncate">{p.hostname || p.id}</span>
                          {!isPeerProtocolCompatible(p) && (
                            <span
                              className="inline-flex"
                              title={`${p.hostname || p.id} is running an older version of ClusterCut and won't be able to send or receive clipboard data. Please upgrade it.`}
                              aria-label="Incompatible version"
                            >
                              <AlertTriangle className="h-4 w-4 shrink-0 text-amber-500" />
                            </span>
                          )}
                        </div>
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
                              {isExpanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
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
                                {d.incompatible && (
                                  <span
                                    className="inline-flex"
                                    title={`${d.hostname || d.id} is running an older version of ClusterCut and can't pair with this device. Please upgrade it.`}
                                    aria-label="Incompatible version"
                                  >
                                    <AlertTriangle className="h-3.5 w-3.5 shrink-0 text-amber-500" />
                                  </span>
                                )}
                              </div>
                            ))}
                          </div>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        </Card>
      </div>

    </div>
  );
}

export { DevicesView };
