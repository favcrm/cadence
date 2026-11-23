/**
 * Hosted-board wiki facts (CAD-1883). The tenant Worker (AOS-418) takes 15 MiB
 * per upload, refuses 0-byte files, and reports a blob's extraction as a
 * `status`. These helpers keep that vocabulary out of the panes.
 */

export const HOSTED_UPLOAD_MAX_BYTES = 15 * 1024 * 1024;

/** Worker blob status -> a plain sentence (an English locale key) and whether it needs attention. */
export function statusText(status: string | undefined | null): { text: string; warn: boolean } | null {
  switch (status) {
    case "pending": return { text: "text extraction was queued — search availability is not confirmed", warn: false };
    case "indexed": return { text: "indexed — searchable", warn: false };
    case "empty": return { text: "saved, but no text was found in it", warn: true };
    case "failed": return { text: "saved, but text extraction failed", warn: true };
    case "unsupported": return { text: "saved, but text in this file type cannot be searched", warn: true };
    default: return null;
  }
}

/** Hosted attribution is a subject, not a display name; native authors are already labels. */
export function whoLabel(raw: string, viewerSub: string | null | undefined, t: (key: string) => string, hosted?: boolean): string {
  if (viewerSub && raw === viewerSub) return t("you");
  return hosted === false ? raw : t("a contributor");
}
