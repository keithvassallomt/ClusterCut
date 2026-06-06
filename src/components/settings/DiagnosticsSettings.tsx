import clsx from "clsx";
import { ShieldCheck } from "lucide-react";
import { SectionHeader, Card } from "../ui";
import type { AppSettings } from "../../types";

export function DiagnosticsSettings({
  settings,
  setSettings,
}: {
  settings: AppSettings;
  setSettings: (s: AppSettings) => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {/* Diagnostics — opt-in verbose logging for the pairing channel. Per
          WIRE-PROTOCOL-0.3.1 §H7, the responder logs only a generic
          "pairing failure" line when this is off so observers can't tell a
          wrong-PIN attempt from any other framing/decrypt error. */}
      <Card className="p-4">
        <SectionHeader
          icon={<ShieldCheck className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Diagnostics"
          subtitle="Logging controls for the pairing channel."
        />
        <div className="mt-4 space-y-4 px-1">
          <div className="flex items-start justify-between gap-4">
            <div className="flex-1">
              <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200">
                Verbose pairing logs
              </div>
              <div className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
                Write detailed pairing-channel diagnostics to the log instead of a generic "pairing failed" line. Off by default to avoid leaking whether a failure was a wrong-PIN attempt or a different protocol error.
              </div>
            </div>
            <button
              onClick={() => setSettings({
                ...settings,
                pairing_debug_logs: !settings.pairing_debug_logs,
              })}
              className={clsx(
                "relative h-6 w-11 shrink-0 rounded-full transition-colors",
                settings.pairing_debug_logs ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700"
              )}
            >
              <span
                className={clsx(
                  "block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform",
                  settings.pairing_debug_logs ? "translate-x-6" : "translate-x-1"
                )}
              />
            </button>
          </div>
        </div>
      </Card>
    </div>
  );
}
