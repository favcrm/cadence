import { useEffect, useRef, useState } from "react";
import { slotBox } from "./screenSlot";
import type { SlotAnchor } from "./screenProtocol";

export interface InertPreview { id: string; document: string; anchor: SlotAnchor; mode: "draft" | "saved" }
const TAGS = new Set("html head body title style div span p br hr h1 h2 h3 h4 h5 h6 table thead tbody tfoot tr td th caption colgroup col img a b strong i em u s small blockquote pre code ul ol li center font".split(" "));
const ATTRS = new Set("style class id title lang dir align valign width height bgcolor border cellpadding cellspacing colspan rowspan role aria-label alt color face size".split(" "));
/** Parse only into an inert template: never attach raw markup to the board,
 * and never use DOMParser's potentially fetching image/iframe document. */
export function inertDocument(preview: InertPreview, remoteImages: readonly string[]): string {
  const template = document.createElement("template");
  template.innerHTML = preview.document;
  const saved = preview.mode === "saved";
  const remote = saved && remoteImages.includes("instagram-cdn");
  const image = (raw: string): string | null => {
    if (/^data:image\/(?:png|jpeg|gif|webp|avif);base64,[A-Za-z0-9+/]+=*$/.test(raw)) return raw;
    if (!saved) return null;
    try {
      const url = new URL(raw, window.location.origin);
      if (url.username || url.password || url.search || url.hash) return null;
      if (url.origin === window.location.origin && /^\/assets\/[A-Za-z0-9_-]+\.(?:png|jpg|jpeg|gif|webp|avif|svg)$/.test(url.pathname)) return url.href;
      if (remote && url.protocol === "https:" && (url.hostname === "cdninstagram.com" || url.hostname.endsWith(".cdninstagram.com") || url.hostname === "fbcdn.net" || url.hostname.endsWith(".fbcdn.net"))) return url.href;
    } catch { /* Invalid URLs do not load. */ }
    return null;
  };
  for (const element of Array.from(template.content.querySelectorAll("*"))) {
    if (element.namespaceURI !== "http://www.w3.org/1999/xhtml" || !TAGS.has(element.localName)) { element.remove(); continue; }
    for (const attr of Array.from(element.attributes)) {
      if (element.localName === "img" && attr.name === "src") {
        const allowed = image(attr.value);
        if (allowed) element.setAttribute("src", allowed); else element.removeAttribute(attr.name);
      } else if (!ATTRS.has(attr.name)) element.removeAttribute(attr.name);
    }
    if (element.localName === "img") element.setAttribute("referrerpolicy", "no-referrer");
  }
  // Defense in depth survives CSS URL syntax and parser mutation: no active
  // context, network styles, subframes, navigation or board/API fetches.
  const assets = saved ? `${window.location.origin}/assets/` : "";
  const cdn = remote ? "https://cdninstagram.com https://*.cdninstagram.com https://fbcdn.net https://*.fbcdn.net" : "";
  const csp = `default-src 'none'; script-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; img-src data: ${assets} ${cdn}; style-src 'unsafe-inline'; font-src 'none'; form-action 'none'; base-uri 'none'; navigate-to 'none'`;
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${csp}"><meta name="referrer" content="no-referrer"></head><body>${template.innerHTML}</body></html>`;
}
export default function InertPreviewLayer({ preview, remoteImages }: { preview: InertPreview | null; remoteImages: readonly string[] }) {
  const layer = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  useEffect(() => {
    if (!layer.current) return;
    const node = layer.current;
    const measure = () => setSize({ w: node.clientWidth, h: node.clientHeight });
    measure();
    const observer = new ResizeObserver(measure); observer.observe(node);
    return () => observer.disconnect();
  }, []);
  const box = preview ? (preview.anchor === "footer" ? slotBox(preview.anchor, size.w, size.h) : preview.anchor) : null;
  return <div ref={layer} style={{ position: "absolute", inset: 0, pointerEvents: "none", overflow: "hidden", zIndex: 2 }}>
    {preview && box && <iframe title="Inert app document preview" sandbox="" referrerPolicy="no-referrer" srcDoc={inertDocument(preview, remoteImages)}
      style={{ position: "absolute", left: box.x, top: box.y, width: box.w, height: box.h, border: 0, pointerEvents: "auto", background: "white" }} />}
  </div>;
}
