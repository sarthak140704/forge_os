import { cn } from "@/lib/utils";
import type { ButtonHTMLAttributes, HTMLAttributes, ReactNode } from "react";

export function Button({
  children,
  className,
  variant = "primary",
  size = "md",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "secondary" | "ghost" | "danger" | "subtle";
  size?: "sm" | "md";
  children: ReactNode;
}) {
  const base =
    "inline-flex items-center justify-center gap-1.5 rounded-md font-medium transition-all " +
    "outline-none focus-visible:ring-2 focus-visible:ring-forge-accent/60 " +
    "active:scale-[0.98] disabled:opacity-50 disabled:cursor-not-allowed disabled:active:scale-100";
  const sizes = {
    sm: "px-2.5 py-1 text-xs",
    md: "px-3 py-1.5 text-sm",
  }[size];
  const styles = {
    primary:
      "bg-accent-grad text-white shadow-sm hover:brightness-110 hover:shadow-glow",
    secondary:
      "bg-forge-panel2 text-forge-fg border border-forge-border hover:border-forge-borderStrong hover:bg-forge-panel",
    ghost:
      "text-forge-muted hover:text-forge-fg hover:bg-forge-panel2",
    subtle:
      "bg-forge-panel2/60 text-forge-muted hover:text-forge-fg hover:bg-forge-panel2",
    danger:
      "bg-forge-err/90 hover:bg-forge-err text-white shadow-sm",
  }[variant];
  return (
    <button className={cn(base, sizes, styles, className)} {...rest}>
      {children}
    </button>
  );
}

export function Card({
  children,
  className,
  hover = false,
  ...rest
}: HTMLAttributes<HTMLDivElement> & {
  children: ReactNode;
  hover?: boolean;
}) {
  return (
    <div
      className={cn(
        "bg-forge-panel/90 border border-forge-border rounded-xl shadow-card",
        hover && "transition-colors hover:border-forge-borderStrong",
        className
      )}
      {...rest}
    >
      {children}
    </div>
  );
}

export function Badge({
  children,
  tone = "default",
  dot = false,
}: {
  children: ReactNode;
  tone?: "default" | "success" | "warn" | "err" | "info";
  dot?: boolean;
}) {
  const style = {
    default: "bg-forge-panel2 text-forge-muted ring-1 ring-inset ring-forge-border",
    success: "bg-forge-success/12 text-forge-success ring-1 ring-inset ring-forge-success/25",
    warn: "bg-forge-warn/12 text-forge-warn ring-1 ring-inset ring-forge-warn/25",
    err: "bg-forge-err/12 text-forge-err ring-1 ring-inset ring-forge-err/25",
    info: "bg-forge-accent/12 text-forge-accent ring-1 ring-inset ring-forge-accent/25",
  }[tone];
  const dotColor = {
    default: "bg-forge-faint",
    success: "bg-forge-success",
    warn: "bg-forge-warn",
    err: "bg-forge-err",
    info: "bg-forge-accent",
  }[tone];
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-[10px] font-semibold uppercase tracking-wide",
        style
      )}
    >
      {dot && <span className={cn("w-1.5 h-1.5 rounded-full", dotColor)} />}
      {children}
    </span>
  );
}
