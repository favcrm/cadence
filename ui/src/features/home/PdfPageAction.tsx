import { createContext, useCallback, useId, useLayoutEffect, useRef, useState, useSyncExternalStore, type FormEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslate } from "../../lib/locale";
import { beginPrivateOperation, type PrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import Button from "../../ui/Button";
import { ATTACH_MAX_BYTES, ATTACH_MAX_FILES, type AttachDestination, type AttachItem } from "../app-shell/chat/attach";
import { activeComposerSlot, addFiles, attachItems, removeAttach, requestComposerFocus, subscribeAttach } from "../app-shell/chat/composerStore";
import { WorkspaceDialog } from "../workspace-apps/WorkspaceDialog";
import "../workspace-apps/workspace-apps.css";
import { attachmentHref, type AttachmentFile } from "./AttachmentMedia";
import type { LoadedPdfPages } from "./pdfPageImages";

interface PageTarget {
  key: string;
  destination: AttachDestination;
  isCurrent?: () => boolean;
}

type PageSource =
  | { kind: "retained"; file: AttachmentFile; conversation: string }
  | { kind: "draft"; file: File; itemKey: string };

export const PdfPageTarget = createContext<PageTarget | null>(null);
const STEP_TIMEOUT = 30_000;

function selection(input: string, pageCount: number, slots: number): number[] | "range" | "slots" {
  const pages = new Set<number>();
  for (const word of input.split(",")) {
    const range = /^([0-9]+)(?:\s*[-–]\s*([0-9]+))?$/.exec(word.trim());
    if (!range) return "range";
    const first = Number(range[1]);
    const last = range[2] === undefined ? first : Number(range[2]);
    if (first < 1 || first > last || last > pageCount) return "range";
    for (let page = first; page <= last; page += 1) {
      pages.add(page);
      if (pages.size > slots) return "slots";
    }
  }
  return [...pages].sort((a, b) => a - b);
}

async function readPdf(source: PageSource, operation: PrivateOperation) {
  const { file, kind } = source;
  if (!Number.isSafeInteger(file.size) || file.size < 1 || file.size > ATTACH_MAX_BYTES) throw new Error("PDF outside attachment limits");
  if (kind === "draft") return new Uint8Array(await file.arrayBuffer());
  const response = await operation.fetch(attachmentHref(file.id, source.conversation), {
    cache: "no-store",
    redirect: "error",
    headers: sessionHeaders(),
  });
  if (!response.ok || response.headers.get("Content-Type")?.split(";", 1)[0] !== "application/pdf" || !response.body) {
    throw new Error("PDF unavailable");
  }
  const reader = response.body.getReader();
  const bytes = new Uint8Array(file.size);
  let offset = 0;
  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) break;
      if (offset + chunk.value.length > bytes.length) throw new Error("PDF size changed");
      bytes.set(chunk.value, offset);
      offset += chunk.value.length;
    }
  } finally {
    reader.releaseLock();
  }
  if (offset !== bytes.length) throw new Error("PDF size changed");
  operation.assertCurrent();
  return bytes;
}

export default function PdfPageAction({ source, target }: { source: PageSource; target: PageTarget }) {
  const t = useTranslate();
  const [open, setOpen] = useState(false);
  const label = t(source.kind === "draft" ? "Replace PDF with page images" : "Attach pages as images");
  return (
    <>
      <Button size="sm" variant="ghost" aria-label={`${label}: ${source.file.name}`} onClick={() => setOpen(true)}>
        {label}
      </Button>
      {open && createPortal(<PdfPageDialog source={source} target={target} title={label} close={() => setOpen(false)} />, document.body)}
    </>
  );
}

