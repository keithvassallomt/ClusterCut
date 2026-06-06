import clsx from "clsx";
import { SectionHeader, Card } from "../ui";
import type { AppSettings } from "../../types";

export function FilesSettings({
  settings,
  setSettings,
  onEnableCompressClick,
}: {
  settings: AppSettings;
  setSettings: (s: AppSettings) => void;
  onEnableCompressClick: () => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {/* File Transfer */}
      <Card className="p-4">
        <SectionHeader
          icon={<div className="h-5 w-5 flex items-center justify-center"><svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z" /><polyline points="14 2 14 8 20 8" /></svg></div>}
          title="File Transfer"
          subtitle="Manage how files are shared."
        />
        <div className="mt-4 px-1 space-y-4">
          <div className="flex items-center justify-between">
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Allow File Transfer</div>
              <div className="text-xs text-zinc-500">Send and receive files with clipboard.</div>
            </div>
            <button
              onClick={() => setSettings({ ...settings, enable_file_transfer: !settings.enable_file_transfer })}
              className={clsx("relative h-6 w-11 rounded-full transition-colors", settings.enable_file_transfer ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
            >
              <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.enable_file_transfer ? "translate-x-6" : "translate-x-1")} />
            </button>
          </div>

          {settings.enable_file_transfer && (
            <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-3 dark:border-white/10 dark:bg-white/5">
              <div className="flex flex-col gap-2">
                <div className="flex items-center justify-between">
                  <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Auto-Download Limit</div>
                  <div className="text-xs font-mono text-zinc-500">
                    {(settings.max_auto_download_size / 1024 / 1024).toFixed(0)} MB
                  </div>
                </div>
                <input
                  type="range"
                  min="0"
                  max="500"
                  step="10"
                  value={(settings.max_auto_download_size / 1024 / 1024) || 0}
                  onChange={(e) => {
                    const val = parseInt(e.target.value) * 1024 * 1024;
                    setSettings({ ...settings, max_auto_download_size: val });
                  }}
                  className="h-2 w-full cursor-pointer appearance-none rounded-lg bg-zinc-200 accent-emerald-500 dark:bg-zinc-700"
                />
                <div className="text-[10px] text-zinc-500">
                  Files larger than this must be manually downloaded.
                </div>
              </div>
            </div>
          )}

          {settings.enable_file_transfer && (
            <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-3 dark:border-white/10 dark:bg-white/5">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="text-sm font-medium text-zinc-900 dark:text-zinc-50">Compress File Transfers</div>
                  <div className="text-xs text-zinc-500">
                    Speeds up transfers of large, compressible files (text, code, logs, datasets) on slower links. Files that are already compressed (images, video, archives, etc.) are skipped automatically.
                  </div>
                </div>
                <button
                  onClick={() => {
                    if (!settings.compress_file_transfers) {
                      onEnableCompressClick();
                    } else {
                      setSettings({ ...settings, compress_file_transfers: false });
                    }
                  }}
                  className={clsx("relative h-6 w-11 shrink-0 rounded-full transition-colors", settings.compress_file_transfers ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
                >
                  <span className={clsx("block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform", settings.compress_file_transfers ? "translate-x-6" : "translate-x-1")} />
                </button>
              </div>
            </div>
          )}
        </div>
      </Card>
    </div>
  );
}
