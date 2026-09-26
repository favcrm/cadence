/**
 * The upload queue (CAD-581). The server caps a file at 100 MB
 * (`wiki.max_upload_bytes`, CAD-580); the client pre-checks the same cap
 * so an oversize file is rejected before it costs a round trip, and shows
 * the server's own message per file when it still refuses one.
 */

export const MAX_UPLOAD_BYTES = 100 * 1024 * 1024;

export type UploadState = "queued" | "uploading" | "done" | "error";

export interface UploadItem {
  id: string;
  name: string;
  size: number;
  state: UploadState;
  /** 0–100; only meaningful while uploading. */
  progress: number;
  error?: string;
}

export function capLabel(cap = MAX_UPLOAD_BYTES): string {
  return `${Math.round(cap / (1024 * 1024))} MB`;
}

/** The pre-check's reason, or null when the file may go. */
export function sizeError(size: number, cap = MAX_UPLOAD_BYTES): string | null {
  return size > cap ? `over ${capLabel(cap)} — rejected` : null;
}

/** Queue a dropped or picked selection; oversize files start failed. */
export function queued(
  files: { name: string; size: number }[],
  startId = 0,
  cap = MAX_UPLOAD_BYTES,
): UploadItem[] {
  return files.map((file, i) => {
    const bad = sizeError(file.size, cap);
    return {
      id: `u${startId + i}`,
      name: file.name,
      size: file.size,
      state: bad ? ("error" as const) : ("queued" as const),
      progress: 0,
      error: bad ?? undefined,
    };
  });
}

function patch(items: UploadItem[], id: string, next: Partial<UploadItem>): UploadItem[] {
  return items.map((item) => (item.id === id ? { ...item, ...next } : item));
}

export const startUpload = (items: UploadItem[], id: string) =>
  patch(items, id, { state: "uploading", progress: 0, error: undefined });

export const uploadProgress = (items: UploadItem[], id: string, percent: number) =>
  patch(items, id, { state: "uploading", progress: Math.max(0, Math.min(100, Math.round(percent))) });

export const uploadDone = (items: UploadItem[], id: string) =>
  patch(items, id, { state: "done", progress: 100, error: undefined });

export const uploadFailed = (items: UploadItem[], id: string, error: string) =>
  patch(items, id, { state: "error", error });

/** The files still waiting to start, in queue order. */
export function pending(items: UploadItem[]): string[] {
  return items.filter((i) => i.state === "queued").map((i) => i.id);
}

export function counts(items: UploadItem[]): { done: number; failed: number; total: number } {
  return {
    done: items.filter((i) => i.state === "done").length,
    failed: items.filter((i) => i.state === "error").length,
    total: items.length,
  };
}

/** One line per file for the server's refusal, never the whole batch's. */
export function serverErrorText(error: unknown): string {
  const message = (error as { message?: unknown } | null)?.message;
  return typeof message === "string" && message ? message : String(error);
}