function PdfPageDialog({ source, target, title, close }: {
  source: PageSource;
  target: PageTarget;
  title: string;
  close: () => void;
}) {
  const t = useTranslate();
  const [original] = useState(source);
  const replaceKey = original.kind === "draft" ? original.itemKey : null;
  const [slot] = useState(() => activeComposerSlot(target.key));
  const subscribe = useCallback((listener: () => void) => subscribeAttach(target.key, listener), [target.key]);
  const [pdf, setPdf] = useState<LoadedPdfPages | null>(null);
  const [range, setRange] = useState("1");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const operation = useRef<PrivateOperation | null>(null);
  const mount = useRef<AbortSignal | null>(null);
  const items = useSyncExternalStore(subscribe, () => attachItems(target.key));
  const occupied = (queue: readonly AttachItem[]) => queue.length - Number(replaceKey !== null && queue.some((item) => item.key === replaceKey));
  const slots = Math.max(0, ATTACH_MAX_FILES - occupied(items));
  const limit = pdf ? Math.min(pdf.count, slots) : slots;
  const full = slots === 0;

  useLayoutEffect(() => {
    // The bundled PDF.js build still requires native Promise.withResolvers.
    // Older browsers can use the rest of chat and open the original PDF.
    if (typeof Promise.withResolvers !== "function" || typeof Worker === "undefined") {
      setError("Update your browser to attach PDF pages as images.");
      return;
    }
    const scope = new AbortController();
    mount.current = scope.signal;
    let op: PrivateOperation | undefined;
    const failLoad = () => {
      op?.dispose();
      if (!scope.signal.aborted) setError("This PDF couldn’t be opened. It may be unavailable, damaged, or password-protected.");
    };
    // A dynamic import cannot be aborted; show the refusal at the deadline
    // even when the browser is still waiting for the renderer chunk.
    const deadline = setTimeout(failLoad, STEP_TIMEOUT);
    async function load() {
      try {
        op = beginPrivateOperation(scope.signal);
        operation.current = op;
        const [{ loadPdfPages }, bytes] = await Promise.all([import("./pdfPageImages"), readPdf(original, op)]);
        op.assertCurrent();
        const loaded = await loadPdfPages(bytes, op.signal);
        op.assertCurrent();
        setPdf(loaded);
      } catch {
        failLoad();
      } finally {
        clearTimeout(deadline);
      }
    }
    void load();
    return () => {
      clearTimeout(deadline);
      scope.abort();
      operation.current?.dispose();
      operation.current = null;
    };
  }, [original]);

  async function submit(event: FormEvent) {
    event.preventDefault();
    event.stopPropagation();
    const op = operation.current;
    const scope = mount.current;
    if (!pdf || busy || !op || !scope || scope.aborted) return;
    const current = () => slot !== undefined && activeComposerSlot(target.key) === slot &&
      (target.isCurrent?.() ?? true) && (replaceKey === null || attachItems(target.key)
        .some((item) => item.key === replaceKey && item.file === original.file));
    if (!current()) {
      setError("The draft changed. Reopen the PDF to add pages to the current draft.");
      return;
    }
    const pages = selection(range, pdf.count, ATTACH_MAX_FILES - occupied(attachItems(target.key)));
    if (pages === "range" || pages === "slots") {
      setError(pages === "range" ? "Enter page numbers or ranges within this PDF." : "The selected pages don’t fit in this draft.");
      return;
    }
    setError(null);
    setBusy(true);
    const deadline = setTimeout(() => op.dispose(), STEP_TIMEOUT);
    try {
      op.assertCurrent();
      const images = await pdf.render(pages, original.file.name);
      op.assertCurrent();
      if (!current()) {
        setError("The draft changed. Reopen the PDF to add pages to the current draft.");
        return;
      }
      if (occupied(attachItems(target.key)) + images.length > ATTACH_MAX_FILES) {
        setError("The selected pages don’t fit in this draft.");
        return;
      }
      if (replaceKey !== null) removeAttach(target.key, replaceKey);
      addFiles(target.key, images, target.destination);
      if (replaceKey !== null) requestComposerFocus(target.key);
      close();
    } catch {
      if (!scope.aborted) {
        op.dispose();
        setPdf(null);
        setError("The selected pages couldn’t be converted. No images were added.");
      }
    } finally {
      clearTimeout(deadline);
      if (!scope.aborted) setBusy(false);
    }
  }

  return (
    <WorkspaceDialog title={title} onClose={close}>
      <form className="grid gap-3" onSubmit={(event) => void submit(event)}>
        <p className="m-0 min-w-0 truncate" title={original.file.name}>{original.file.name}</p>
        {!pdf && !error && <p className="m-0" role="status">{t("Opening PDF…")}</p>}
        {pdf && <>
          <label className="grid gap-1" htmlFor={id}>
            {t("Pages")}
            <input id={id} className="wa-input" value={range} onChange={(event) => { setRange(event.target.value); setError(null); }}
              placeholder="1, 3–5" maxLength={120} disabled={busy || full} aria-describedby={`${id}-hint`} aria-invalid={error !== null || undefined} />
          </label>
          <p id={`${id}-hint`} className="m-0 text-muted">{full
            ? t("This draft has no free attachment slots. Remove an attachment first.")
            : t("Choose up to {limit} of {count} pages, e.g. 1, 3–5. Images are added to the draft, not sent.")
              .replace("{limit}", String(limit)).replace("{count}", String(pdf.count))}</p>
        </>}
        {error && <p className="m-0 text-fail" role="alert">{t(error)}</p>}
        {pdf && <div className="flex justify-end">
          <Button type="submit" variant="primary" disabled={full || range.trim() === ""} loading={busy}>
            {busy ? t("Converting pages…") : replaceKey === null ? t("Add to draft") : t("Replace in draft")}
          </Button>
        </div>}
      </form>
    </WorkspaceDialog>
  );
}
