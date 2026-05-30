import clsx from "clsx";
import { AlertTriangle, PlusCircle } from "lucide-react";
import { Button, Modal } from "./ui";

/* --- IncompatibleModal --- */

interface IncompatibleModalProps {
  open: boolean;
  hostname: string;
  onClose: () => void;
}

export function IncompatibleModal({ open, hostname, onClose }: IncompatibleModalProps) {
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4 backdrop-blur-sm">
      <div className="w-full max-w-sm overflow-hidden rounded-2xl bg-white shadow-2xl ring-1 ring-zinc-900/10 dark:bg-zinc-900 dark:ring-white/10">
        <div className="p-6">
          <div className="flex items-center gap-3">
            <AlertTriangle className="h-5 w-5 shrink-0 text-amber-500" />
            <h3 className="text-lg font-semibold text-zinc-900 dark:text-zinc-50">
              Peer needs updating
            </h3>
          </div>
          <p className="mt-3 text-sm text-zinc-600 dark:text-zinc-400">
            <strong>{hostname}</strong> is running an older version of ClusterCut and cannot receive this data. Please upgrade it to the latest version.
          </p>
        </div>
        <div className="flex justify-end gap-2 bg-zinc-50 px-6 py-4 dark:bg-zinc-800/50">
          <Button
            variant="primary"
            onClick={onClose}
          >
            OK
          </Button>
        </div>
      </div>
    </div>
  );
}

/* --- ConnectionFailedModal --- */

interface ConnectionFailedModalProps {
  open: boolean;
  onRetry: () => void;
  onLeave: () => void;
  onExit: () => void;
  onDoNothing: () => void;
}

export function ConnectionFailedModal({ open, onRetry, onLeave, onExit, onDoNothing }: ConnectionFailedModalProps) {
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4 backdrop-blur-sm">
      <div className="w-full max-w-sm overflow-hidden rounded-2xl bg-white shadow-2xl ring-1 ring-zinc-900/10 dark:bg-zinc-900 dark:ring-white/10">
        <div className="p-6">
          <h3 className="text-lg font-semibold text-zinc-900 dark:text-zinc-50">Connection Failed</h3>
          <p className="mt-2 text-sm text-zinc-500 dark:text-zinc-400">
            Could not connect to the remote cluster. What would you like to do?
          </p>
        </div>
        <div className="flex flex-col gap-2 bg-zinc-50 px-6 py-4 dark:bg-zinc-800/50">
          <Button variant="primary" onClick={onRetry}>
            Retry Connection
          </Button>
          <Button variant="danger" onClick={onLeave}>
            Leave Cluster
          </Button>
          <Button variant="default" onClick={onExit}>
            Exit Application
          </Button>
          <Button variant="ghost" onClick={onDoNothing}>
            Do nothing
          </Button>
        </div>
      </div>
    </div>
  );
}

/* --- JoinModal --- */

interface JoinModalProps {
  open: boolean;
  joinTarget: string;
  joinPin: string;
  joinError: string;
  joinBusy: boolean;
  onPinChange: (value: string) => void;
  onClearError: () => void;
  onSubmit: () => void;
  onClose: () => void;
}

export function JoinModal({
  open,
  joinTarget,
  joinPin,
  joinError,
  joinBusy,
  onPinChange,
  onClearError,
  onSubmit,
  onClose,
}: JoinModalProps) {
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={`Join "${joinTarget}"`}
      subtitle="Enter the 6-character Network PIN shown on any device in that cluster."
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={onSubmit} disabled={joinBusy || joinPin.trim().length < 6} iconLeft={<PlusCircle className="h-4 w-4" />}>
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
                : "border-zinc-200 focus:ring-emerald-500/40 dark:border-white/10"
            )}
            placeholder="••••••"
            value={joinPin}
            onChange={(e) => {
              onPinChange(e.target.value.trim());
              onClearError();
            }}
            onKeyDown={(e) => e.key === "Enter" && onSubmit()}
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
  );
}

/* --- LeaveModal --- */

interface LeaveModalProps {
  open: boolean;
  onConfirm: () => void;
  onClose: () => void;
}

export function LeaveModal({ open, onConfirm, onClose }: LeaveModalProps) {
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Leave network?"
      subtitle="This wipes this device's identity, keys, and trusted peers (factory reset)."
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="danger" onClick={onConfirm} iconLeft={<AlertTriangle className="h-4 w-4" />}>
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
  );
}

/* --- AddRemoteModal --- */

interface AddRemoteModalProps {
  open: boolean;
  manualIp: string;
  manualBusy: boolean;
  onIpChange: (value: string) => void;
  onSubmit: () => void;
  onClose: () => void;
}

export function AddRemoteModal({
  open,
  manualIp,
  manualBusy,
  onIpChange,
  onSubmit,
  onClose,
}: AddRemoteModalProps) {
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Add Remote Peer"
      subtitle="Enter an IP address to pair with a remote peer, or a CIDR range (e.g. 192.168.1.0/24) to rediscover already-paired peers."
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={onSubmit} disabled={manualBusy || !manualIp} iconLeft={<PlusCircle className="h-4 w-4" />}>
            {manualBusy ? "Scanning..." : "Add"}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="rounded-2xl border border-zinc-900/10 bg-zinc-50 p-4 dark:border-white/10 dark:bg-white/5">
          <div className="text-xs font-medium text-zinc-600 dark:text-zinc-400">IP Address / CIDR</div>
          <input
            className="mt-2 h-12 w-full rounded-2xl border border-zinc-200 bg-white px-4 text-zinc-900 outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-white/10 dark:bg-zinc-950 dark:text-zinc-50"
            placeholder="e.g. 10.8.0.5 or 192.168.1.0/24"
            value={manualIp}
            onChange={(e) => onIpChange(e.target.value)}
            autoFocus
            onKeyDown={(e) => e.key === "Enter" && onSubmit()}
          />
          <div className="mt-2 text-xs text-zinc-500">
            Target must be running ClusterCut on the default port (4654).
          </div>
        </div>
      </div>
    </Modal>
  );
}

/* --- PortWarningModal --- */

interface PortWarningModalProps {
  open: boolean;
  currentPort: number;
  onClose: () => void;
}

export function PortWarningModal({ open, currentPort, onClose }: PortWarningModalProps) {
  return (
    <Modal
      open={open}
      title="Non-Standard Port Detected"
      subtitle="ClusterCut is running on a fallback port."
      onClose={onClose}
      footer={
        <Button onClick={onClose}>
          OK
        </Button>
      }
    >
      <div className="space-y-4">
        <div className="flex items-center gap-3 rounded-xl border border-amber-200 bg-amber-50 p-4 text-amber-900 dark:border-amber-900/30 dark:bg-amber-900/10 dark:text-amber-200">
          <AlertTriangle className="h-5 w-5 shrink-0" />
          <div className="text-sm">
            <span className="font-semibold">Port 4654 is busy.</span>
            <p className="mt-1 opacity-90">
              ClusterCut is listening on port <span className="font-mono font-bold">{currentPort}</span> instead.
            </p>
          </div>
        </div>
        <p className="text-sm text-zinc-600 dark:text-zinc-400">
          This usually happens if another instance of ClusterCut is already running.
          Peer discovery might be affected if other devices expect the standard port.
        </p>
      </div>
    </Modal>
  );
}
