import { useEffect, useState } from "react";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import type { LoadedPdfPages } from "../home/pdfPageImages";
import "./document-preview.css";

const MAX_HTML_BYTES = 1024 * 1024;
const HTML_TAGS = new Set([
  "a", "abbr", "article", "b", "blockquote", "br", "caption", "code", "col", "colgroup",
  "dd", "del", "details", "div", "dl", "dt", "em", "figcaption", "figure", "footer",
  "h1", "h2", "h3", "h4", "h5", "h6", "header", "hr", "i", "ins", "kbd", "li",
  "main", "mark", "ol", "p", "pre", "q", "s", "samp", "section", "small", "span",
  "strong", "sub", "summary", "sup", "table", "tbody", "td", "tfoot", "th", "thead",
  "time", "tr", "u", "ul", "var",
]);
const STYLE_PROPERTIES = new Set([
  "background-color", "border", "border-color", "border-style", "border-width", "color",
  "font-family", "font-size", "font-style", "font-weight", "letter-spacing", "line-height",
  "margin", "margin-bottom", "margin-left", "margin-right", "margin-top", "padding",
  "padding-bottom", "padding-left", "padding-right", "padding-top", "text-align",
  "text-decoration", "vertical-align", "white-space", "word-break",
]);

/** Parse in an inert template, then construct a fresh tree from an explicit allowlist. */
function staticHtml(text: string): string {
  const template = document.createElement("template");
  template.innerHTML = text;
  const output = document.createElement("div");
  const copy = (source: Node, parent: Node) => {
    if (source.nodeType === Node.TEXT_NODE) {
      parent.appendChild(document.createTextNode(source.textContent ?? ""));
      return;
    }
    if (!(source instanceof HTMLElement) || !HTML_TAGS.has(source.localName)) return;
    const element = document.createElement(source.localName);
    for (const attr of ["title", "colspan", "rowspan", "scope", "start", "reversed"]) {
      if (source.hasAttribute(attr)) element.setAttribute(attr, source.getAttribute(attr) ?? "");
    }
    // Only simple, static CSS values; no escapes, functions, URLs, or custom properties.
    const style = source.getAttribute("style");
    if (style) {
      for (const declaration of style.split(";")) {
        const colon = declaration.indexOf(":");
        if (colon < 0) continue;
        const property = declaration.slice(0, colon).trim().toLowerCase();
        const value = declaration.slice(colon + 1).trim();
        if (STYLE_PROPERTIES.has(property) && /^[\w\s#.,%+-]+$/.test(value)) {
          element.style.setProperty(property, value);
        }
      }
    }
    parent.appendChild(element);
    for (const child of source.childNodes) copy(child, element);
  };
  for (const child of template.content.childNodes) copy(child, output);
  if (!output.textContent?.trim()) throw new Error("No supported HTML content");
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src 'none'; font-src 'none'; connect-src 'none'; form-action 'none'; base-uri 'none'"><meta name="referrer" content="no-referrer"><style>body{margin:clamp(16px,4vw,34px);color:#1d2731;background:white;font:15px/1.55 system-ui,sans-serif;overflow-wrap:anywhere}table{border-collapse:collapse;max-width:100%}th,td{border:1px solid #d3d8dd;padding:5px 8px}pre{white-space:pre-wrap}</style></head><body>${output.innerHTML}</body></html>`;
}

function dataUrl(blob: Blob, signal: AbortSignal): Promise<string> {
  const { promise, resolve, reject } = Promise.withResolvers<string>();
  const reader = new FileReader();
  const abort = () => reader.abort();
  signal.addEventListener("abort", abort, { once: true });
  reader.onload = () => resolve(String(reader.result));
  reader.onerror = () => reject(reader.error);
  reader.onabort = () => reject(signal.reason);
  reader.onloadend = () => signal.removeEventListener("abort", abort);
  if (signal.aborted) reject(signal.reason);
  else reader.readAsDataURL(blob);
  return promise;
}

export default function DocumentPreview({ bytes, name, kind }: { bytes: Blob; name: string; kind: "pdf" | "html" }) {
  const t = useTranslate();
  const [html, setHtml] = useState<string | null>(null);
  const [pdf, setPdf] = useState<LoadedPdfPages | null>(null);
  const [page, setPage] = useState(1);
  const [image, setImage] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const abort = new AbortController();
    setHtml(null);
    setPdf(null);
    setPage(1);
    setImage(null);
    setFailed(false);
    void (async () => {
      try {
        if (kind === "html") {
          if (bytes.size > MAX_HTML_BYTES) throw new Error("HTML too large");
          const text = new TextDecoder("utf-8", { fatal: true }).decode(await bytes.arrayBuffer());
          if (!abort.signal.aborted) setHtml(staticHtml(text));
        } else {
          // The Vite worker entry exists only in the browser bundle.
          const { loadPdfPages } = await import("../home/pdfPageImages");
          abort.signal.throwIfAborted();
          const data = new Uint8Array(await bytes.arrayBuffer());
          const loaded = await loadPdfPages(data, abort.signal);
          if (!abort.signal.aborted) setPdf(loaded);
        }
      } catch {
        if (!abort.signal.aborted) {
          setFailed(true);
          abort.abort();
        }
      }
    })();
    return () => abort.abort();
  }, [bytes, kind]);

  useEffect(() => {
    if (!pdf) return;
    const abort = new AbortController();
    setImage(null);
    void (async () => {
      try {
        const [file] = await pdf.render([page], name);
        const url = await dataUrl(file, abort.signal);
        if (!abort.signal.aborted) setImage(url);
      } catch {
        if (!abort.signal.aborted) setFailed(true);
      }
    })();
    return () => abort.abort();
  }, [pdf, page, name]);

  if (failed) return <div className="wk-download"><div className="wk-etitle">{name}</div><div className="wk-ebody">{t("Preview unavailable. Download the original to open it.")}</div></div>;
  if (kind === "html" && html !== null) return <div className="wk-document-static">
    <p className="wk-meta">{t("Read-only HTML preview")} · {t("Scripts and external content blocked")}</p>
    <iframe className="wk-document-html" title={name} sandbox="" referrerPolicy="no-referrer" srcDoc={html} />
  </div>;
  if (kind === "pdf" && pdf) return <div className="wk-document-pdf">
    <div className="wk-document-controls">
      <Button disabled={page <= 1} onClick={() => setPage(page - 1)}>{t("Previous page")}</Button>
      <span>{t("Page")} {page} / {pdf.count}</span>
      <Button disabled={page >= pdf.count} onClick={() => setPage(page + 1)}>{t("Next page")}</Button>
    </div>
    {image ? <img className="wk-document-page" src={image} alt={`${name} — ${t("Page")} ${page}`} /> : <div className="wk-ebody">{t("Loading preview…")}</div>}
  </div>;
  return <div className="wk-ebody">{t("Loading preview…")}</div>;
}
