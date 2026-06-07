import { ArrowDown, ArrowUp, Send, Copy } from "lucide-react";
import { Button } from "./ui";
import type { ClipboardBlobPreview, ClipboardFormatPreview } from "../types";
import { formatBytes } from "../lib/format";
import { shortRichLabel } from "../lib/protocol";

export function ManualSyncFAB({
  hasPendingSend,
  hasPendingReceive,
  onClick
}: {
  hasPendingSend: boolean,
  hasPendingReceive: boolean,
  onClick: () => void
}) {
  if (!hasPendingSend && !hasPendingReceive) return null;

  return (
    <button
      onClick={onClick}
      className="fixed bottom-6 right-6 z-50 flex h-14 w-14 items-center justify-center rounded-full bg-emerald-600 text-white shadow-xl shadow-emerald-600/30 transition hover:scale-105 hover:bg-emerald-500 focus:outline-none focus:ring-4 focus:ring-emerald-500/30"
    >
      {hasPendingReceive ? (
        <ArrowDown className="h-6 w-6" />
      ) : (
        <Send className="h-6 w-6 pl-0.5" />
      )}
      <span className="absolute -top-1 -right-1 flex h-4 w-4 items-center justify-center rounded-full bg-rose-500 text-[10px] font-bold text-white shadow-sm ring-2 ring-white dark:ring-zinc-900">
        !
      </span>
    </button>
  );
}

export function ManualSyncModal({
  open,
  onClose,
  localContent,
  remoteContent, // ClipboardPayload or string
  onSend,
  onReceive
}: {
  open: boolean;
  onClose: () => void;
  localContent: string;
  remoteContent: { text: string, sender: string, timestamp: number, blob?: ClipboardBlobPreview, formats?: ClipboardFormatPreview[] } | null;
  onSend: () => void;
  onReceive: () => void;
}) {
  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4 backdrop-blur-sm">
      <div className="w-full max-w-2xl overflow-hidden rounded-3xl bg-zinc-950 shadow-2xl ring-1 ring-white/10 text-zinc-50">
        <div className="flex items-center justify-between border-b border-white/10 p-5">
          <h3 className="text-lg font-semibold">Synchronization</h3>
          <button onClick={onClose} className="rounded-lg p-1 hover:bg-white/10">
            <span className="text-xl leading-none text-zinc-400">×</span>
          </button>
        </div>

        <div className="grid grid-cols-1 md:grid-cols-2 divide-y md:divide-y-0 md:divide-x divide-white/10">
          {/* Send Column */}
          <div className="flex flex-col p-6 gap-4">
            <div className="flex items-center gap-2 text-emerald-400">
              <ArrowUp className="h-5 w-5" />
              <span className="font-medium">Send Local</span>
            </div>
            <div className="flex-1 rounded-xl bg-white/5 p-4 text-sm font-mono text-zinc-300 h-32 overflow-y-auto whitespace-pre-wrap border border-white/5">
              {localContent || <span className="text-zinc-600 italic">Clipboard empty</span>}
            </div>
            <Button variant="primary" onClick={onSend} disabled={!localContent} iconLeft={<Send className="h-4 w-4" />}>
              Broadcast to Cluster
            </Button>
          </div>

          {/* Receive Column */}
          <div className="flex flex-col p-6 gap-4">
            <div className="flex items-center gap-2 text-blue-400">
              <ArrowDown className="h-5 w-5" />
              <span className="font-medium">Receive Remote</span>
            </div>
            <div className="flex-1 rounded-xl bg-white/5 p-4 text-sm font-mono text-zinc-300 h-32 overflow-y-auto whitespace-pre-wrap border border-white/5 relative">
              {remoteContent ? (
                <>
                  {remoteContent.blob ? (
                    <div className="flex items-start gap-3 font-sans">
                      {remoteContent.blob.thumbnail ? (
                        <img
                          src={remoteContent.blob.thumbnail}
                          alt="Pending image"
                          className="max-h-24 max-w-[40%] rounded object-contain"
                        />
                      ) : (
                        <div className="flex h-24 w-24 items-center justify-center rounded bg-white/5 text-2xl text-zinc-500">
                          🖼️
                        </div>
                      )}
                      <div className="text-xs text-zinc-300">
                        {remoteContent.blob.descriptor ? "Large image (not yet fetched)" : "Image"}
                        {remoteContent.blob.width && remoteContent.blob.height
                          ? ` (${remoteContent.blob.width}×${remoteContent.blob.height})`
                          : ""}
                        <div className="text-zinc-500">{formatBytes(remoteContent.blob.size)}</div>
                        {remoteContent.blob.descriptor && (
                          <div className="mt-1 text-amber-400">Click "Apply to Clipboard" to download.</div>
                        )}
                      </div>
                    </div>
                  ) : (
                    <>
                      {remoteContent.formats && remoteContent.formats.length > 0 && (
                        <div className="mb-2 inline-flex items-center rounded-md bg-violet-500/15 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-violet-200 font-sans">
                          Rich · {remoteContent.formats.map(f => shortRichLabel(f.mime_type)).join(", ")}
                        </div>
                      )}
                      {remoteContent.text}
                    </>
                  )}
                  <div className="absolute bottom-2 right-2 flex gap-2">
                    <span className="text-[10px] bg-white/10 px-2 py-0.5 rounded text-zinc-400">
                      From: {remoteContent.sender}
                    </span>
                  </div>
                </>
              ) : (
                <span className="text-zinc-600 italic">No pending data</span>
              )}
            </div>
            <Button variant="primary" onClick={onReceive} disabled={!remoteContent} iconLeft={<Copy className="h-4 w-4" />}>
              Apply to Clipboard
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
