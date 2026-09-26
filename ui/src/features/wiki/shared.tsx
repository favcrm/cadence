import type { ReactNode } from "react";
import Link from "../../ui/Link";
import { breadcrumbs } from "./paths";
import type { WikiKind } from "./api";

/** The wiki root's label — the tracker path the store lives in (CAD-580). */
export const WIKI_ROOT_LABEL = "~/pm/wiki";

export function KindIcon({ kind, size = 14 }: { kind: WikiKind; size?: number }) {
  return kind === "dir" ? (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
      <path d="M2 4.5h4l1.2 1.5H14v6.5a1 1 0 01-1 1H3a1 1 0 01-1-1v-8z" />
    </svg>
  ) : (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
      <path d="M3.5 1.8h6l3 3v9.4h-9z" />
      <path d="M5.8 7.5h4.4M5.8 10h4.4" />
    </svg>
  );
}

export function LockIcon({ size = 11 }: { size?: number }) {
  return (
    <svg
      className="wk-lock"
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      aria-label="read-only"
      role="img"
    >
      <rect x="3.2" y="7" width="9.6" height="6.8" rx="1.4" />
      <path d="M5.6 7V5.2a2.4 2.4 0 014.8 0V7" />
    </svg>
  );
}

/** A toolbar button; `href` makes it a link, otherwise it is a button. */
export function Btn({
  children,
  onClick,
  href,
  accent,
  danger,
  disabled,
  title,
}: {
  children: ReactNode;
  onClick?: () => void;
  href?: string;
  accent?: boolean;
  danger?: boolean;
  disabled?: boolean;
  title?: string;
}) {
  const className = `wk-btn${accent ? " wk-btn-acc" : ""}${danger ? " wk-btn-danger" : ""}`;
  if (href && !disabled) {
    return (
      <Link href={href} className={className} title={title} onClick={onClick}>
        {children}
      </Link>
    );
  }
  return (
    <button className={className} onClick={onClick} disabled={disabled} title={title} type="button">
      {children}
    </button>
  );
}

/** Breadcrumbs for a path; every ancestor is a link, the leaf is bold. */
export function Crumbs({ path, hrefFor }: { path: string; hrefFor: (path: string) => string }) {
  const trail = breadcrumbs(path);
  return (
    <div className="wk-crumbs">
      <Link href={hrefFor("")}>{WIKI_ROOT_LABEL}</Link>
      {trail.map((crumb, i) => (
        <span key={crumb.path} className="wk-crumb">
          <span className="wk-sep">/</span>
          {i === trail.length - 1 ? (
            <b>{crumb.name}</b>
          ) : (
            <Link href={hrefFor(crumb.path)}>{crumb.name}</Link>
          )}
        </span>
      ))}
    </div>
  );
}

/** The warn note a read-only folder or an operator-only path shows. */
export function Note({ children, warn }: { children: ReactNode; warn?: boolean }) {
  return (
    <div className={`wk-note${warn ? " warn" : ""}`} role="note">
      {children}
    </div>
  );
}

export function EmptyCard({
  title,
  children,
  action,
  warn,
}: {
  title: string;
  children?: ReactNode;
  action?: ReactNode;
  warn?: boolean;
}) {
  return (
    <div className="wk-empty card">
      {warn ? (
        <svg width="34" height="34" viewBox="0 0 16 16" fill="none" stroke="var(--color-warn)" strokeWidth="1.3">
          <rect x="3.2" y="7" width="9.6" height="6.8" rx="1.4" />
          <path d="M5.6 7V5.2a2.4 2.4 0 014.8 0V7" />
        </svg>
      ) : (
        <svg width="34" height="34" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.1">
          <path d="M2 4.5h4l1.2 1.5H14v6.5a1 1 0 01-1 1H3a1 1 0 01-1-1v-8z" />
        </svg>
      )}
      <div className="wk-etitle">{title}</div>
      {children && <div className="wk-ebody">{children}</div>}
      {action}
    </div>
  );
}

export function Loading({ what }: { what: string }) {
  return (
    <div className="wk-loading" role="status">
      {what}
    </div>
  );
}

export function Failure({ what, error, onRetry }: { what: string; error: string; onRetry?: () => void }) {
  return (
    <div className="wk-failure" role="alert">
      <span className="min-w-0">
        {what} — {error}
      </span>
      {onRetry && (
        <button type="button" className="chip bg-fail/10 text-fail hover:bg-fail/20 ml-auto" onClick={onRetry}>
          retry
        </button>
      )}
    </div>
  );
}
