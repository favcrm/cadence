/**
 * How one wiki blob opens (CAD-581). Images, video and PDFs get a preview;
 * everything else downloads. SVG and HTML never render inline — an
 * uploaded file must not be able to run script in the board's origin,
 * whatever MIME the server sniffed (CAD-580 serves them as attachments).
 */

export type PreviewKind = "image" | "video" | "pdf" | "download";

const IMAGE_EXT = new Set(["png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "ico"]);
const VIDEO_EXT = new Set(["mp4", "webm", "ogv", "mov", "m4v"]);
const PDF_EXT = new Set(["pdf"]);
/** Types that must never render in a page, only download. */
const NEVER_INLINE_EXT = new Set(["svg", "svgz", "html", "htm", "xhtml", "xml", "mhtml"]);

/** The lower-cased extension without the dot; "" when there is none. */
export function extOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot <= 0 ? "" : name.slice(dot + 1).toLowerCase();
}

export function previewKind(name: string, mime?: string | null): PreviewKind {
  const ext = extOf(name);
  const type = (mime ?? "").toLowerCase();
  const never =
    NEVER_INLINE_EXT.has(ext) || type.includes("svg") || type.includes("html") || type.includes("xml");
  if (never) return "download";
  if (IMAGE_EXT.has(ext) || type.startsWith("image/")) return "image";
  if (VIDEO_EXT.has(ext) || type.startsWith("video/")) return "video";
  if (PDF_EXT.has(ext) || type === "application/pdf") return "pdf";
  return "download";
}

/** A short chip label for a tile — the file's own kind, not its preview. */
export function kindLabel(name: string, mime?: string | null): string {
  switch (previewKind(name, mime)) {
    case "image":
      return "IMG";
    case "video":
      return "VID";
    case "pdf":
      return "PDF";
    default:
      return (extOf(name) || "FILE").toUpperCase().slice(0, 4);
  }
}
