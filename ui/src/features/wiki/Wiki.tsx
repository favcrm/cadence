import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { fmtBytes } from "../../lib/fmt";
import type { Route } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import Button from "../../ui/Button";
import {
  blobPage,
  wiki,
  WikiError,
  wikiFileUrl,
  wikiRawUrl,
  type WikiEntry,
  type WikiListing,
  type WikiPage,
} from "./api";
import { treeRows } from "./tree";
import { scopedAncestors, withinScope, type WikiScope } from "./scope";
import { baseName, joinPath, parentPath } from "./paths";
import { relTime } from "./history";
import { kindLabel, previewKind } from "./preview";
import {
  WikiToolbar,
  EmptyCard,
  Failure,
  Loading,
  LockIcon,
  Note,
  WIKI_ROOT_LABEL,
  WikiScopeContext,
} from "./shared";
import FileExplorer from "./FileExplorer";
import PagePane from "./PagePane";
import EditorPane from "./EditorPane";
import FolderPane from "./FolderPane";
import UploadPane from "./UploadPane";
import SearchPane from "./SearchPane";
import HistoryPane from "./HistoryPane";
import "./wiki.css";

/**
 * The wiki screen (CAD-581): the folder tree on the left, one pane on the
 * right — a rendered page, a folder, a blob preview, the editor, the
 * upload queue, search, or a page's history. The route decides the pane
 * and the path; every pane navigates by real hrefs, so refresh and
 * back/forward work.
 */

export type WikiRoute = Extract<Route, { screen: "wiki" }>;

export interface WikiProps {
  route: WikiRoute;
  /** The href of any route, scope and drawer carried along (App's `hrefFor`). */
  navHref: (route: Route) => string;
  /** Writes are blocked (read-only board or unsigned) — the reason is shown. */
  readOnly: boolean;
  /** The acting agent, for the "you" marker and write attribution. */
  actor: string;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
  scope?: WikiScope;
  paneFooter?: ReactNode;
}

