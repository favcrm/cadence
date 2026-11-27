import { useEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import type { LoadedPdfDocument } from "./pdfDocumentLoader";

const ZOOM_STEPS = [0.75, 1, 1.25, 1.5, 2] as const;

/** Render PDF pages inline via lazy-loaded pdf.js worker, with zoom controls and graceful fallback. */
export default function PdfDocumentView({
  title,
  bytes,
  onOpenTab,
  onDownload,
  onOpenOverlay,
  compact = false,
}: {
  title: string;
  bytes: ArrayBuffer;
  onOpenTab: () => void;
  onDownload: () => void;
  onOpenOverlay?: () => void;
  compact?: boolean;
}) {
  const t = useTranslate();
  const [status, setStatus] = useState<"loading" | "ready" | "failed">("loading");
  const [pageCount, setPageCount] = useState(0);
  const [zoomIdx, setZoomIdx] = useState(1);
  const docRef = useRef<LoadedPdfDocument | null>(null);
  const pagesHostRef = useRef<HTMLDivElement | null>(null);
  const zoom = ZOOM_STEPS[zoomIdx] ?? 1;

  useEffect(() => {
    const controller = new AbortController();
    let mounted = true;
    setStatus("loading");
    setPageCount(0);
    docRef.current?.destroy();
    docRef.current = null;
    void (async () => {
      try {
        const mod = await import("./pdfDocumentLoader");
        const doc = await mod.openPdfDocument(bytes, controller.signal);
        if (!mounted || controller.signal.aborted) {
          doc.destroy();
          return;
        }
        docRef.current = doc;
        setPageCount(doc.pageCount);
        setStatus("ready");
      } catch {
        if (mounted && !controller.signal.aborted) {
          setStatus("failed");
        }
      }
    })();
    return () => {
      mounted = false;
      controller.abort();
      docRef.current?.destroy();
      docRef.current = null;
    };
  }, [bytes]);

  useEffect(() => {
    const doc = docRef.current;
    const host = pagesHostRef.current;
    if (status !== "ready" || !doc || !host || pageCount < 1) return;
    const controller = new AbortController();
    void (async () => {
      const canvases = Array.from(host.querySelectorAll<HTMLCanvasElement>("canvas[data-pdf-page]"));
      for (let i = 0; i < canvases.length; i++) {
        if (controller.signal.aborted) return;
        const canvas = canvases[i];
        const pageNo = Number(canvas.dataset.pdfPage || i + 1);
        try {
          await doc.renderPage(pageNo, canvas, zoom, controller.signal);
        } catch {
          if (!controller.signal.aborted) setStatus("failed");
          return;
        }
      }
    })();
    return () => controller.abort();
  }, [status, pageCount, zoom]);

  const pageNumbers = Array.from({ length: pageCount }, (_, i) => i + 1);

  return (
    <div className="canvas-doc canvas-pdf" data-artifact-view="pdf" data-compact={compact || undefined}>
      <div className="cv-pdf-toolbar">
        <div className="cv-pdf-zoom" role="group" aria-label={t("Zoom")}>
          <button
            type="button"
            className="btn btn-ghost"
            disabled={status !== "ready" || zoomIdx <= 0}
            aria-label={t("Zoom out")}
            title={t("Zoom out")}
            onClick={() => setZoomIdx((z) => Math.max(0, z - 1))}
          >
            −
          </button>
          <span className="num text-micro cv-pdf-zoom-val">{Math.round(zoom * 100)}%</span>
          <button
            type="button"
            className="btn btn-ghost"
            disabled={status !== "ready" || zoomIdx >= ZOOM_STEPS.length - 1}
            aria-label={t("Zoom in")}
            title={t("Zoom in")}
            onClick={() => setZoomIdx((z) => Math.min(ZOOM_STEPS.length - 1, z + 1))}
          >
            +
          </button>
          {zoomIdx !== 1 && status === "ready" ? (
            <button
              type="button"
              className="btn btn-ghost"
              aria-label={t("Reset zoom")}
              onClick={() => setZoomIdx(1)}
            >
              100%
            </button>
          ) : null}
        </div>
        {pageCount > 0 ? (
          <span className="num text-micro text-ink-400">
            {pageCount} {t("Pages")}
          </span>
        ) : null}
        {onOpenOverlay ? (
          <button type="button" className="btn btn-ghost cv-pdf-overlay-btn" onClick={onOpenOverlay}>
            {t("Open in overlay")}
          </button>
        ) : null}
      </div>

      {status === "loading" ? (
        <p className="text-label text-ink-400 p-3">{t("Loading PDF preview…")}</p>
      ) : null}

      {status === "failed" ? (
        <div className="cv-pdf-fallback">
          <p className="text-body text-ink-100">{title}</p>
          <p className="text-micro text-ink-400">{t("A preview is not available yet. Open in a new tab or use Download.")}</p>
          <div className="flex flex-wrap items-center gap-3 pt-1">
            <button type="button" className="lnk text-micro" onClick={onOpenTab}>
              {t("Open in new tab")}
            </button>
            <button type="button" className="lnk text-micro" onClick={onDownload}>
              {t("Download")}
            </button>
          </div>
        </div>
      ) : null}

      {status === "ready" ? (
        <div className="cv-pdf-pages" ref={pagesHostRef}>
          {pageNumbers.map((num) => (
            <div key={num} className="cv-pdf-page">
              <canvas data-pdf-page={num} aria-label={`${title} — ${num} / ${pageCount}`} />
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
