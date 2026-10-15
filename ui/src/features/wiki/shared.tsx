import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import type { WikiScope } from "./scope";
import type { Route } from "../../lib/router";
import Link from "../../ui/Link";
import { IconFolder, IconLock, IconWiki } from "../../ui/icons";
import { breadcrumbs } from "./paths";
import { wikiBlob, type WikiKind } from "./api";
import { sessionKey } from "../../lib/sessionKey";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import type { PreviewKind } from "./preview";

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
  const t = useTranslate();
  const scope = useWikiScope();
  const root = scope?.root ?? "";
  const trail = breadcrumbs(path).filter((crumb) => crumb.path !== root && (!root || crumb.path.startsWith(`${root}/`)));
  return (
    <div className="wk-crumbs">
      <Link href={hrefFor(root)}>{scope?.label ?? t(WIKI_ROOT_LABEL)}</Link>
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
 * Credentialed wiki bytes never go straight into an element's URL. Raster
 * images use data: (the image CSP excludes blob:); PDF and allowlisted video
 * use local blob: previews. Downloads use an inert octet-stream blob, even
 * for uploaded HTML/SVG. The board CSP must explicitly admit blob: frames
 * and media; PDF callers must keep their iframe sandboxed.
 *
 * Abort fetch/encoding and revoke every owned URL on path/session changes,
 * paused private reads, or unmount. Fence both decoding and rendering with
 * the identity generation, including a reset that reuses the same tab key.
 */
export function useWikiBlobSource(path: string, kind: PreviewKind, withDownload = false) {
  const key = sessionKey();
  const generation = privateSessionGeneration();
  const [loaded, setLoaded] = useState<{
    path: string; key: string | null; generation: number; kind: PreviewKind; withDownload: boolean;
    src: string | null; download: string | null; error: string | null;
  } | null>(null);
  useEffect(() => {
    const abort = new AbortController();
    let reader: FileReader | null = null;
    const urls: string[] = [];
    const current = () => !abort.signal.aborted && privateReadsAllowed()
      && sessionKey() === key && privateSessionGeneration() === generation;
    const dispose = () => {
      abort.abort();
      if (reader && reader.readyState === FileReader.LOADING) reader.abort();
      for (const url of urls.splice(0)) URL.revokeObjectURL(url);
      setLoaded(null);
    };
    const stop = onPrivateReadsPaused(dispose);
    const localUrl = (blob: Blob) => {
      const url = URL.createObjectURL(blob);
      urls.push(url);
      return url;
    };
    setLoaded(null);
    wikiBlob(path, abort.signal)
      .then((blob) => {
        if (!current()) return;
        const download = withDownload ? localUrl(blob.slice(0, blob.size, "application/octet-stream")) : null;
        const publish = (src: string | null) => {
          if (current()) setLoaded({ path, key, generation, kind, withDownload, src, download, error: null });
        };
        if (kind === "image") {
          const owned = new FileReader();
          reader = owned;
          owned.onload = () => {
            if (typeof owned.result === "string") publish(owned.result);
          };
          owned.onerror = () => {
            if (current()) setLoaded({ path, key, generation, kind, withDownload, src: null, download, error: "Could not decode this image" });
          };
          owned.readAsDataURL(blob);
        } else {
          const mime = blob.type.split(";")[0].trim().toLowerCase();
          const video = ["video/mp4", "video/webm", "video/ogg", "video/quicktime", "video/x-m4v"].includes(mime);
          const preview = (kind === "pdf" && mime === "application/pdf") || (kind === "video" && video);
          publish(preview ? localUrl(blob.slice(0, blob.size, mime)) : null);
        }
      })
      .catch((error: unknown) => {
        if (current()) setLoaded({ path, key, generation, kind, withDownload, src: null, download: null, error: error instanceof Error ? error.message : String(error) });
      });
    return () => {
      stop();
      dispose();
    };
  }, [path, kind, withDownload, key, generation]);
  const result = loaded && loaded.path === path && loaded.key === key
    && loaded.generation === generation && loaded.kind === kind && loaded.withDownload === withDownload
    && privateReadsAllowed() ? loaded : null;
  return { src: result?.src ?? null, download: result?.download ?? null, error: result?.error ?? null, loading: result === null };
}

/** Markdown and file previews share the same fenced, CSP-compatible image path. */
export function WikiImage({ path, className, alt }: { path: string; className?: string; alt: string }) {
  const { src } = useWikiBlobSource(path, "image");
  return src ? <img className={className} src={src} alt={alt} /> : null;
}

export function Loading({ what }: { what: string }) {
  const t = useTranslate();
  return (
    <div className="wk-loading" role="status">
      {t(what)}
    </div>
  );
}

export function Failure({ what, error, onRetry }: { what: string; error: string; onRetry?: () => void }) {
  const t = useTranslate();
  return (
    <div className="wk-failure" role="alert">
      <span className="min-w-0">
        {t(what)} — {t(error)}
      </span>
      {onRetry && (
        <button type="button" className="chip bg-fail/10 text-fail hover:bg-fail/20 ml-auto" onClick={onRetry}>
          retry
        </button>
      )}
    </div>
  );
}
