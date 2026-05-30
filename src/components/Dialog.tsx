import { Button } from "./ui";

export function Dialog({
  open,
  title,
  description,
  onConfirm,
  onCancel,
  confirmLabel = "Confirm",
  cancelLabel = "Cancel",
  type = "neutral"
}: {
  open: boolean;
  title: string;
  description: string;
  onConfirm: () => void;
  onCancel?: () => void;
  confirmLabel?: string;
  cancelLabel?: string;
  type?: "neutral" | "danger" | "success";
}) {
  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4 backdrop-blur-sm">
      <div className="w-full max-w-sm overflow-hidden rounded-2xl bg-white shadow-2xl ring-1 ring-zinc-900/10 dark:bg-zinc-900 dark:ring-white/10">
        <div className="p-6">
          <h3 className="text-lg font-semibold text-zinc-900 dark:text-zinc-50">{title}</h3>
          <p className="mt-2 text-sm text-zinc-500 dark:text-zinc-400">{description}</p>
        </div>
        <div className="flex justify-end gap-3 bg-zinc-50 px-6 py-4 dark:bg-zinc-800/50">
          {onCancel && (
            <Button variant="default" onClick={onCancel}>
              {cancelLabel}
            </Button>
          )}
          <Button
            variant={type === "danger" ? "danger" : "primary"}
            onClick={onConfirm}
          >
            {confirmLabel}
          </Button>
        </div>
      </div>
    </div>
  );
}