type Open =
  | { path: string; kind: "dir"; listing: WikiListing }
  | { path: string; kind: "file"; page: WikiPage };

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function Wiki({ route, navHref, readOnly, actor, onToast, scope, paneFooter }: WikiProps) {
  const root = scope?.root ?? "";
  const current = route.path ?? root;
  const validPath = withinScope(current, root);
  const [children, setChildren] = useState<Record<string, WikiEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set([""]));
  const [dirError, setDirError] = useState<Record<string, string>>({});
  const [open, setOpen] = useState<Open | null>(null);
  const [openError, setOpenError] = useState<{ status: number; message: string } | null>(null);
  const [tick, setTick] = useState(0);
  const [missingRoot, setMissingRoot] = useState(false);
  const chooseStartPage = useRef(Boolean(scope && current === root && route.mode === "browse"));

  useEffect(() => {
    if (!chooseStartPage.current || !children[root]) return;
    chooseStartPage.current = false;
    const start = children[root].find((entry) => entry.kind === "page" && entry.name.toLowerCase() === "readme.md");
    if (start) navigate(navHref({ screen: "wiki", mode: "browse", path: start.path, query: null }), { replace: true });
  }, [children, root, navHref]);

  const refresh = useCallback(() => setTick((n) => n + 1), []);
  const fail = useCallback(
    (error: unknown, verb: string) => onToast("err", `${verb} failed — ${message(error)}`),
    [onToast],
  );

  const loadDir = useCallback(async (path: string) => {
    if (!withinScope(path, root)) return;
    try {
      const listing = await wiki.ls(path);
      if (path === root) setMissingRoot(false);
      const entries = listing.kind && listing.kind !== "dir"
        ? []
        : (listing.entries ?? []).filter((entry) => withinScope(entry.path, root));
      setChildren((c) => ({ ...c, [path]: entries }));
      setDirError((e) => {
        if (!(path in e)) return e;
        const next = { ...e };
        delete next[path];
        return next;
      });
    } catch (error) {
      if (scope && path === root && message(error).includes("no such directory")) {
        setMissingRoot(true);
        setChildren((c) => ({ ...c, [root]: [] }));
      } else {
        setDirError((e) => ({ ...e, [path]: message(error) }));
      }
    }
  }, [root, scope]);

  // The route's path is expanded in the tree, and every folder on the way
  // is loaded — a pasted deep link opens with its branch already open.
  useEffect(() => {
    if (!validPath) return;
    const dirs = scopedAncestors(current === root ? root : parentPath(current), root);
    setExpanded((prev) => {
      const next = new Set(prev);
      for (const dir of dirs) next.add(dir);
      return next;
    });
    for (const dir of dirs) void loadDir(dir);
  }, [current, root, validPath, route.mode, loadDir, tick]);

  // The listings as a ref: the browse effect reads the known entry without
  // re-running every time a folder finishes loading (which would loop).
  const childrenRef = useRef(children);
  childrenRef.current = children;
  const entryOf = useCallback((path: string): WikiEntry | null => {
    for (const entries of Object.values(childrenRef.current)) {
      const hit = entries.find((entry) => entry.path === path);
      if (hit) return hit;
    }
    return null;
  }, []);

  // Open the route's path: a folder lists, a page renders, a blob previews.
  // The kind is known when the click came from a listing; a deep link asks
  // the listing route first and falls back to the file route.
  useEffect(() => {
    if (route.mode !== "browse" || !validPath) return;
    const path = current;
    let cancelled = false;
    setOpenError(null);
    setOpen(null);
    const land = (next: Open) => {
      if (cancelled) return;
      setOpen(next);
      if (next.kind === "dir") setChildren((c) => ({ ...c, [path]: next.listing.entries ?? [] }));
    };
    // A blob previews from its listing entry — the file route streams the
    // bytes, so asking it for JSON would be the wrong call. A page's text
    // comes from the file route.
    const openEntry = async (entry: WikiEntry): Promise<Open> =>
      entry.kind === "page"
        ? { path, kind: "file", page: await wiki.file(path) }
        : { path, kind: "file", page: blobPage(entry) };
    const listing = async (): Promise<Open> => {
      let ls: WikiListing;
      try {
        ls = await wiki.ls(path);
      } catch (error) {
        if (scope && path === root && message(error).includes("no such directory")) {
          setMissingRoot(true);
          return { path, kind: "dir", listing: { path, entries: [] } };
        }
        throw error;
      }
      if (ls.kind && ls.kind !== "dir") {
        return openEntry(
          ls.entries?.find((e) => e.path === path) ?? {
            path,
            name: baseName(path),
            kind: ls.kind,
            size: ls.size,
            mime: ls.mime,
            rev: ls.rev,
            edited_by: ls.edited_by,
            mtime: ls.mtime,
          },
        );
      }
      return { path, kind: "dir", listing: { ...ls, path, entries: (ls.entries ?? []).filter((entry) => withinScope(entry.path, root)) } };
    };
    void (async () => {
      try {
        const known = path ? entryOf(path) : null;
        if (path === root || known?.kind === "dir") {
          land(await listing());
        } else if (known) {
          land(await openEntry(known));
        } else {
          try {
            land(await listing());
          } catch (error) {
            if (error instanceof WikiError && error.status === 403) throw error;
            // A pasted blob URL needs its listing metadata; the file
            // endpoint streams bytes and cannot be decoded as JSON.
            const siblings = await wiki.ls(parentPath(path));
            const entry = siblings.entries?.find((item) => item.path === path);
            land(entry && entry.kind !== "dir"
              ? await openEntry(entry)
              : { path, kind: "file", page: await wiki.file(path) });
          }
        }
      } catch (error) {
        if (cancelled) return;
        setOpenError({
          status: error instanceof WikiError ? error.status : 0,
          message: message(error),
        });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [route.mode, current, root, validPath, entryOf, tick, scope]);

  const toggle = (path: string) => {
    if (expanded.has(path)) {
      setExpanded((prev) => {
        const next = new Set(prev);
        next.delete(path);
        return next;
      });
      return;
    }
    setExpanded((prev) => new Set(prev).add(path));
    if (!children[path]) void loadDir(path);
  };

  const rows = useMemo(
    () => treeRows(children, expanded, { root, self: actor }),
    [children, expanded, root, actor],
  );
  const hrefFor = (path: string) =>
    navHref({ screen: "wiki", mode: "browse", path: path || null, query: null });
  const dir = open?.kind === "dir" ? open.path : parentPath(current);

  if (!validPath) return <EmptyCard title="Page outside this project">Choose a page from this project’s file explorer.</EmptyCard>;

  return (
    <WikiScopeContext value={scope}>
    <div className="wk-content">
      <div className="wk-split">
        <FileExplorer
          label={scope ? "Project files" : "Files"}
          rows={rows}
          current={current}
          folder={open?.kind === "file" ? parentPath(current) : undefined}
          emptyText={dirError[root] ? "Files unavailable" : missingRoot || open?.kind === "dir" ? "No pages yet" : "Loading…"}
          hrefFor={hrefFor}
          onToggle={toggle}
        />

        <div className="wk-pane" data-mode={route.mode}>
          {route.mode === "search" ? (
            <SearchPane route={route} navHref={navHref} />
          ) : route.mode === "history" && route.path ? (
            <HistoryPane
              path={route.path}
              navHref={navHref}
              readOnly={readOnly}
              onToast={onToast}
              onChanged={refresh}
            />
          ) : route.mode === "edit" && route.path ? (
            <EditorPane
              key={route.path}
              path={route.path}
              navHref={navHref}
              readOnly={readOnly}
              onToast={onToast}
              onSaved={() => {
                refresh();
                onToast("ok", `saved ${baseName(route.path ?? "")}`);
              }}
            />
          ) : route.mode === "upload" ? (
            <UploadPane
              dir={route.path ?? dir}
              navHref={navHref}
              readOnly={readOnly}
              onToast={onToast}
              onUploaded={refresh}
            />
          ) : (
            <BrowsePane
              scope={scope}
              missingRoot={missingRoot}
              open={open}
              error={openError}
              path={current}
              dir={dir}
              navHref={navHref}
              readOnly={readOnly}
              onToast={onToast}
              onRefresh={refresh}
              onFail={fail}
            />
          )}
          {paneFooter}
        </div>
      </div>
    </div>
    </WikiScopeContext>
  );
}

function BrowsePane({
  scope,
  missingRoot,
  open,
  error,
  path,
  dir,
  navHref,
  readOnly,
  onToast,
  onRefresh,
  onFail,
}: {
  scope?: WikiScope;
  missingRoot: boolean;
  open: Open | null;
  error: { status: number; message: string } | null;
  path: string;
  dir: string;
  navHref: (route: Route) => string;
  readOnly: boolean;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
  onRefresh: () => void;
  onFail: (error: unknown, verb: string) => void;
}) {
  const [creating, setCreating] = useState<{ kind: "page" | "dir"; value: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const createContext = async () => {
    if (!scope || readOnly) return;
    setBusy(true);
    try {
      await wiki.mkdir(scope.root);
      onRefresh();
    } catch (error) {
      onFail(error, "Create project context");
    } finally {
      setBusy(false);
    }
  };

  if (error) {
    if (error.status === 403) {
      return (
        <>
          <Bar path={path} navHref={navHref} />
          <EmptyCard title={error.message.includes("session") ? "Sign in to view these pages" : "Access unavailable"} warn>
            {error.message.includes("session") ? "Use Sign in at the top of the board to open your project pages." : error.message}
            <div className="mt-2">
              <Button href={navHref({ screen: "wiki", mode: "browse", path: null, query: null })}>
                Back to {scope?.label ?? WIKI_ROOT_LABEL}
              </Button>
            </div>
          </EmptyCard>
        </>
      );
    }
    return (
      <>
        <Bar path={path} navHref={navHref} />
        <Failure what="could not open this path" error={error.message} onRetry={onRefresh} />
      </>
    );
  }
  if (!open || open.path !== path) return <Loading what="loading the wiki…" />;

  const write = async () => {
    if (!creating) return;
    const name = creating.value.trim();
    if (!name) return;
    if (name.includes("/") || !withinScope(joinPath(dir, name), scope?.root ?? "")) {
      onToast("err", "Use a page or folder name without path separators.");
      return;
    }
    setBusy(true);
    try {
      if (creating.kind === "dir") {
        await wiki.mkdir(joinPath(dir, name));
        onToast("ok", `created ${name}/`);
      } else {
        const target = joinPath(dir, name.endsWith(".md") ? name : `${name}.md`);
        await wiki.save(target, "", "none");
        onToast("ok", `created ${baseName(target)}`);
        onRefresh();
        navigate(navHref({ screen: "wiki", mode: "edit", path: target, query: null }));
        return;
      }
      setCreating(null);
      onRefresh();
    } catch (e) {
      onFail(e, creating.kind === "dir" ? "new folder" : "new page");
    } finally {
      setBusy(false);
    }
  };

  const newRow = creating && (
    <form
      className="wk-newrow"
      onSubmit={(e) => {
        e.preventDefault();
        void write();
      }}
    >
      <input
        className="field wk-newinput"
        autoFocus
        aria-label={creating.kind === "dir" ? "Folder name" : "Page name"}
        placeholder={creating.kind === "dir" ? "folder name" : "page name"}
        value={creating.value}
        onChange={(e) => setCreating({ ...creating, value: e.target.value })}
      />
      <Button variant="primary" onClick={() => void write()} disabled={busy}>
        create
      </Button>
      <Button onClick={() => setCreating(null)}>cancel</Button>
    </form>
  );

  if (open.kind === "dir") {
    const locked = open.listing.writable === false || open.listing.locked === true;
    const writeBlocked = readOnly || locked || missingRoot;
    return (
      <>
        <WikiToolbar path={path} navHref={navHref} actions={<>
            <Button disabled={writeBlocked} onClick={() => setCreating({ kind: "page", value: "" })}>
              New page
            </Button>
            <Button disabled={writeBlocked} onClick={() => setCreating({ kind: "dir", value: "" })}>
              New folder
            </Button>
            <Button
              href={navHref({ screen: "wiki", mode: "upload", path: path || null, query: null })}
              disabled={writeBlocked}
            >
              Upload
            </Button>
            <Button href={navHref({ screen: "wiki", mode: "search", path: null, query: null })}>Search</Button>
        </>} />
        {missingRoot && scope && (
          <EmptyCard title="No project context yet" action={
            <Button disabled={readOnly || busy} onClick={() => void createContext()}>Create project context</Button>
          }>
            Start with a project brief, research, or decisions.
            {readOnly && " Sign in with editing access to create pages."}
          </EmptyCard>
        )}
        {locked && (
          <Note warn>
            <LockIcon size={13} />
            {path ? `${path}/` : WIKI_ROOT_LABEL} is read-only for you — you can read, not write.
          </Note>
        )}
        {readOnly && !locked && !missingRoot && <Note warn>Sign in with editing access to add or edit pages.</Note>}
        {newRow}
        {!missingRoot && <FolderPane
          entries={open.listing.entries ?? []}
          navHref={navHref}
          readOnly={writeBlocked}
          onRefresh={onRefresh}
          onToast={onToast}
          onFail={onFail}
        />}
      </>
    );
  }

  const page = open.page;
  if (page.kind === "page") return <PagePane page={page} navHref={navHref} readOnly={readOnly} />;
  return (
    <>
      <WikiToolbar path={page.path} navHref={navHref} actions={<>
          <Button href={navHref({ screen: "wiki", mode: "search", path: null, query: null })}>Search</Button>
          <a className="btn" href={wikiFileUrl(page.path)} download>Download</a>
      </>} />
      <BlobPane page={page} blob={previewKind(baseName(page.path), page.mime)} />
    </>
  );
}

function BlobPane({ page, blob }: { page: WikiPage; blob: ReturnType<typeof previewKind> }) {
  const url = wikiFileUrl(page.path);
  const name = baseName(page.path);
  return (
    <>
      <div className="wk-pstage">
        {blob === "image" ? (
          <img className="wk-img" src={url} alt={name} />
        ) : blob === "video" ? (
          <video className="wk-video" src={url} controls preload="metadata">
            <track kind="captions" />
          </video>
        ) : blob === "pdf" ? (
          <iframe className="wk-pdf" src={url} sandbox="" title={name} />
        ) : (
          <div className="wk-download">
            <span className="chip bg-ink-800 text-ink-400">{kindLabel(name, page.mime)}</span>
            <div className="wk-etitle">{name}</div>
            <div className="wk-ebody">
              this type has no inline preview — download it, or open it with the tool it belongs to.
            </div>
            <a className="btn btn-primary" href={wikiRawUrl(page.path)} download>
              Download
            </a>
          </div>
        )}
      </div>
      <div className="wk-pmeta">
        <span className="num">{name}</span>
        {page.size != null && <span>{fmtBytes(page.size)}</span>}
        {page.mime && <span>{page.mime}</span>}
        {page.edited_by && <span>uploaded by {page.edited_by}</span>}
        {page.mtime && <span>{relTime(page.mtime)}</span>}
      </div>
    </>
  );
}

function Bar({ path, navHref }: { path: string; navHref: (route: Route) => string }) {
  return (
    <WikiToolbar path={path} navHref={navHref} />
  );
}
