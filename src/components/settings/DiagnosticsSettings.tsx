import { useState, useEffect, useRef } from "react";
import clsx from "clsx";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ShieldCheck, Activity } from "lucide-react";
import { SectionHeader, Card } from "../ui";
import type { AppSettings, DiagLevel, DiagnosticEvent } from "../../types";

const LEVEL_ORDER: Record<DiagLevel, number> = { minimal: 0, detailed: 1, debug: 2 };
const LEVELS: DiagLevel[] = ["minimal", "detailed", "debug"];

export function DiagnosticsSettings({
  settings,
  setSettings,
}: {
  settings: AppSettings;
  setSettings: (s: AppSettings) => void;
}) {
  const [events, setEvents] = useState<DiagnosticEvent[]>([]);
  const [level, setLevel] = useState<DiagLevel>("minimal");
  const [paused, setPaused] = useState(false);
  const [autoScroll, setAutoScroll] = useState(true);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    invoke<DiagnosticEvent[]>("get_diagnostic_events").then(setEvents).catch(() => {});
    const un = listen<DiagnosticEvent>("diagnostic-event", (e) => {
      if (pausedRef.current) return;
      setEvents((prev) => [...prev, e.payload].slice(-1000));
    });
    return () => { un.then((f) => f()); };
  }, []);

  // On resume, re-sync from the backend buffer so nothing is missed while paused.
  useEffect(() => {
    if (!paused) invoke<DiagnosticEvent[]>("get_diagnostic_events").then(setEvents).catch(() => {});
  }, [paused]);

  const shown = events.filter((ev) => LEVEL_ORDER[ev.level] <= LEVEL_ORDER[level]);

  useEffect(() => {
    if (autoScroll && listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [shown.length, autoScroll]);

  const clearEvents = () => {
    invoke("clear_diagnostic_events").catch(() => {});
    setEvents([]);
  };
  const copyAll = () => {
    const text = shown
      .map((ev) => `${new Date(ev.ts_ms).toLocaleTimeString()} [${ev.level}] ${ev.kind}${ev.peer ? " " + ev.peer : ""} ${ev.message}`)
      .join("\n");
    navigator.clipboard.writeText(text).catch(() => {});
  };

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

      {/* In-memory event log — live pairing/connection diagnostics, never
          written to disk. Populated by the backend `diagnostic-event` stream
          and the `get_diagnostic_events` snapshot. */}
      <Card className="p-4">
        <SectionHeader
          icon={<Activity className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Event Log"
          subtitle="In-memory pairing & connection diagnostics (not written to disk)."
        />
        <div className="mt-4 flex flex-wrap items-center gap-2">
          <label className="flex items-center gap-1.5 text-xs text-zinc-600 dark:text-zinc-300">
            Level
            <select
              value={level}
              onChange={(e) => setLevel(e.target.value as DiagLevel)}
              className="rounded-md border border-zinc-300 bg-white px-2 py-1 text-xs text-zinc-900 dark:border-white/15 dark:bg-zinc-800 dark:text-zinc-100"
            >
              {LEVELS.map((l) => (
                <option
                  key={l}
                  value={l}
                  className="bg-white text-zinc-900 dark:bg-zinc-800 dark:text-zinc-100"
                >
                  {l.charAt(0).toUpperCase() + l.slice(1)}
                </option>
              ))}
            </select>
          </label>

          <button
            onClick={() => setPaused((p) => !p)}
            className={clsx(
              "rounded-md border px-2 py-1 text-xs transition-colors",
              paused
                ? "border-emerald-500 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
                : "border-zinc-300 text-zinc-700 dark:border-white/15 dark:text-zinc-300"
            )}
          >
            {paused ? "Paused" : "Pause"}
          </button>

          <button
            onClick={() => setAutoScroll((a) => !a)}
            className={clsx(
              "rounded-md border px-2 py-1 text-xs transition-colors",
              autoScroll
                ? "border-emerald-500 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
                : "border-zinc-300 text-zinc-700 dark:border-white/15 dark:text-zinc-300"
            )}
          >
            Auto-scroll
          </button>

          <button
            onClick={copyAll}
            className="rounded-md border border-zinc-300 px-2 py-1 text-xs text-zinc-700 transition-colors hover:bg-zinc-50 dark:border-white/15 dark:text-zinc-300 dark:hover:bg-white/5"
          >
            Copy all
          </button>

          <button
            onClick={clearEvents}
            className="rounded-md border border-rose-300 px-2 py-1 text-xs text-rose-600 transition-colors hover:bg-rose-50 dark:border-rose-500/30 dark:text-rose-400 dark:hover:bg-rose-500/10"
          >
            Clear
          </button>
        </div>

        <div
          ref={listRef}
          className="mt-3 max-h-60 overflow-y-auto rounded-lg border border-zinc-200 bg-zinc-50 p-2 font-mono text-[11px] dark:border-white/10 dark:bg-white/5"
        >
          {shown.length === 0 ? (
            <div className="text-zinc-400">No events.</div>
          ) : (
            shown.map((ev, i) => (
              <div key={i} className="flex gap-2 py-0.5">
                <span className="text-zinc-400">{new Date(ev.ts_ms).toLocaleTimeString()}</span>
                <span className={clsx("font-semibold", ev.level === "debug" ? "text-amber-500" : ev.kind === "mtls" ? "text-blue-500" : "text-emerald-600 dark:text-emerald-400")}>{ev.kind}</span>
                {ev.peer && <span className="text-zinc-500">{ev.peer}</span>}
                <span className="text-zinc-700 dark:text-zinc-300">{ev.message}</span>
              </div>
            ))
          )}
        </div>
      </Card>
    </div>
  );
}
