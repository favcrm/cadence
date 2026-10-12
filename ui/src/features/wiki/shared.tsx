import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import type { WikiScope } from "./scope";
import type { Route } from "../../lib/router";
import Link from "../../ui/Link";
import { IconFolder, IconLock, IconWiki } from "../../ui/icons";
import { breadcrumbs } from "./paths";
import { wikiBlob, type WikiKind } from "./api";
import { sessionKey } from "../../lib/sessionKey";

export const WIKI_ROOT_LABEL = "Wiki";
export const WikiScopeContext = createContext<WikiScope | undefined>(undefined);
export const useWikiScope = () => useContext(WikiScopeContext);

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

/** Breadcrumbs for a path; every ancestor is a link, the leaf is bold. */
export function Crumbs({ path, hrefFor }: { path: string; hrefFor: (path: string) => string }) {
  const scope = useWikiScope();
  const root = scope?.root ?? "";
  const trail = breadcrumbs(path).filter((crumb) => crumb.path !== root && (!root || crumb.path.startsWith(`${root}/`)));
  return (
    <div className="wk-crumbs">
      <Link href={hrefFor(root)}>{scope?.label ?? WIKI_ROOT_LABEL}</Link>
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

/** One breadcrumb/action layout for every Wiki and Context mode. */
export function WikiToolbar({ path, navHref, label, children, actions }: {
  path: string;
  navHref: (route: Route) => string;
  label?: string;
  children?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div className="wk-bar">
      <Crumbs path={path} hrefFor={(p) => navHref({ screen: "wiki", mode: "browse", path: p || null, query: null })} />
      {label && <span className="wk-editing">{label}</span>}
      {children}
      {actions && <div className="wk-tools">{actions}</div>}
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

/**
 * An image from the authenticated file route. The fetch carries the session
 * header; the bytes become a data URL (the CSP admits `data:` images, not
 * `blob:`). The read is aborted and the encoded bytes dropped when its path or
 * session key changes or the image unmounts. The loaded source is tagged with
 * the path and session key it was fetched for, and renders only while both
 * still match the current props and tab session, so a re-render with a new
 * path or key never shows the previous bytes.
 */
export function WikiImage({ path, className, alt }: { path: string; className?: string; alt: string }) {
  const key = sessionKey();
  const [loaded, setLoaded] = useState<{ path: string; key: string | null; src: string } | null>(null);
  useEffect(() => {
    const abort = new AbortController();
    let reader: FileReader | null = null;
    wikiBlob(path, abort.signal)
      .then((blob) => {
        if (abort.signal.aborted || sessionKey() !== key) return;
        const owned = new FileReader();
        reader = owned;
        owned.onload = () => {
          const src = owned.result;
          if (abort.signal.aborted || sessionKey() !== key || typeof src !== "string") return;
          setLoaded({ path, key, src });
        };
        owned.readAsDataURL(blob);
      })
      .catch(() => {});
    return () => {
      abort.abort();
      if (reader?.readyState === FileReader.LOADING) reader.abort();
      setLoaded(null);
    };
  }, [path, key]);
  const src = loaded && loaded.path === path && loaded.key === sessionKey() ? loaded.src : null;
  return src ? <img className={className} src={src} alt={alt} /> : null;
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
