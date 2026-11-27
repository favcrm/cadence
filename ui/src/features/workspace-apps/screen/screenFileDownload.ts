// Host-mediated file download handler for sandboxed app frames (`sandbox="allow-scripts"`).
// Frames cannot initiate downloads directly without `allow-downloads`; this enforces strict
// source binding, canonical extension-to-MIME allowlists, safe basenames, and a 16 MiB cap.

export const SCREEN_FILE_DOWNLOAD_TYPE = "cadence-file-download";
export const SCREEN_FILE_DOWNLOAD_ACK_TYPE = "cadence-file-download-ack";
export const SCREEN_FILE_DOWNLOAD_MAX_BYTES = 16 * 1024 * 1024;
const SCREEN_FILE_NAME_MAX_CHARS = 120;
const REQUEST_ID_RE = /^[A-Za-z0-9._-]{1,128}$/;

const ALLOWED_DOWNLOAD_MIMES: ReadonlyMap<string, string> = new Map([
  ["md", "text/markdown;charset=utf-8"],
  ["html", "text/html;charset=utf-8"],
  ["csv", "text/csv;charset=utf-8"],
  ["xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"],
  ["pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"],
  ["docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"],
]);

export interface ValidatedScreenFileDownload {
  requestId: string;
  fileName: string;
  mimeType: string;
  bytes: ArrayBuffer;
}

function normalizeMimeBase(raw: string): string {
  return raw.split(";")[0]?.trim().toLowerCase() ?? "";
}

function isSafeDownloadFileName(raw: string): boolean {
  if (!raw || raw.length > SCREEN_FILE_NAME_MAX_CHARS) return false;
  if (raw.includes("/") || raw.includes("\\") || raw.includes("..") || raw === ".") return false;
  for (let i = 0; i < raw.length; i++) {
    const code = raw.charCodeAt(i);
    if (code < 32 || code === 127) return false;
  }
  return true;
}

function extractArrayBuffer(raw: unknown): ArrayBuffer | null {
  const view =
    raw instanceof ArrayBuffer
      ? new Uint8Array(raw)
      : raw instanceof Uint8Array
        ? raw
        : null;
  if (!view || view.byteLength === 0 || view.byteLength > SCREEN_FILE_DOWNLOAD_MAX_BYTES) {
    return null;
  }
  const copy = new Uint8Array(view.byteLength);
  copy.set(view);
  return copy.buffer;
}

export function parseScreenFileDownload(data: unknown): ValidatedScreenFileDownload | null {
  if (!data || typeof data !== "object" || Array.isArray(data)) return null;
  const record = data as Record<string, unknown>;
  if (record.type !== SCREEN_FILE_DOWNLOAD_TYPE) return null;
  if (typeof record.requestId !== "string" || !REQUEST_ID_RE.test(record.requestId)) return null;
  if (typeof record.filename !== "string" || !isSafeDownloadFileName(record.filename)) return null;
  const dot = record.filename.lastIndexOf(".");
  if (dot <= 0 || dot === record.filename.length - 1) return null;
  const ext = record.filename.slice(dot + 1).toLowerCase();
  const canonicalMime = ALLOWED_DOWNLOAD_MIMES.get(ext);
  if (!canonicalMime) return null;
  if (typeof record.mimeType !== "string") return null;
  if (normalizeMimeBase(record.mimeType) !== normalizeMimeBase(canonicalMime)) return null;
  const bytes = extractArrayBuffer(record.bytes);
  if (!bytes) return null;
  return {
    requestId: record.requestId,
    fileName: record.filename,
    mimeType: canonicalMime,
    bytes,
  };
}

export interface DownloadMessageEventLike {
  source: MessageEventSource | null;
  data: unknown;
}

export interface DownloadReplyTarget {
  postMessage(message: unknown, targetOrigin: string): void;
}

export function handleScreenFileDownloadMessage(
  event: DownloadMessageEventLike,
  expectedSource: DownloadReplyTarget | null,
  isCurrent: () => boolean,
  saveFile: (bytes: ArrayBuffer, mimeType: string, fileName: string) => void,
): boolean {
  const data = event.data as Record<string, unknown> | null;
  if (!data || typeof data !== "object" || data.type !== SCREEN_FILE_DOWNLOAD_TYPE) {
    return false;
  }
  if (!expectedSource || event.source !== (expectedSource as unknown as MessageEventSource) || !isCurrent()) {
    return true;
  }
  const parsed = parseScreenFileDownload(data);
  if (!parsed) return true;
  saveFile(parsed.bytes, parsed.mimeType, parsed.fileName);
  expectedSource.postMessage(
    { type: SCREEN_FILE_DOWNLOAD_ACK_TYPE, requestId: parsed.requestId },
    "*",
  );
  return true;
}
