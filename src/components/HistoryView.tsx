import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ArrowDown, ArrowUp, Copy, Download, Send, Trash2 } from "lucide-react";
import { Badge, SectionHeader, Card, Button, IconButton } from "./ui";
import type { HistoryItem } from "../types";
import { timeAgo, formatBytes } from "../lib/format";
import { shortRichLabel } from "../lib/protocol";

// Stateless; one instance reused for all rows rather than allocating per render.
const utf8 = new TextEncoder();

export function HistoryView({ items, onClearHistory }: { items: HistoryItem[]; onClearHistory: () => void }) {
  const [myHostname, setMyHostname] = useState<string>("");
  const [progress, setProgress] = useState<Record<string, { transferred: number, total: number }>>({});
  const [downloadedFiles, setDownloadedFiles] = useState<Record<string, string[]>>({});

  useEffect(() => {
    invoke<string>("get_hostname").then(setMyHostname);

    const unlistenProgress = listen<{ id: string, fileName: string, total: number, transferred: number }>("file-progress", (e) => {
      // Update state
      setProgress(p => ({
        ...p,
        [e.payload.id]: { transferred: e.payload.transferred, total: e.payload.total }
      }));

      // If complete, remove after delay
      if (e.payload.transferred >= e.payload.total) {
        setTimeout(() => {
          setProgress(p => {
            const n = { ...p };
            delete n[e.payload.id];
            return n;
          });
        }, 2000); // 2 seconds delay to see "100%"
      }
    });

    const unlistenReceived = listen<{ id: string, path: string }>("file-received", (e) => {
      setDownloadedFiles(prev => {
        const existing = prev[e.payload.id] || [];
        if (existing.includes(e.payload.path)) return prev;
        return { ...prev, [e.payload.id]: [...existing, e.payload.path] };
      });
    });

    return () => {
      unlistenProgress.then(u => u());
      unlistenReceived.then(u => u());
    };
  }, []);

  const handleSend = async (id: string) => {
    try {
      await invoke("recall_send_history_item", { id });
    } catch (e) {
      console.error("Failed to send:", e);
      alert("Failed to send: " + e);
    }
  };

  const handleLocalCopy = async (id: string) => {
    try {
      await invoke("recall_copy_history_item", { id });
    } catch (e) {
      console.error("Failed to copy:", e);
      alert("Failed to copy: " + e);
    }
  };

  const handleLocalCopyFiles = async (paths: string[]) => {
    try {
      await invoke("set_local_clipboard_files", { paths });
    } catch (e) {
      console.error("Failed to set local clipboard files:", e);
      alert("Failed to set clipboard: " + e);
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await invoke("delete_history_item", { id });
      // Optimistic Update is fine
    } catch (e) {
      console.error("Failed to delete:", e);
    }
  };

  const handleDownloadAll = async (fileId: string, files: { name: string }[], peerId: string) => {
    try {
      for (let i = 0; i < files.length; i++) {
        setProgress(p => ({ ...p, [fileId]: { transferred: 0, total: 100 } }));
        await invoke("request_file", { fileId, fileIndex: i, peerId });
      }
    } catch (e) {
      alert("Download failed: " + e);
      setProgress(p => { const n = { ...p }; delete n[fileId]; return n; });
    }
  };

  return (
    <div className="flex h-full flex-col gap-4 overflow-y-auto pb-4">
      <Card className="p-5">
        <SectionHeader
          icon={<Copy className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Clipboard history"
          subtitle="Recent entries."
          right={
            items.length > 0 ? (
              <Button
                size="sm"
                variant="ghost"
                iconLeft={<Trash2 className="h-4 w-4" />}
                onClick={onClearHistory}
              >
                Clear
              </Button>
            ) : undefined
          }
        />

        <div className="mt-4 space-y-2">
          {items.map((it) => {
            const isMe = it.device === myHostname || it.device === "localhost" || it.origin === "local";
            // Logic check: "origin" in item type is mostly placeholder now if we trust device name.
            // If device name matches myHostname, it is "Sent" (Arrow Up).
            // Else "Received" (Arrow Down).

            return (
              <div
                key={it.id}
                className="rounded-2xl border border-zinc-200 bg-white p-4 dark:border-white/10 dark:bg-white/5"
              >
                <div className="flex flex-col gap-3 md:flex-row md:items-start md:justify-between">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <Badge tone={isMe ? "neutral" : "good"}>
                        {isMe ? (
                          <>
                            <ArrowUp className="h-3.5 w-3.5" /> Sent
                          </>
                        ) : (
                          <>
                            <ArrowDown className="h-3.5 w-3.5" /> {it.device}
                          </>
                        )}
                      </Badge>
                      <span className="text-xs text-zinc-500 dark:text-zinc-400">{timeAgo(it.ts)}</span>
                      {it.formats && it.formats.length > 0 && (
                        <span
                          className="inline-flex items-center rounded-md bg-violet-100 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-violet-700 dark:bg-violet-500/15 dark:text-violet-200"
                          title={`Rich formats: ${it.formats.map(f => f.mime_type).join(", ")}`}
                        >
                          Rich · {it.formats.map(f => shortRichLabel(f.mime_type)).join(", ")}
                        </span>
                      )}
                    </div>
                    {it.text && <div className="mt-2 line-clamp-3 whitespace-pre-wrap text-sm text-zinc-900 dark:text-zinc-50">{it.text}</div>}
                    {it.text && it.text_len > utf8.encode(it.text).byteLength && (
                      <div className="mt-1 text-[11px] text-zinc-500">Large text • {formatBytes(it.text_len)}</div>
                    )}

                    {it.blob && (
                      <div className="mt-2 flex flex-col gap-1 rounded-lg bg-zinc-50 p-2 dark:bg-zinc-800">
                        {it.blob.thumbnail ? (
                          <img
                            src={it.blob.thumbnail}
                            alt="Clipboard image"
                            className="max-h-48 max-w-full rounded-md object-contain"
                          />
                        ) : (
                          <div className="flex h-24 w-full items-center justify-center rounded-md bg-zinc-200 text-3xl text-zinc-500 dark:bg-zinc-700">
                            🖼️
                          </div>
                        )}
                        <div className="text-[11px] text-zinc-500">
                          {it.blob.descriptor && !it.blob.thumbnail ? "Large image (not yet fetched)" : "Image"}
                          {it.blob.width && it.blob.height ? ` • ${it.blob.width}×${it.blob.height}` : ""}
                          {` • ${formatBytes(it.blob.size)}`}
                        </div>
                      </div>
                    )}

                    {it.files && it.files.length > 0 && (
                      <div className="mt-2 space-y-1">
                        {it.files.map((f, idx) => (
                          <div key={idx} className="flex flex-col gap-2 rounded-lg bg-zinc-50 p-2 text-sm dark:bg-zinc-800">
                            <div className="flex items-center justify-between">
                              <div className="flex items-center gap-2 overflow-hidden">
                                <span className="truncate font-medium text-zinc-700 dark:text-zinc-300">{f.name}</span>
                                <span className="shrink-0 text-xs text-zinc-500">({formatBytes(f.size)})</span>
                              </div>
                            </div>
                            {progress[it.id] && (
                              <div className="w-full">
                                <div className="flex justify-between text-[10px] text-zinc-500 mb-1">
                                  <span>Downloading...</span>
                                  <span>{Math.round((progress[it.id].transferred / progress[it.id].total) * 100)}%</span>
                                </div>
                                <div className="h-1.5 w-full overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-700">
                                  <div
                                    className="h-full bg-emerald-500 transition-all duration-300 ease-out"
                                    style={{ width: `${(progress[it.id].transferred / progress[it.id].total) * 100}%` }}
                                  />
                                </div>
                              </div>
                            )}
                          </div>
                        ))}
                      </div>
                    )}
                  </div>

                  <div className="flex items-center justify-end gap-2">
                    {(it.text || it.blob) && (
                      <IconButton
                        label={it.has_backing ? "Copy to Clipboard" : "Content no longer available"}
                        onClick={() => it.has_backing && handleLocalCopy(it.id)}
                        disabled={!it.has_backing}
                      >
                        <Copy className={`h-4 w-4 ${it.has_backing ? "text-zinc-600 dark:text-zinc-300" : "text-zinc-300 dark:text-zinc-600"}`} />
                      </IconButton>
                    )}

                    {!isMe && it.files && it.files.length > 0 && it.sender_id && (
                      <>
                        {downloadedFiles[it.id] && downloadedFiles[it.id].length >= it.files.length ? (
                          <IconButton label="Copy Files" onClick={() => handleLocalCopyFiles(downloadedFiles[it.id])}>
                            <Copy className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                          </IconButton>
                        ) : (
                          <IconButton label="Download All" onClick={() => handleDownloadAll(it.id, it.files!, it.sender_id!)}>
                            <Download className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                          </IconButton>
                        )}
                      </>
                    )}

                    {(it.text || it.blob) && (
                      <IconButton
                        label={it.has_backing ? "Send to Cluster" : "Content no longer available"}
                        onClick={() => it.has_backing && handleSend(it.id)}
                        disabled={!it.has_backing}
                      >
                        <Send className={`h-4 w-4 ${it.has_backing ? "text-emerald-600 dark:text-emerald-400" : "text-emerald-600/30 dark:text-emerald-400/30"}`} />
                      </IconButton>
                    )}

                    <IconButton label="Delete Everywhere" onClick={() => handleDelete(it.id)}>
                      <Trash2 className="h-4 w-4 text-rose-600 dark:text-rose-400" />
                    </IconButton>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      </Card>
    </div>
  );
}
