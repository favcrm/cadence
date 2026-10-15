import { useEffect, useId, useRef, useState } from "react";
import type { Route } from "../../lib/router";
import Link from "../../ui/Link";
import { IconFolder, IconGrid, IconList, IconMore } from "../../ui/icons";
import { wiki, wikiBlob, type WikiEntry } from "./api";
import { baseName, joinPath, parentPath } from "./paths";
import { withinScope } from "./scope";
import { kindLabel, previewKind } from "./preview";
import { entryOrder } from "./tree";
import Button from "../../ui/Button";
import { KindIcon, Loading, WikiImage, useWikiScope } from "./shared";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { sessionKey } from "../../lib/sessionKey";

/**
 * The folder view (CAD-581): grid or list, image thumbnails, a per-item
 * menu (rename, move, download, delete-to-trash) and the empty state.
 */

export default function FolderPane({
  entries,
  navHref,
  readOnly,
  onRefresh,
  onToast,
  onFail,
}: {
  entries: WikiEntry[];
  navHref: (route: Route) => string;
  readOnly: boolean;
  onRefresh: () => void;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
  onFail: (error: unknown, verb: string) => void;
}) {
  const t = useTranslate();
  const scope = useWikiScope();
  const [view, setView] = useState<"grid" | "list">("list");
  const [menu, setMenu] = useState<string | null>(null);
  const [dialog, setDialog] = useState<{ kind: "rename" | "move"; entry: WikiEntry; value: string } | null>(null);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fieldId = useId();
  const menuId = useId();
  const menuTrigger = useRef<HTMLButtonElement | null>(null);
  const downloads = useRef(new Set<AbortController>());
  const downloadUrls = useRef(new Map<string, number>());

  useEffect(() => {
    const dispose = () => {
      for (const controller of downloads.current) controller.abort();
      downloads.current.clear();
      for (const [url, timer] of downloadUrls.current) {
        window.clearTimeout(timer);
        URL.revokeObjectURL(url);
      }
      downloadUrls.current.clear();
    };
    const stop = onPrivateReadsPaused(dispose);
    return () => { stop(); dispose(); };
  }, [entries]);

  useEffect(() => {
    if (!menu) return;
    const dismiss = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setMenu(null);
      menuTrigger.current?.focus();
    };
    window.addEventListener("keydown", dismiss);
    return () => window.removeEventListener("keydown", dismiss);
  }, [menu]);

  const rows = [...entries].sort(entryOrder);
  const openHref = (entry: WikiEntry) =>
    navHref({ screen: "wiki", mode: "browse", path: entry.path, query: null });
  const closeDialog = () => {
    setDialog(null);
    menuTrigger.current?.focus();
  };

  const submit = async () => {
    if (!dialog || busy || readOnly) return;
    const value = dialog.value.trim();
    if (!value) return;
    setBusy(true);
    try {
      if (dialog.kind === "rename" && value.includes("/")) throw new Error("Use a file name without folders. Use Move to change its location.");
      const to = dialog.kind === "rename" ? joinPath(parentPath(dialog.entry.path), value) : value;
      if (scope && !withinScope(to, scope.root)) throw new Error("Choose a destination inside this project’s context");
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

  const downloadEntry = async (entry: WikiEntry) => {
    const generation = privateSessionGeneration();
    const key = sessionKey();
    const controller = new AbortController();
    downloads.current.add(controller);
    try {
      const blob = entry.kind === "page"
        ? new Blob([(await wiki.file(entry.path)).text ?? ""], { type: "text/markdown;charset=utf-8" })
        : await wikiBlob(entry.path, controller.signal);
      if (controller.signal.aborted || !privateReadsAllowed() || generation !== privateSessionGeneration() || key !== sessionKey()) return;
      const url = URL.createObjectURL(blob.slice(0, blob.size, "application/octet-stream"));
      const link = document.createElement("a");
      link.href = url;
      link.download = entry.name;
      document.body.appendChild(link);
      link.click();
      link.remove();
      const timer = window.setTimeout(() => {
        URL.revokeObjectURL(url);
        downloadUrls.current.delete(url);
      }, 1000);
      downloadUrls.current.set(url, timer);
      setMenu(null);
    } catch (error) {
      if (!controller.signal.aborted && privateReadsAllowed() && generation === privateSessionGeneration() && key === sessionKey()) onFail(error, "Download file");
    } finally {
      downloads.current.delete(controller);
    }
  };

  if (rows.length === 0) {
    return (
      <div className="wk-empty card">
        <IconFolder size={34} />
        <div className="wk-etitle">{t("This folder is empty")}</div>
        <div className="wk-ebody">
          {readOnly ? t("nothing here yet — and writes are disabled for this board.") : t("start with New page, or drop files to upload.")}
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
        aria-expanded={menu === entry.path}
        aria-controls={`${menuId}-${encodeURIComponent(entry.path)}`}
        onClick={(event) => {
          menuTrigger.current = event.currentTarget;
          setMenu(menu === entry.path ? null : entry.path);
        }}
      >
        <IconMore />
      </button>
      {menu === entry.path && (
        <>
          <div className="wk-menu-backdrop" onClick={() => setMenu(null)} />
          <div id={`${menuId}-${encodeURIComponent(entry.path)}`} className="wk-fmenu" role="group" aria-label={`${entry.name} actions`}>
            {entry.kind === "page" && (
              <>
                <button type="button" onClick={() => { setMenu(null); setDialog({ kind: "rename", entry, value: entry.name }); }} disabled={readOnly}>
                  {t("Rename")}
                </button>
                <button type="button" onClick={() => { setMenu(null); setDialog({ kind: "move", entry, value: entry.path }); }} disabled={readOnly}>
                  {t("Move")}
                </button>
              </>
            )}
            {(entry.kind === "page" || entry.kind === "file") && <button type="button" onClick={() => void downloadEntry(entry)}>{t("Download")}</button>}
            <button
              type="button"
              className="danger"
              disabled={readOnly}
              onClick={() => { setMenu(null); setConfirm(entry.path); }}
            >
              {t("Delete")}
            </button>
          </div>
        </>
      )}
    </>
  );

  return (
    <>
      <div className="wk-viewrow">
        <span className="text-label text-ink-500">{rows.length} {rows.length === 1 ? "item" : "items"}</span>
        <span className="wk-vt" role="group" aria-label={t("Folder view")}>
          <button
            type="button"
            className={view === "grid" ? "on" : ""}
            title={t("Grid view")}
            aria-label={t("Grid view")}
            aria-pressed={view === "grid"}
            onClick={() => setView("grid")}
          >
            <IconGrid />
          </button>
          <button
            type="button"
            className={view === "list" ? "on" : ""}
            title={t("List view")}
            aria-label={t("List view")}
            aria-pressed={view === "list"}
            onClick={() => setView("list")}
          >
            <IconList />
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
          onKeyDown={(event) => {
            if (event.key === "Escape" && !busy) closeDialog();
          }}
        >
          <h2 className="wk-dialog-title">{dialog.kind === "rename" ? t("Rename") : t("Move")} {dialog.entry.name}</h2>
          <label className="slabel" htmlFor={fieldId}>{dialog.kind === "rename" ? t("File name") : t("Destination path")}</label>
          {dialog.kind === "move" && <p id={`${fieldId}-hint`} className="wk-dialog-hint">{t("Include the file name")}{scope ? ` ${t("and keep it inside this project’s context")}` : ` ${t("relative to Wiki")}`}.</p>}
          <input
            id={fieldId}
            name={dialog.kind === "rename" ? "name" : "destination"}
            className="field"
            autoFocus
            required
            spellCheck={false}
            autoComplete="off"
            disabled={busy || readOnly}
            aria-describedby={dialog.kind === "move" ? `${fieldId}-hint` : undefined}
            value={dialog.value}
            onChange={(e) => setDialog({ ...dialog, value: e.target.value })}
          />
          <div className="wk-tools">
            <Button type="submit" variant="primary" loading={busy} disabled={readOnly}>
              {dialog.kind === "rename" ? t("Rename") : t("Move")}
            </Button>
            <Button disabled={busy} onClick={closeDialog}>{t("Cancel")}</Button>
          </div>
        </form>
      )}

      {confirm && (
        <div className="wk-dialog card">
          <h2 className="wk-dialog-title">{t("Delete")} {baseName(confirm)}?</h2>
          <div className="text-secondary text-ink-300">
            {t("This item moves to Trash. It can be restored later.")}
          </div>
          <div className="wk-tools">
            <Button
              variant="danger"
              disabled={busy}
              onClick={() => {
                const entry = rows.find((r) => r.path === confirm);
                if (entry) void remove(entry);
              }}
            >
              {t("Move to Trash")}
            </Button>
            <Button disabled={busy} onClick={() => setConfirm(null)}>{t("Cancel")}</Button>
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
                <Link className="wk-ftile-link" href={openHref(entry)} title={entry.name}>
                  <span className="wk-fthumb">
                    {entry.kind === "dir" ? (
                      <KindIcon kind="dir" size={22} />
                    ) : blob === "image" ? (
                      <WikiImage path={entry.path} alt="" />
                    ) : (
                      <>
                        <KindIcon kind="page" size={22} />
                        <span className="chip wk-fkind">{kindLabel(entry.name, entry.mime)}</span>
                      </>
                    )}
                    {entry.locked && <span className="wk-flock">locked</span>}
                  </span>
                  <span className="wk-fmeta">
                    <span className="wk-fname">{entry.name}</span>
                  </span>
                </Link>
                {itemMenu(entry)}
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
              <Link className="wk-fname" href={openHref(entry)}>
                {entry.name}
              </Link>
              <span className="wk-fsub">
                {entry.kind === "dir" ? "folder" : kindLabel(entry.name, entry.mime).toLowerCase()}
              </span>
              {entry.locked && <span className="wk-flock">locked</span>}
              {itemMenu(entry)}
            </div>
          ))}
        </div>
      )}
    </>
  );
}
