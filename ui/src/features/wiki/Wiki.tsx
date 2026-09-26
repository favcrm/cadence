import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { fmtBytes } from "../../lib/fmt";
import type { Route } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Md from "../../ui/Md";
import { IconCaret } from "../../ui/icons";
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
import { ancestors, treeRows } from "./tree";
import { baseName, joinPath, parentPath } from "./paths";
import { relTime } from "./history";
import { kindLabel, previewKind } from "./preview";
import {
  Crumbs,
  EmptyCard,
  Failure,
  KindIcon,
  Loading,
  LockIcon,
  Note,
  WIKI_ROOT_LABEL,
} from "./shared";
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
}

type Open =
  | { path: string; kind: "dir"; listing: WikiListing }
  | { path: string; kind: "file"; page: WikiPage };

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function Wiki({ route, navHref, readOnly, actor, onToast }: WikiProps) {
  const [children, setChildren] = useState<Record<string, WikiEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set([""]));
  const [dirError, setDirError] = useState<Record<string, string>>({});
  const [open, setOpen] = useState<Open | null>(null);
  const [openError, setOpenError] = useState<{ status: number; message: string } | null>(null);
  const [tick, setTick] = useState(0);

  const refresh = useCallback(() => setTick((n) => n + 1), []);
  const fail = useCallback(
    (error: unknown, verb: string) => onToast("err", `${verb} failed — ${message(error)}`),
    [onToast],
  );

  const loadDir = useCallback(async (path: string) => {
    try {
      const listing = await wiki.ls(path);
      const entries = listing.kind && listing.kind !== "dir" ? [] : (listing.entries ?? []);
      setChildren((c) => ({ ...c, [path]: entries }));
      setDirError((e) => {
        if (!(path in e)) return e;
        const next = { ...e };
        delete next[path];
        return next;
      });
    } catch (error) {
      setDirError((e) => ({ ...e, [path]: message(error) }));
    }
  }, []);

  // The route's path is expanded in the tree, and every folder on the way
  // is loaded — a pasted deep link opens with its branch already open.
  useEffect(() => {
    const dirs = ["", ...ancestors(parentPath(route.path ?? ""))];
    setExpanded((prev) => {
      const next = new Set(prev);
      for (const dir of dirs) next.add(dir);
      return next;
    });
    for (const dir of dirs) void loadDir(dir);
  }, [route.path, route.mode, loadDir, tick]);

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
    if (route.mode !== "browse") return;
    const path = route.path ?? "";
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
      const ls = await wiki.ls(path);
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
      return { path, kind: "dir", listing: { ...ls, path, entries: ls.entries ?? [] } };
    };
    void (async () => {
      try {
        const known = path ? entryOf(path) : null;
        if (path === "" || known?.kind === "dir") {
          land(await listing());
        } else if (known) {
          land(await openEntry(known));
        } else {
          try {
            land(await listing());
          } catch (error) {
            if (error instanceof WikiError && error.status === 403) throw error;
            land({ path, kind: "file", page: await wiki.file(path) });
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
  }, [route.mode, route.path, entryOf, tick]);

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
    () => treeRows(children, expanded, { self: actor }),
    [children, expanded, actor],
  );
  const current = route.path ?? "";
  const hrefFor = (path: string) =>
    navHref({ screen: "wiki", mode: "browse", path: path || null, query: null });
  const dir = open?.kind === "dir" ? open.path : parentPath(current);

  return (
    <div className="wk-content">
      <div className="wk-split">
        <div className="wk-tree">
          <div className="slabel wk-treeroot">{WIKI_ROOT_LABEL}</div>
          {rows.length === 0 && (
            <div className="wk-treemeta">
              {dirError[""] ? (
                <Failure what="could not load the tree" error={dirError[""]} onRetry={refresh} />
              ) : (
                <span className="kicker">loading…</span>
              )}
            </div>
          )}
          {rows.map((row) => {
            // The open path, or the folder holding the open file.
            const on = row.path === current || (open?.kind === "file" && row.path === parentPath(current));
            return (
              <div
                key={row.path}
                className={`wk-trow${row.dir ? " dir" : ""}${row.expanded ? " open" : ""}${on ? " on" : ""}`}
                style={{ paddingLeft: 8 + row.depth * 16 }}
              >
                {row.dir ? (
                  <button
                    type="button"
                    className="wk-caret"
                    aria-label={row.expanded ? `collapse ${row.name}` : `expand ${row.name}`}
                    aria-expanded={row.expanded}
                    onClick={() => toggle(row.path)}
                  >
                    <IconCaret />
                  </button>
                ) : (
                  <span className="wk-caret-space" />
                )}
                <span className="wk-tkind">
                  <KindIcon kind={row.kind} />
                </span>
                <Link className="wk-tname" href={hrefFor(row.path)}>
                  {row.name}
                  {row.dir ? "/" : ""}
                </Link>
                {row.own && <span className="kicker">you</span>}
                {row.locked && <LockIcon />}
                <span className="wk-tmeta num">
                  {row.size != null ? fmtBytes(row.size) : row.childCount != null ? row.childCount : ""}
                </span>
              </div>
            );
          })}
          {dirError[current] && <Failure what="could not load this folder" error={dirError[current]} onRetry={refresh} />}
        </div>

        <div className="wk-pane">
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
        </div>
      </div>
    </div>
  );
}

function BrowsePane({
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

  if (error) {
    if (error.status === 403) {
      return (
        <>
          <Bar path={path} navHref={navHref} />
          <EmptyCard title="Permission denied" warn>
            <span className="num">{path}</span> is private to its owner. Agents see their own
            folder, their project folders, and everything read-only.
            <div className="mt-2">
              <Button href={navHref({ screen: "wiki", mode: "browse", path: null, query: null })}>
                Back to {WIKI_ROOT_LABEL}
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
    setBusy(true);
    try {
      if (creating.kind === "dir") {
        await wiki.mkdir(joinPath(dir, name));
        onToast("ok", `created ${name}/`);
      } else {
        const target = joinPath(dir, name.endsWith(".md") ? name : `${name}.md`);
        await wiki.save(target, "", "");
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
    const writeBlocked = readOnly || locked;
    return (
      <>
        <div className="wk-bar">
          <Crumbs path={path} hrefFor={(p) => navHref({ screen: "wiki", mode: "browse", path: p || null, query: null })} />
          <div className="wk-tools">
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
          </div>
        </div>
        {locked && (
          <Note warn>
            <LockIcon size={13} />
            {path ? `${path}/` : WIKI_ROOT_LABEL} is read-only for you — you can read, not write.
          </Note>
        )}
        {readOnly && !locked && <Note warn>writes are disabled on this board — sign in as the operator to edit.</Note>}
        {newRow}
        <FolderPane
          dir={path}
          entries={open.listing.entries ?? []}
          navHref={navHref}
          readOnly={writeBlocked}
          onRefresh={onRefresh}
          onToast={onToast}
          onFail={onFail}
        />
      </>
    );
  }

  const page = open.page;
  const name = baseName(page.path);
  const blob = previewKind(name, page.mime);
  const isPage = page.kind === "page" || blob === "download";
  return (
    <>
      <div className="wk-bar">
        <Crumbs path={page.path} hrefFor={(p) => navHref({ screen: "wiki", mode: "browse", path: p || null, query: null })} />
        <div className="wk-tools">
          <Button href={navHref({ screen: "wiki", mode: "search", path: null, query: null })}>Search</Button>
        </div>
      </div>
      {!isPage && (
        <div className="wk-bar wk-bar-tight">
          <div className="wk-tools wk-tools-left">
            <Button href={navHref({ screen: "wiki", mode: "browse", path: dir || null, query: null })}>Back</Button>
          </div>
          <div className="wk-tools">
            <a className="btn" href={wikiFileUrl(page.path)} download>
              Download
            </a>
          </div>
        </div>
      )}
      {isPage ? (
        <>
          <div className="wk-pagehead">
            <h1>{name}</h1>
            <span className="wk-meta">
              {page.edited_by ? (
                <>
                  last edited by <b>{page.edited_by}</b> ·{" "}
                </>
              ) : null}
              {relTime(page.mtime) || "no history yet"}
              {page.rev ? (
                <>
                  {" "}
                  · rev <span className="num">{page.rev}</span>
                </>
              ) : null}
            </span>
            <div className="wk-tools">
              <Button href={navHref({ screen: "wiki", mode: "history", path: page.path, query: null })}>History</Button>
              <Button
                variant="primary"
                disabled={readOnly}
                title={readOnly ? "writes are disabled — sign in as the operator" : undefined}
                href={readOnly ? undefined : navHref({ screen: "wiki", mode: "edit", path: page.path, query: null })}
              >
                Edit
              </Button>
            </div>
          </div>
          <div className="md card pad wk-md issue-reader">
            <Md text={page.text ?? ""} />
          </div>
        </>
      ) : (
        <BlobPane page={page} blob={blob} />
      )}
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
    <div className="wk-bar">
      <Crumbs path={path} hrefFor={(p) => navHref({ screen: "wiki", mode: "browse", path: p || null, query: null })} />
    </div>
  );
}
