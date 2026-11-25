import { useEffect, useId, useRef, useState } from "react";
import { NO_FILTERS } from "../../lib/filters";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, privateSessionGeneration } from "../../lib/privateSession";
import { locationHref } from "../../lib/router";
import Button from "../../ui/Button";
import { IconMore, IconTrash } from "../../ui/icons";
import { wiki, WikiError } from "./api";
import { confirmedDeleteLanding } from "./trash";

/** A durable, access-checked Board link, without source-view state or tokens. */
export function wikiReaderHref(path: string, at?: string, section?: string, page?: number): string {
  return locationHref({
    route: { screen: "wiki", mode: "browse", path, query: null, at, section, page },
    project: "all", view: "list", openId: null, filters: NO_FILTERS,
  });
}

export function CopyWikiLink({ href }: { href: string }) {
  const t = useTranslate();
  const [result, setResult] = useState<{ href: string; address?: string } | null>(null);
  const manual = useRef<HTMLInputElement>(null);
  const fallback = result?.href === href ? result.address : undefined;
  useEffect(() => { manual.current?.select(); }, [fallback]);
  useEffect(() => onPrivateReadsPaused(() => setResult(null)), []);
  const copy = async () => {
    const generation = privateSessionGeneration();
    const address = new URL(href, window.location.origin).href;
    try {
      await navigator.clipboard.writeText(address);
      if (generation === privateSessionGeneration()) setResult({ href });
    } catch {
      if (generation === privateSessionGeneration()) setResult({ href, address });
    }
  };
  return <>
    <Button onClick={() => void copy()}>{t("Copy link")}</Button>
    <span role="status" className={result?.href === href ? "wk-meta" : "sr-only"}>
      {result?.href === href && t(fallback ? "Could not copy the link" : "Link copied")}
    </span>
    {fallback && <input ref={manual} className="field" aria-label={t("Copy this link manually")} value={fallback} readOnly />}
  </>;
}

export function ReaderTrash({ path, onRemoved, onUnconfirmed, onFail }: {
  path: string;
  onRemoved: (landing: string) => void;
  onUnconfirmed: () => void;
  onFail: (error: unknown) => void;
}) {
  const t = useTranslate();
  const id = useId();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const mounted = useRef(true);
  const pending = useRef(false);
  useEffect(() => {
    mounted.current = true;
    const stop = onPrivateReadsPaused(() => {
      mounted.current = false;
      dialog.current?.close();
      setOpen(false);
    });
    return () => { mounted.current = false; stop(); };
  }, []);
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setOpen(false);
      trigger.current?.focus();
    };
    window.addEventListener("keydown", dismiss);
    return () => window.removeEventListener("keydown", dismiss);
  }, [open]);
  const remove = async () => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    try {
      const landing = confirmedDeleteLanding(await wiki.rm(path), path);
      if (!mounted.current) return;
      dialog.current?.close();
      if (landing) onRemoved(landing);
      else onUnconfirmed();
    } catch (error) {
      if (!mounted.current) return;
      dialog.current?.close();
      if (error instanceof WikiError && [400, 401, 403, 404, 409].includes(error.status)) onFail(error);
      else onUnconfirmed();
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  };
  return <>
    <div className="relative">
      <button type="button" className="btn" ref={trigger} aria-label={t("File actions")} aria-expanded={open} aria-controls={open ? `${id}-actions` : undefined} onClick={() => setOpen(!open)}><IconMore /></button>
      {open && <>
        <div className="wk-menu-backdrop" onClick={() => setOpen(false)} />
        <div id={`${id}-actions`} className="wk-fmenu right-0 top-full mt-1">
          <button type="button" className="danger" onClick={() => { setOpen(false); dialog.current?.showModal(); }}><IconTrash />{t("Move to Trash")}</button>
        </div>
      </>}
    </div>
    <dialog ref={dialog} className="wk-reader-trash card" aria-labelledby={`${id}-title`} aria-describedby={`${id}-effect`} onClose={() => trigger.current?.focus()} onCancel={(event) => { if (pending.current) event.preventDefault(); }}>
      <h2 id={`${id}-title`} className="wk-dialog-title">{t("Move this file to Trash?")}</h2>
      <p className="font-mono break-all">{path}</p>
      <p id={`${id}-effect`} className="text-secondary text-ink-300">{t("This item moves to .trash/ with its stored bytes. Restoring it is the operator’s step.")}</p>
      <div className="wk-tools mt-4 justify-end">
        <button type="button" className="btn" autoFocus onClick={() => dialog.current?.close()} disabled={busy}>{t("Cancel")}</button>
        <Button variant="danger" loading={busy} disabled={busy} onClick={() => void remove()}>{t("Move to Trash")}</Button>
      </div>
    </dialog>
  </>;
}

export function movedWikiTarget(path: string, text: string): string | null {
  // Recognize the legacy pointer format, not arbitrary text containing a path.
  const firstLine = text.trimStart().split(/\r?\n/, 1)[0].replace(/^(?:#{1,6}\s+)?(?:\*\*)?/, "");
  const header = /^MOVED\s*(?:→|[—–-])\s*(.*)$/i.exec(firstLine);
  if (!header) return null;
  for (const line of [header[1], ...text.split(/\r?\n/).slice(1, 16)]) {
    const target = line.trim().replace(/^`([^`]+)`$/, "$1");
    if (target === path || target.length > 2048 || !/^(?:global|projects|users|agents)\/[^\\\u0000-\u001f\u007f]+$/u.test(target)) continue;
    if (target.split("/").some((segment) => !segment || segment === "." || segment === "..")) continue;
    return target;
  }
  return null;
}
