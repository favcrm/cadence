import { getDocument, PDFWorker, type PDFDocumentProxy } from "pdfjs-dist/legacy/build/pdf.mjs";
import PdfWorkerScript from "pdfjs-dist/legacy/build/pdf.worker.min.mjs?worker";

const PDF_ASSETS = "/assets/pdfjs/";
const MAX_PIXEL_SIDE = 2400;

export interface LoadedPdfDocument {
  pageCount: number;
  renderPage(pageNumber: number, canvas: HTMLCanvasElement, scale: number, signal: AbortSignal): Promise<void>;
  destroy(): void;
}

/**
 * Open a PDF ArrayBuffer inside pdf.js using its bundled worker and local
 * static assets. Clones the input bytes so the caller's buffer never detaches.
 */
export async function openPdfDocument(bytes: ArrayBuffer, signal: AbortSignal): Promise<LoadedPdfDocument> {
  signal.throwIfAborted();
  const data = new Uint8Array(bytes.slice(0));
  const port = new PdfWorkerScript();
  const worker = PDFWorker.create({ port });
  const task = getDocument({
    data,
    worker,
    stopAtErrors: true,
    canvasMaxAreaInBytes: 32 * 1024 * 1024,
    cMapUrl: `${PDF_ASSETS}cmaps/`,
    standardFontDataUrl: `${PDF_ASSETS}standard_fonts/`,
    wasmUrl: `${PDF_ASSETS}wasm/`,
    iccUrl: `${PDF_ASSETS}iccs/`,
  });
  const unavailable = Promise.withResolvers<never>();
  let destroyed = false;
  const cleanup = () => {
    if (destroyed) return;
    destroyed = true;
    const terminate = () => {
      worker.destroy();
      port.terminate();
    };
    const deadline = setTimeout(terminate, 250);
    void task
      .destroy()
      .catch(() => {})
      .finally(() => {
        clearTimeout(deadline);
        terminate();
      });
  };
  port.addEventListener("error", () => unavailable.reject(new Error("PDF worker unavailable")), { once: true });
  signal.addEventListener(
    "abort",
    () => {
      unavailable.reject(signal.reason);
      cleanup();
    },
    { once: true },
  );
  const pdf: PDFDocumentProxy = await Promise.race([task.promise, unavailable.promise]);
  return {
    pageCount: pdf.numPages,
    async renderPage(pageNumber: number, canvas: HTMLCanvasElement, scale: number, pageSignal: AbortSignal) {
      pageSignal.throwIfAborted();
      const page = await pdf.getPage(pageNumber);
      pageSignal.throwIfAborted();
      try {
        const base = page.getViewport({ scale: 1 });
        if (!(base.width > 0 && base.height > 0 && Number.isFinite(base.width) && Number.isFinite(base.height))) {
          throw new Error("Invalid PDF page size");
        }
        const dpr = Math.min(typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1, 2);
        const cssWidth = Math.max(1, base.width * scale);
        const cssHeight = Math.max(1, base.height * scale);
        const longest = Math.max(cssWidth * dpr, cssHeight * dpr);
        const pixelRatio = longest > MAX_PIXEL_SIDE ? MAX_PIXEL_SIDE / Math.max(cssWidth, cssHeight) : dpr;
        const viewport = page.getViewport({ scale: scale * pixelRatio });
        canvas.width = Math.max(1, Math.min(MAX_PIXEL_SIDE, Math.ceil(viewport.width)));
        canvas.height = Math.max(1, Math.min(MAX_PIXEL_SIDE, Math.ceil(viewport.height)));
        canvas.style.width = `${Math.round(cssWidth)}px`;
        canvas.style.height = `${Math.round(cssHeight)}px`;
        const render = page.render({ canvas, viewport, background: "white" });
        const cancel = () => render.cancel();
        pageSignal.addEventListener("abort", cancel, { once: true });
        try {
          await render.promise;
        } finally {
          pageSignal.removeEventListener("abort", cancel);
        }
      } finally {
        page.cleanup();
      }
    },
    destroy: cleanup,
  };
}
