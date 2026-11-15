import type { ReactNode } from "react";

export type StatusTone = "neutral" | "ok" | "warn" | "fail" | "info" | "accent";

export interface StatusPillProps {
  tone?: StatusTone;
  icon?: ReactNode;
  children: ReactNode;
  className?: string;
  title?: string;
}

/** Compact semantic badge backed by canonical `--color-*` surface tokens. */
export function StatusPill({
  tone = "neutral",
  icon,
  children,
  className,
  title,
}: StatusPillProps) {
  return (
    <span
      className={className ? `s-status-pill ${className}` : "s-status-pill"}
      data-tone={tone}
      title={title}
    >
      {icon ? <span aria-hidden="true">{icon}</span> : null}
      <span>{children}</span>
    </span>
  );
}
