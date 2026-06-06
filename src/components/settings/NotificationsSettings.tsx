import clsx from "clsx";
import { Info } from "lucide-react";
import { SectionHeader, Card } from "../ui";
import type { AppSettings } from "../../types";

export function NotificationsSettings({
  settings,
  setSettings,
}: {
  settings: AppSettings;
  setSettings: (s: AppSettings) => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {/* Notifications */}
      <Card className="p-4">
        <SectionHeader
          icon={<Info className="h-5 w-5 text-zinc-600 dark:text-zinc-300" />}
          title="Notifications"
          subtitle="Choose what to see."
        />
        <div className="mt-4 px-1 space-y-3">
          {[
            { label: "Device Joins", key: "device_join" as const },
            { label: "Device Leaves", key: "device_leave" as const },
            { label: "Data Sent", key: "data_sent" as const },
            { label: "Data Received", key: "data_received" as const },
          ].map(item => (
            <div key={item.key} className="flex items-center justify-between">
              <div className="text-sm text-zinc-700 dark:text-zinc-300">{item.label}</div>
              <button
                onClick={() => setSettings({
                  ...settings,
                  notifications: { ...settings.notifications, [item.key]: !settings.notifications[item.key] }
                })}
                className={clsx("relative h-5 w-9 rounded-full transition-colors", settings.notifications[item.key] ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
              >
                <span className={clsx("block h-3 w-3 transform rounded-full bg-white shadow-sm transition-transform", settings.notifications[item.key] ? "translate-x-5" : "translate-x-1")} />
              </button>
            </div>
          ))}

          {/* Large File Notification (Root Setting) */}
          <div className="flex items-center justify-between">
            <div className="text-sm text-zinc-700 dark:text-zinc-300">Large File Transfers</div>
            <button
              onClick={() => setSettings({
                ...settings,
                notify_large_files: !settings.notify_large_files
              })}
              className={clsx("relative h-5 w-9 rounded-full transition-colors", settings.notify_large_files ? "bg-emerald-500" : "bg-zinc-200 dark:bg-zinc-700")}
            >
              <span className={clsx("block h-3 w-3 transform rounded-full bg-white shadow-sm transition-transform", settings.notify_large_files ? "translate-x-5" : "translate-x-1")} />
            </button>
          </div>
        </div>
      </Card>
    </div>
  );
}
