import { getDocument, PDFWorker, type PDFDocumentProxy } from "pdfjs-dist/legacy/build/pdf.mjs";
import PdfWorkerScript from "pdfjs-dist/legacy/build/pdf.worker.min.mjs?worker";
import { ATTACH_MAX_BYTES } from "../app-shell/chat/attach";

const PDF_ASSETS = "/assets/pdfjs/";
const MAX_PAGE_SIDE = 2048;

export type LoadedPdfPages = {
  count: number;
  render(pages: readonly number[], name: string): Promise<File[]>;
};

export async function loadPdfPages(data: Uint8Array<ArrayBuffer>, signal: AbortSignal): Promise<LoadedPdfPages> {
  signal.throwIfAborted();
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
  port.addEventListener("error", () => unavailable.reject(new Error("PDF worker unavailable")), { once: true });
  signal.addEventListener("abort", () => {
    unavailable.reject(signal.reason);
    const terminate = () => {
      worker.destroy();
      port.terminate();
    };
    const deadline = setTimeout(terminate, 250);
    void task.destroy().catch(() => {}).finally(() => {
      clearTimeout(deadline);
      terminate();
    });
  }, { once: true });
  const pdf = await Promise.race([task.promise, unavailable.promise]);
  return {
    count: pdf.numPages,
    render: (pages, name) => Promise.race([renderPdfPages(pdf, pages, name, signal), unavailable.promise]),
  };
}

async function renderPdfPages(
  pdf: PDFDocumentProxy,
  pages: readonly number[],
  name: string,
  signal: AbortSignal,
): Promise<File[]> {
  const stem = (name.replace(/\.pdf$/i, "").replace(/[/\\\x00-\x1f\x7f]/g, "_").trim() || "document").slice(0, 100);
  const canvas = document.createElement("canvas");
  const files: File[] = [];
  for (const number of pages) {
    signal.throwIfAborted();
    const page = await pdf.getPage(number);
    signal.throwIfAborted();
    try {
      const { width, height } = page.getViewport({ scale: 1 });
      const longest = Math.max(width, height);
      if (!(width > 0 && height > 0 && Number.isFinite(longest))) throw new Error("Invalid PDF page dimensions");
      const viewport = page.getViewport({ scale: Math.min(2, MAX_PAGE_SIDE / longest) });
      canvas.width = Math.max(1, Math.min(MAX_PAGE_SIDE, Math.ceil(viewport.width)));
      canvas.height = Math.max(1, Math.min(MAX_PAGE_SIDE, Math.ceil(viewport.height)));
      const render = page.render({ canvas, viewport, background: "white" });
      const cancel = () => render.cancel();
      signal.addEventListener("abort", cancel, { once: true });
      try {
        await render.promise;
      } finally {
        signal.removeEventListener("abort", cancel);
      }
      signal.throwIfAborted();
      const png = Promise.withResolvers<Blob | null>();
      canvas.toBlob(png.resolve, "image/png");
      const blob = await png.promise;
      signal.throwIfAborted();
      if (!blob || blob.size === 0 || blob.size > ATTACH_MAX_BYTES) throw new Error("PDF page PNG outside attachment limits");
      files.push(new File([blob], `${stem}-page-${number}.png`, { type: "image/png" }));
    } finally {
      page.cleanup();
      canvas.width = canvas.height = 0;
    }
  }
  return files;
}
