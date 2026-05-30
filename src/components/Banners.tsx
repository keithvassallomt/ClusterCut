import { ShieldCheck, AlertTriangle } from "lucide-react";
import { Button } from "./ui";

type LegacyPeer = { id: string; hostname: string };

export function LegacyPeerBanner({ peers, onDismiss }: { peers: LegacyPeer[]; onDismiss: () => void }) {
  return (
    <div className="fixed inset-x-0 top-0 z-40 border-b border-amber-300 bg-amber-50 px-4 py-3 shadow-sm dark:border-amber-700 dark:bg-amber-950/80">
      <div className="mx-auto flex max-w-5xl items-start gap-3">
        <ShieldCheck className="mt-0.5 h-5 w-5 flex-shrink-0 text-amber-600 dark:text-amber-400" />
        <div className="flex-1 text-sm">
          <div className="font-medium text-amber-900 dark:text-amber-100">
            Security upgrade — please re-pair your devices
          </div>
          <div className="mt-1 text-amber-800 dark:text-amber-200">
            {peers.length === 1
              ? `${peers[0].hostname} was paired before this version's TLS upgrade and can no longer receive clipboard data until re-paired.`
              : `${peers.length} devices (${peers.map((p) => p.hostname).join(", ")}) were paired before this version's TLS upgrade and can no longer receive clipboard data until re-paired.`}{" "}
            Re-pair using the PIN flow on the Devices tab.
          </div>
        </div>
        <Button
          variant="ghost"
          onClick={onDismiss}
        >
          Dismiss
        </Button>
      </div>
    </div>
  );
}

export function PairingLockoutBanner({ onReEnable }: { onReEnable: () => void }) {
  return (
    <div className="fixed inset-x-0 top-0 z-40 border-b border-rose-300 bg-rose-50 px-4 py-3 shadow-sm dark:border-rose-700 dark:bg-rose-950/80">
      <div className="mx-auto flex max-w-5xl items-start gap-3">
        <AlertTriangle className="mt-0.5 h-5 w-5 flex-shrink-0 text-rose-600 dark:text-rose-400" />
        <div className="flex-1 text-sm">
          <div className="font-medium text-rose-900 dark:text-rose-100">
            Pairing paused — too many failed attempts
          </div>
          <div className="mt-1 text-rose-800 dark:text-rose-200">
            Another device tried to pair with this one too many times with the wrong PIN. Pairing is disabled until you re-enable it. If you didn't expect this, check that no one else on your network is trying to join.
          </div>
        </div>
        <Button
          variant="primary"
          onClick={onReEnable}
        >
          Re-enable pairing
        </Button>
      </div>
    </div>
  );
}
