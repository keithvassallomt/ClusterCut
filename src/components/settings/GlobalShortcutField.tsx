import { Fragment, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Edit2 } from "lucide-react";
import { Kbd, ShortcutRecorder } from "../ShortcutRecorder";
import { Button } from "../ui";
import type { ShortcutStatus } from "../../types";

const KEY_LABELS: Record<string, string> = {
  control: "Ctrl", ctrl: "Ctrl", primary: "Ctrl",
  alt: "Alt", shift: "Shift",
  super: "Super", meta: "Super", logo: "Super",
  return: "Enter", enter: "Enter", space: "Space", backspace: "Backspace",
  page_up: "PageUp", prior: "PageUp", page_down: "PageDown", next: "PageDown",
};

const keyLabel = (key: string) => KEY_LABELS[key.toLowerCase()] ?? (key.length === 1 ? key.toUpperCase() : key);

// The desktop's own description of a bound key, as key caps. GNOME says
// "Press <Control><Alt>c", KDE "Ctrl+Alt+C"; several keys may be joined by
// "or" or commas. Null when unrecognisable, so the caller shows the raw text.
function parseTrigger(description: string): string[][] | null {
  const text = description.trim().replace(/^press\b\s*/i, "");
  if (!text) return null;
  const combos = text.split(/\s*(?:,|\bor\b)\s*/i).filter(Boolean).map((combo) => {
    if (combo.includes("<")) {
      // GTK accelerator: <Control><Alt>c
      const mods = [...combo.matchAll(/<([^>]+)>/g)].map((m) => m[1]);
      const key = combo.replace(/<[^>]+>/g, "").trim();
      return key ? [...mods, key] : null;
    }
    const parts = combo.split("+").map((p) => p.trim()).filter(Boolean);
    return parts.length > 0 ? parts : null;
  });
  if (combos.length === 0 || combos.some((c) => c === null || c.some((key) => /\s/.test(key)))) return null;
  return (combos as string[][]).map((keys) => keys.map(keyLabel));
}

// For desktops with no shortcuts portal (Sway, older GNOME): users bind a key
// to this command themselves.
const dbusCommand = (method: string) =>
  `gdbus call --session --dest app.clustercut.clustercut --object-path /org/gnome/Shell/Extensions/ClusterCut --method app.clustercut.clustercut.${method}`;

function Hint({ children, warning }: { children: ReactNode; warning?: boolean }) {
  return <div className={warning ? "text-[10px] text-amber-600 dark:text-amber-400" : "text-[10px] text-zinc-500"}>{children}</div>;
}

function Code({ children }: { children: ReactNode }) {
  return (
    <code className="select-all break-all rounded-lg bg-zinc-100 p-2 font-mono text-[11px] text-zinc-700 dark:bg-white/5 dark:text-zinc-300">
      {children}
    </code>
  );
}

// The key control for one manual shortcut. On X11, Windows and macOS that's
// the recorder; on Wayland the desktop owns the key, so this shows what it
// bound and how to change it.
export function GlobalShortcutField({
  shortcutId,
  dbusMethod,
  value,
  onChange,
  status,
  hyprlandBindKeys,
}: {
  shortcutId: string;
  dbusMethod: string;
  value: string | null;
  onChange: (value: string | null) => void;
  status: ShortcutStatus | null;
  hyprlandBindKeys: boolean;
}) {
  const recorder = <ShortcutRecorder value={value} onChange={onChange} placeholder="No shortcut set" />;
  if (!status || status.backend === "native") return recorder;

  const portal = status.portal;
  const hyprland = status.hyprland;

  // Hyprland's portal never assigns keys: ClusterCut binds them (opt-in) or the user does.
  if (hyprland && portal?.state === "bound") {
    if (hyprlandBindKeys) {
      const conflict = hyprland.conflicts[shortcutId];
      return (
        <>
          {recorder}
          {conflict ? (
            <Hint warning>{conflict} is already bound in Hyprland, so ClusterCut left it alone. Pick another key.</Hint>
          ) : hyprland.bound[shortcutId] ? (
            <Hint>Bound in Hyprland while ClusterCut runs.</Hint>
          ) : null}
        </>
      );
    }
    const line = hyprland.config_lines[shortcutId];
    return (
      <>
        <Hint>Hyprland doesn't assign keys to app shortcuts. Add this to your Hyprland config, or turn on "Bind Keys in Hyprland" below.</Hint>
        {line && <Code>{line}</Code>}
      </>
    );
  }

  switch (portal?.state) {
    case "bound": {
      // Laid out like the recorder, but "Change" opens the desktop's own settings.
      const trigger = portal.triggers[shortcutId];
      const combos = trigger ? parseTrigger(trigger) : null;
      return (
        <div className="flex items-center justify-between rounded-lg border border-zinc-200 bg-white p-2 dark:border-white/10 dark:bg-zinc-900">
          <div className="flex flex-wrap items-center gap-1">
            {combos ? (
              combos.map((keys, i) => (
                <Fragment key={i}>
                  {i > 0 && <span className="px-1 text-xs text-zinc-400">or</span>}
                  {keys.map((key, j) => <Kbd key={j} label={key} />)}
                </Fragment>
              ))
            ) : trigger ? (
              <span className="text-sm text-zinc-700 dark:text-zinc-300">{trigger}</span>
            ) : (
              <span className="text-sm text-zinc-400">Not assigned yet</span>
            )}
          </div>
          {portal.version >= 2 && (
            <button
              onClick={() => invoke("configure_portal_shortcuts")}
              title="Opens your desktop's shortcut settings"
              className="flex items-center gap-1.5 rounded bg-zinc-100 px-2 py-1 text-xs font-medium text-zinc-700 hover:bg-zinc-200 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
            >
              <Edit2 className="h-3 w-3" />
              Change
            </button>
          )}
        </div>
      );
    }
    case "declined":
      return (
        <div className="flex items-center justify-between gap-2">
          <Hint>You declined the desktop's shortcut request.</Hint>
          <Button size="sm" onClick={() => invoke("retry_portal_shortcuts")}>Ask Again</Button>
        </div>
      );
    case "unavailable":
      return (
        <>
          <Hint>This desktop can't assign global shortcuts to apps. Bind a key in your desktop's settings to run:</Hint>
          <Code>{dbusCommand(dbusMethod)}</Code>
        </>
      );
    case "error":
      return (
        <div className="flex items-center justify-between gap-2">
          <Hint warning>Couldn't set up the shortcut: {portal.message}</Hint>
          <Button size="sm" onClick={() => invoke("retry_portal_shortcuts")}>Try Again</Button>
        </div>
      );
    default:
      return <Hint>Setting up the shortcut…</Hint>;
  }
}
