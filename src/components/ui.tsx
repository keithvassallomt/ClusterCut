import type { ReactNode, ButtonHTMLAttributes } from "react";
import clsx from "clsx";

export function Badge({
  tone = "neutral",
  children,
}: {
  tone?: "neutral" | "good" | "warn" | "bad";
  children: ReactNode;
}) {
  const classes =
    tone === "good"
      ? "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300 border-emerald-500/25"
      : tone === "warn"
        ? "bg-amber-500/15 text-amber-700 dark:text-amber-300 border-amber-500/25"
        : tone === "bad"
          ? "bg-rose-500/15 text-rose-700 dark:text-rose-300 border-rose-500/25"
          : "bg-zinc-100 text-zinc-700 dark:bg-zinc-500/10 dark:text-zinc-300 border-zinc-200 dark:border-zinc-500/20";

  return (
    <span className={clsx("inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium", classes)}>
      {children}
    </span>
  );
}

export function SectionHeader({
  icon,
  title,
  subtitle,
  right,
}: {
  icon: ReactNode;
  title: string;
  subtitle?: string;
  right?: ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-3">
      <div className="flex items-start gap-3">
        <div className="mt-0.5 inline-flex h-9 w-9 items-center justify-center rounded-xl bg-zinc-100 dark:bg-zinc-800/50">
          {icon}
        </div>
        <div>
          <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-50">{title}</div>
          {subtitle ? <div className="text-xs text-zinc-600 dark:text-zinc-400">{subtitle}</div> : null}
        </div>
      </div>
      {right ? <div className="flex items-center gap-2">{right}</div> : null}
    </div>
  );
}

export function Card({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={clsx(
        "rounded-2xl border border-zinc-200 bg-white/70 shadow-sm backdrop-blur dark:border-white/10 dark:bg-zinc-900/40",
        className
      )}
    >
      {children}
    </div>
  );
}

export function Button({
  variant = "default",
  size = "md",
  iconLeft,
  iconRight,
  children,
  className,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "default" | "primary" | "ghost" | "danger";
  size?: "sm" | "md";
  iconLeft?: ReactNode;
  iconRight?: ReactNode;
}) {
  const base =
    "inline-flex select-none items-center justify-center gap-2 rounded-xl font-medium transition focus:outline-none focus:ring-2 focus:ring-emerald-500/40 disabled:opacity-50 disabled:cursor-not-allowed";
  const sizes = size === "sm" ? "h-9 px-3 text-sm" : "h-11 px-4 text-sm";
  const variants =
    variant === "primary"
      ? "bg-emerald-600 text-white hover:bg-emerald-700"
      : variant === "danger"
        ? "bg-rose-600 text-white hover:bg-rose-700"
        : variant === "ghost"
          ? "bg-transparent hover:bg-zinc-900/5 dark:hover:bg-white/5 text-zinc-800 dark:text-zinc-100"
          : "bg-zinc-100 hover:bg-zinc-200 text-zinc-900 dark:bg-white/5 dark:hover:bg-white/10 dark:text-zinc-50";

  return (
    <button className={clsx(base, sizes, variants, "min-w-[44px]", className)} {...props}>
      {iconLeft}
      <span>{children}</span>
      {iconRight}
    </button>
  );
}

export function IconButton({
  label,
  onClick,
  children,
  variant = "ghost",
  active = false,
  danger = false,
  disabled = false,
}: {
  label: string;
  onClick?: () => void;
  children: ReactNode;
  variant?: "ghost" | "default";
  active?: boolean;
  danger?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={disabled ? undefined : onClick}
      disabled={disabled}
      className={clsx(
        "group relative flex h-10 w-10 items-center justify-center rounded-xl transition focus:outline-none focus:ring-2 focus:ring-emerald-500/40 no-drag",
        disabled && "cursor-not-allowed opacity-60",
        active
          ? "bg-white text-zinc-900 shadow-sm dark:bg-zinc-800 dark:text-zinc-50"
          : danger
            ? "text-red-500 hover:bg-red-50 dark:hover:bg-red-900/20"
            : "text-zinc-500 hover:bg-zinc-900/5 hover:text-zinc-900 dark:text-zinc-400 dark:hover:bg-white/5 dark:hover:text-zinc-50",
        variant === "default" && !active && !danger && "bg-zinc-100 dark:bg-white/5"
      )}
    >
      {children}

      {/* Tooltip */}
      <span className="pointer-events-none absolute top-full mt-2 hidden whitespace-nowrap rounded-lg bg-zinc-900 px-2 py-1 text-xs font-medium text-white opacity-0 shadow-lg transition group-hover:block group-hover:opacity-100 dark:bg-white dark:text-zinc-900 z-50">
        {label}
      </span>
    </button>
  );
}

export function Field({
  label,
  value,
  mono = false,
  action,
}: {
  label: string;
  value: string;
  mono?: boolean;
  action?: ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-3 rounded-2xl border border-zinc-200 bg-white p-4 dark:border-white/10 dark:bg-white/5">
      <div className="min-w-0">
        <div className="text-xs font-medium text-zinc-600 dark:text-zinc-400">{label}</div>
        <div className={clsx("mt-1 truncate text-sm font-semibold text-zinc-900 dark:text-zinc-50", mono && "font-mono tracking-wide")}>
          {value}
        </div>
      </div>
      {action ? <div className="shrink-0">{action}</div> : null}
    </div>
  );
}

export function Modal({
  open,
  title,
  subtitle,
  children,
  footer,
  onClose,
}: {
  open: boolean;
  title: string;
  subtitle?: string;
  children: ReactNode;
  footer: ReactNode;
  onClose: () => void;
}) {
  if (!open) return null;

  return (
    <div className="no-drag fixed inset-0 z-50 flex items-end justify-center bg-black/40 p-4 backdrop-blur-sm md:items-center">
      <div className="w-full max-w-lg overflow-hidden rounded-3xl border border-zinc-200 bg-white shadow-2xl dark:border-white/10 dark:bg-zinc-950">
        <div className="flex items-start justify-between gap-3 p-5">
          <div>
            <div className="text-sm font-semibold text-zinc-900 dark:text-zinc-50">{title}</div>
            {subtitle ? <div className="mt-1 text-xs text-zinc-600 dark:text-zinc-400">{subtitle}</div> : null}
          </div>
          <IconButton label="Close" onClick={onClose}>
            <span className="text-xl leading-none text-zinc-500">×</span>
          </IconButton>
        </div>
        <div className="px-5 pb-5">{children}</div>
        <div className="flex flex-col gap-2 border-t border-zinc-200 bg-zinc-50 p-4 dark:border-white/10 dark:bg-zinc-900/30 md:flex-row md:items-center md:justify-end">
          {footer}
        </div>
      </div>
    </div>
  );
}
