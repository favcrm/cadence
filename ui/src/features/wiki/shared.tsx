import type { ReactNode } from "react";
import Link from "../../ui/Link";
import { IconFolder, IconLock, IconWiki } from "../../ui/icons";
import { breadcrumbs } from "./paths";
import type { WikiKind } from "./api";

/** The wiki root's label — the tracker path the store lives in (CAD-580). */
export const WIKI_ROOT_LABEL = "~/pm/wiki";

export function KindIcon({ kind, size = 14 }: { kind: WikiKind; size?: number }) {
  return kind === "dir" ? <IconFolder size={size} /> : <IconWiki size={size} />;
}

export function LockIcon({ size = 11 }: { size?: number }) {
  return (
    <span className="wk-lock" role="img" aria-label="read-only">
      <IconLock size={size} />
    </span>
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
        <IconLock size={34} style={{ color: "var(--color-warn)" }} />
      ) : (
        <IconFolder size={34} />
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
