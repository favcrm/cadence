/** CAD-2436: a sent file tile's badge colour family, from the stored MIME type. */
export function fileKind(mime: string): "pdf" | "image" | "video" | "text" | "file" {
  const m = mime.toLowerCase();
  if (m === "application/pdf") return "pdf";
  if (m.startsWith("image/")) return "image";
  if (m.startsWith("video/")) return "video";
  if (m.startsWith("text/") || m === "application/json" || m === "application/xml") return "text";
  return "file";
}
