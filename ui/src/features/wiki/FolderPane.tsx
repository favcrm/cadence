import { useState } from "react";
import type { Route } from "../../lib/router";
import { fmtBytes } from "../../lib/fmt";
import { wiki, wikiFileUrl, wikiRawUrl, type WikiEntry } from "./api";
import { baseName, joinPath, parentPath } from "./paths";
import { kindLabel, previewKind } from "./preview";
import { entryOrder } from "./tree";
import { Btn, KindIcon, Loading } from "./shared";

/**
 * The folder view (CAD-581): grid or list, image thumbnails, a per-item
 * menu (rename, move, download, delete-to-trash) and the empty state.
 */

export default function FolderPane({
  dir,
  entries,
  navHref,
  readOnly,
  onRefresh,
  onToast,
  onFail,
}: {
  dir: string;
  entries: WikiEntry[];
  navHref: (route: Route) => string;
  readOnly: boolean;
  onRefresh: () => void;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
  onFail: (error: unknown, verb: string) => void;
}) {
  const [view, setView] = useState<"grid" | "list">("grid");
  const [menu, setMenu] = useState<string | null>(null);
  const [dialog, setDialog] = useState<{ kind: "rename" | "move"; entry: WikiEntry; value: string } | null>(null);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const rows = [...entries].sort(entryOrder);
  const openHref = (entry: WikiEntry) =>
    navHref({ screen: "wiki", mode: "browse", path: entry.path, query: null });

  const submit = async () => {
    if (!dialog) return;
    const value = dialog.value.trim();
    if (!value) return;
    setBusy(true);
    try {
      const to = dialog.kind === "rename" ? joinPath(parentPath(dialog.entry.path), value) : value;
      await wiki.mv(dialog.entry.path, to);
      onToast("ok", `${dialog.kind === "rename" ? "renamed" : "moved"} ${baseName(dialog.entry.path)}`);
      setDialog(null);
      onRefresh();
    } catch (error) {
      onFail(error, dialog.kind === "rename" ? "rename" : "move");
    } finally {
      setBusy(false);
    }
  };

  const remove = async (entry: WikiEntry) => {
    setBusy(true);
    try {
      await wiki.rm(entry.path);
      onToast("ok", `${entry.name} moved to trash`);
      setConfirm(null);
      setMenu(null);
      onRefresh();
    } catch (error) {
      onFail(error, "delete");
    } finally {
      setBusy(false);
    }
  };

  if (rows.length === 0) {
    return (
      <div className="wk-empty card">
        <svg width="34" height="34" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.1">
          <path d="M2 4.5h4l1.2 1.5H14v6.5a1 1 0 01-1 1H3a1 1 0 01-1-1v-8z" />
        </svg>
        <div className="wk-etitle">{dir ? `${dir}/ is empty` : `${"~/pm/wiki"} is empty`}</div>
        <div className="wk-ebody">
          {readOnly ? "nothing here yet — and writes are disabled for this board." : "start with New page, or drop files to upload."}
        </div>
      </div>
    );
  }

  const itemMenu = (entry: WikiEntry) => (
    <>
      <button
        type="button"
        className={`wk-fmenu-btn${menu === entry.path ? " on" : ""}`}
        aria-label={`${entry.name} actions`}
        onClick={() => setMenu(menu === entry.path ? null : entry.path)}
      >
        ⋯
      </button>
      {menu === entry.path && (
        <>
          <div className="wk-menu-backdrop" onClick={() => setMenu(null)} />
          <div className="wk-fmenu">
            {entry.kind === "page" && (
              <>
                <button type="button" onClick={() => { setMenu(null); setDialog({ kind: "rename", entry, value: entry.name }); }} disabled={readOnly}>
                  Rename
                </button>
                <button type="button" onClick={() => { setMenu(null); setDialog({ kind: "move", entry, value: entry.path }); }} disabled={readOnly}>
                  Move
                </button>
              </>
            )}
            <a href={wikiRawUrl(entry.path)} download onClick={() => setMenu(null)}>
              Download
            </a>
            <button
              type="button"
              className="danger"
              disabled={readOnly}
              onClick={() => { setMenu(null); setConfirm(entry.path); }}
            >
              Delete
            </button>
          </div>
        </>
      )}
    </>
  );

  return (
    <>
      <div className="wk-viewrow">
        <span className="wk-vt" role="group" aria-label="view">
          <button
            type="button"
            className={view === "grid" ? "on" : ""}
            title="grid"
            aria-pressed={view === "grid"}
            onClick={() => setView("grid")}
          >
            <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
              <rect x="2" y="2" width="5" height="5" rx="1" />
              <rect x="9" y="2" width="5" height="5" rx="1" />
              <rect x="2" y="9" width="5" height="5" rx="1" />
              <rect x="9" y="9" width="5" height="5" rx="1" />
            </svg>
          </button>
          <button
            type="button"
            className={view === "list" ? "on" : ""}
            title="list"
            aria-pressed={view === "list"}
            onClick={() => setView("list")}
          >
            <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
              <path d="M2.5 4h11M2.5 8h11M2.5 12h11" />
            </svg>
          </button>
        </span>
      </div>

      {dialog && (
        <form
          className="wk-dialog card"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <span className="slabel">{dialog.kind === "rename" ? "rename" : "move to"}</span>
          <input
            className="field"
            autoFocus
            value={dialog.value}
            onChange={(e) => setDialog({ ...dialog, value: e.target.value })}
          />
          <div className="wk-tools">
            <Btn accent onClick={() => void submit()} disabled={busy}>
              {dialog.kind}
            </Btn>
            <Btn onClick={() => setDialog(null)}>cancel</Btn>
          </div>
        </form>
      )}

      {confirm && (
        <div className="wk-dialog card">
          <span className="slabel">delete</span>
          <div className="text-secondary text-ink-300">
            <span className="num">{confirm}</span> moves to <span className="num">.trash/</span> — it can be
            restored from there.
          </div>
          <div className="wk-tools">
            <Btn
              danger
              disabled={busy}
              onClick={() => {
                const entry = rows.find((r) => r.path === confirm);
                if (entry) void remove(entry);
              }}
            >
              delete to trash
            </Btn>
            <Btn onClick={() => setConfirm(null)}>cancel</Btn>
          </div>
        </div>
      )}

      {busy && !dialog && !confirm && <Loading what="working…" />}

      {view === "grid" ? (
        <div className="wk-fgrid">
          {rows.map((entry) => {
            const blob = entry.kind === "dir" ? null : previewKind(entry.name, entry.mime);
            return (
              <div key={entry.path} className="wk-ftile">
                <a className="wk-fthumb" href={openHref(entry)} title={entry.name}>
                  {entry.kind === "dir" ? (
                    <KindIcon kind="dir" size={22} />
                  ) : blob === "image" ? (
                    <img src={wikiFileUrl(entry.path)} alt="" loading="lazy" />
                  ) : (
                    <>
                      <KindIcon kind="page" size={22} />
                      <span className="chip wk-fkind">{kindLabel(entry.name, entry.mime)}</span>
                    </>
                  )}
                  {entry.locked && <span className="wk-flock">locked</span>}
                </a>
                {itemMenu(entry)}
                <div className="wk-fmeta">
                  <a className="wk-fname" href={openHref(entry)}>
                    {entry.name}
                  </a>
                  <span className="wk-fsize num">{entry.size != null ? fmtBytes(entry.size) : entry.entries ?? ""}</span>
                </div>
              </div>
            );
          })}
        </div>
      ) : (
        <div className="wk-flist">
          {rows.map((entry) => (
            <div key={entry.path} className="wk-frow">
              <span className="wk-ficon">
                <KindIcon kind={entry.kind} />
              </span>
              <a className="wk-fname" href={openHref(entry)}>
                {entry.name}
              </a>
              <span className="wk-fsub">
                {entry.kind === "dir" ? "folder" : kindLabel(entry.name, entry.mime).toLowerCase()}
              </span>
              {entry.locked && <span className="wk-flock">locked</span>}
              <span className="wk-fsize num">{entry.size != null ? fmtBytes(entry.size) : entry.entries ?? ""}</span>
              {itemMenu(entry)}
            </div>
          ))}
        </div>
      )}
    </>
  );
}
