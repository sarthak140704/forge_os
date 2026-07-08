import { cn } from "@/lib/utils";
import type { ButtonHTMLAttributes, ReactNode } from "react";

export function Button({
  children,
  className,
  variant = "primary",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "ghost" | "danger";
  children: ReactNode;
}) {
  const base =
    "px-3 py-1.5 rounded-md text-sm font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed";
  const styles = {
    primary: "bg-forge-accent/90 hover:bg-forge-accent text-white",
    ghost: "text-forge-muted hover:text-forge-fg hover:bg-forge-panel",
    danger: "bg-forge-err/80 hover:bg-forge-err text-white",
  }[variant];
  return (
    <button className={cn(base, styles, className)} {...rest}>
      {children}
    </button>
  );
}

export function Card({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "bg-forge-panel border border-forge-border rounded-lg",
        className
      )}
    >
      {children}
    </div>
  );
}

export function Badge({
  children,
  tone = "default",
}: {
  children: ReactNode;
  tone?: "default" | "success" | "warn" | "err" | "info";
}) {
  const style = {
    default: "bg-forge-border text-forge-muted",
    success: "bg-forge-success/20 text-forge-success",
    warn: "bg-forge-warn/20 text-forge-warn",
    err: "bg-forge-err/20 text-forge-err",
    info: "bg-forge-accent/20 text-forge-accent",
  }[tone];
  return (
    <span
      className={cn(
        "px-2 py-0.5 rounded text-xs font-medium uppercase tracking-wide",
        style
      )}
    >
      {children}
    </span>
  );
}
