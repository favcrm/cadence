import { sessionHeaders } from "../../lib/sessionKey";
import { notifyPrivateSessionRequired, privateFetch, privateReadsAllowed, privateSessionGeneration, usingPrivateOperation, withPrivateOperation, type PrivateOperation } from "../../lib/privateSession";
import type { WikiVersion } from "./history";
import type { SearchHit } from "./search";
import { normalizeListing, normalizePage, normalizeSearch, normalizeHistory } from "./wire";
import { joinPath } from "./paths";

/**
 * The wiki store's HTTP surface (CAD-580) as this screen reads it. One
 * module owns requests; wire.ts adapts the daemon’s response shapes:
 *
 *   GET  /api/wiki/ls?path=       a folder's entries — or one entry's kind
 *   GET  /api/wiki/file?path=     text as JSON; blobs streamed (Range)
 *   PUT  /api/wiki/file           {path, text, if_rev}
 *   POST /api/wiki/upload         multipart, one file per request
 *   POST /api/wiki/mkdir|mv|rm    {path}, {from, to}, {path}
 *   GET  /api/wiki/search?q=      full-text search
 *   GET  /api/wiki/history?path=  git log (current daemon has no diff/restore)
 */

export type WikiKind = "dir" | "page" | "file";

export interface WikiEntry {
  name: string;
  path: string;
  kind: WikiKind;
  size?: number;
  mime?: string | null;
  rev?: string;
  mtime?: string | null;
  edited_by?: string | null;
  /** Read-only for this caller — the row's lock badge. */
  locked?: boolean;
  read_only?: boolean;
  /** A folder's child count, when the server counts them. */
  entries?: number;
  /** A view over existing agent files (profile/, memory/), not a page. */
  view?: boolean;
}

export interface WikiListing {
  path: string;
  /** Present when the path itself is one entry rather than a folder. */
  kind?: WikiKind;
  entries?: WikiEntry[];
  /** The caller may write here (new page, folder, upload). */
  writable?: boolean;
  locked?: boolean;
  rev?: string;
  edited_by?: string | null;
  mtime?: string | null;
  size?: number;
  mime?: string | null;
}

export interface WikiPage {
  path: string;
  kind: "page" | "file";
  text?: string;
  rev?: string;
  mtime?: string | null;
  edited_by?: string | null;
  size?: number;
  mime?: string | null;
  sha256?: string;
}

export interface WikiSearchResponse {
  hits?: SearchHit[];
  results?: SearchHit[];
}

export interface WikiHistoryResponse {
  path: string;
  rev?: string;
  entries?: WikiVersion[];
  history?: WikiVersion[];
  diff?: string;
  canCompare?: boolean;
}

/** A wiki route's refusal, with the conflict fields the editor reads. */
export class WikiError extends Error {
  status: number;
  code?: string;
  /** The rev that won a refused `if_rev` save. */
  rev?: string;
  author?: string | null;
  at?: string | null;
  constructor(
    message: string,
    status: number,
    opts?: { code?: string; rev?: string; author?: string | null; at?: string | null },
  ) {
    super(message);
    this.name = "WikiError";
    this.status = status;
    this.code = opts?.code;
    this.rev = opts?.rev;
    this.author = opts?.author;
    this.at = opts?.at;
  }
}

/** Map a refused response's body into the error the UI shows. */
export function wikiErrorFrom(status: number, body: unknown): WikiError {
  const b = (body ?? {}) as Record<string, unknown>;
  const message =
    typeof b.error === "string" && b.error
      ? b.error
      : typeof b.message === "string" && b.message
        ? b.message
        : `${status} response`;
  const str = (v: unknown) => (typeof v === "string" ? v : undefined);
  return new WikiError(message, status, {
    code: str(b.code),
    rev: str(b.rev) ?? str(b.current_rev) ?? str(b.revision),
    author: str(b.author) ?? str(b.edited_by) ?? null,
    at: str(b.at) ?? str(b.mtime) ?? null,
  });
}

async function json<T>(resp: Response): Promise<T> {
  const body = await resp.json().catch(() => null);
  if (!resp.ok) throw wikiErrorFrom(resp.status, body);
  // RPC conflicts are successful envelopes carrying a refused write.
  // Never clear a draft or report success for that envelope.
  if (body?.conflict === "if_rev") throw wikiErrorFrom(409, { ...body, error: "This page changed since you opened it", code: "conflict" });
  // A blob route streams bytes, not JSON: a caller that asked for a page
  // where a blob lives gets a clear refusal, not a null that renders as a
  // crash. The preview panes never call this — they read the listing.
  if (body === null) throw new WikiError("the server answered without JSON", resp.status);
  return body as T;
}

/** The metadata a preview needs, taken from a listing entry (no fetch). */
export function blobPage(entry: WikiEntry): WikiPage {
  return {
    path: entry.path,
    kind: "file",
    mime: entry.mime ?? null,
    size: entry.size,
    rev: entry.rev,
    edited_by: entry.edited_by,
    mtime: entry.mtime ?? null,
  };
}

function get<T>(url: string, operation?: PrivateOperation): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(url, { headers: sessionHeaders() }, op);
    const value = await json<T>(resp);
    op.assertCurrent();
    return value;
  });
}

function send<T>(method: "PUT" | "POST", url: string, body: object, operation?: PrivateOperation): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(url, {
      method,
      headers: {
        "Content-Type": "application/json",
        "X-Cadence-Board": "1",
        ...sessionHeaders(),
      },
      body: JSON.stringify(body),
    }, op);
    const value = await json<T>(resp);
    op.assertCurrent();
    return value;
  });
}

export function lsQuery(path: string): string {
  return `/api/wiki/ls?path=${encodeURIComponent(path)}`;
}

export function fileQuery(path: string): string {
  return `/api/wiki/file?path=${encodeURIComponent(path)}`;
}

/**
 * Blob bytes are fetched with this tab's session header, never by element
 * URLs. Callers render data: images or local blob: previews/downloads and
 * must fence their own asynchronous encoding and revoke owned object URLs.
 */
export async function wikiBlob(path: string, signal?: AbortSignal, operation?: PrivateOperation): Promise<Blob> {
  const generation = privateSessionGeneration();
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(fileQuery(path), { headers: sessionHeaders(), signal }, op);
    const value = resp.ok ? await resp.blob() : await resp.json().catch(() => null);
    op.assertCurrent();
    if (signal?.aborted || generation !== privateSessionGeneration() || !privateReadsAllowed()) {
      throw new DOMException("The private session changed", "AbortError");
    }
    if (!resp.ok) throw wikiErrorFrom(resp.status, value);
    return value as Blob;
  }, signal);
}

export function searchQuery(query: string, type?: string, path?: string): string {
  const params = new URLSearchParams({ q: query });
  if (type && type !== "all") params.set("type", type);
  if (path) params.set("path", path);
  return `/api/wiki/search?${params.toString()}`;
}

export function historyQuery(path: string, from?: string, to?: string): string {
  const params = new URLSearchParams({ path });
  if (from) params.set("from", from);
  if (to) params.set("to", to);
  return `/api/wiki/history?${params.toString()}`;
}

export function uploadTargetPath(dir: string, filename: string): string {
  return joinPath(dir, filename);
}

export function uploadQuery(path: string): string {
  return `/api/wiki/upload?path=${encodeURIComponent(path)}`;
}

export const mkdirBody = (path: string) => ({ path });
export const mvBody = (from: string, to: string) => ({ from, to });
export const rmBody = (path: string) => ({ path });

export const wiki = {
  ls: (path: string) => get<Parameters<typeof normalizeListing>[0]>(lsQuery(path)).then(normalizeListing),
  file: (path: string) => get<Parameters<typeof normalizePage>[0]>(fileQuery(path)).then(normalizePage),
  save: (path: string, text: string, ifRev: string) =>
    send<WikiPage>("PUT", "/api/wiki/file", { path, text, if_rev: ifRev }),
  mkdir: (path: string) => send<WikiListing>("POST", "/api/wiki/mkdir", mkdirBody(path)),
  mv: (from: string, to: string) => send<WikiListing>("POST", "/api/wiki/mv", mvBody(from, to)),
  rm: (path: string) => send<{ path: string; trash?: string }>("POST", "/api/wiki/rm", rmBody(path)),
  search: (query: string, type?: string, path?: string) => get<Parameters<typeof normalizeSearch>[0]>(searchQuery(query, type, path)).then(normalizeSearch),
  history: (path: string, from?: string, to?: string) =>
    get<Parameters<typeof normalizeHistory>[0]>(historyQuery(path, from, to)).then(normalizeHistory),
  restore: (path: string, rev: string) =>
    send<WikiPage>("POST", "/api/wiki/restore", { path, rev }),
};

/**
 * One upload, with progress (fetch cannot report it). The file part is
 * `file`; the target folder travels in the query and as a `path` field,
 * so either convention the server settles on works. The server's own
 * refusal — size, MIME, secret scan — surfaces as this file's error.
 */
export interface UploadResult {
  extraction?: { status: string; reason?: string; path?: string };
}

function recordField(body: object, key: string): unknown {
  if (!Object.prototype.hasOwnProperty.call(body, key)) return undefined;
  return Reflect.get(body, key);
}

function textField(body: object, key: string): string | undefined {
  const value = recordField(body, key);
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

/** Local `{error, code}` and hosted `{error:{code,message}}` bodies, guarded. */
function uploadRefusalBody(body: unknown): unknown {
  if (body === null || typeof body !== "object" || Array.isArray(body)) return body;
  const error = recordField(body, "error");
  if (typeof error === "string" || error === undefined) return body;
  if (error === null || typeof error !== "object" || Array.isArray(error)) return { error: undefined, code: textField(body, "code") };
  return {
    error: textField(error, "message"),
    code: textField(body, "code") ?? textField(error, "code"),
  };
}

function uploadResultFrom(body: unknown): UploadResult | { conflict: "if_rev" } {
  if (body === null || typeof body !== "object" || Array.isArray(body)) return {};
  if (textField(body, "conflict") === "if_rev") return { conflict: "if_rev" };
  const extraction = recordField(body, "extraction");
  if (extraction === null || typeof extraction !== "object" || Array.isArray(extraction)) return {};
  const status = textField(extraction, "status");
  if (status === undefined) return {};
  return {
    extraction: {
      status,
      reason: textField(extraction, "reason"),
      path: textField(extraction, "path"),
    },
  };
}

function sendUpload(
  dir: string,
  file: File,
  onProgress: ((percent: number) => void) | undefined,
  operation: PrivateOperation,
): Promise<UploadResult> {
  return new Promise((resolve, reject) => {
    if (operation.signal.aborted) {
      reject(new DOMException("upload canceled", "AbortError"));
      return;
    }
    const path = uploadTargetPath(dir, file.name);
    const form = new FormData();
    form.append("path", path);
    form.append("file", file, file.name);
    const xhr = new XMLHttpRequest();
    let settled = false;
    const onAbort = () => xhr.abort();
    operation.signal.addEventListener("abort", onAbort, { once: true });
    const detach = () => {
      operation.signal.removeEventListener("abort", onAbort);
      xhr.onload = null;
      xhr.onerror = null;
      xhr.onabort = null;
      xhr.upload.onprogress = null;
    };
    const settle = (done: () => void) => {
      if (settled) return;
      settled = true;
      detach();
      done();
    };
    xhr.open("POST", uploadQuery(path));
    xhr.setRequestHeader("X-Cadence-Board", "1");
    for (const [key, value] of Object.entries(sessionHeaders())) xhr.setRequestHeader(key, value);
    xhr.upload.onprogress = (event) => {
      if (settled || operation.signal.aborted || !event.lengthComputable || !onProgress) return;
      onProgress((event.loaded / event.total) * 100);
    };
    xhr.onload = () => {
      const status = xhr.status;
      const responseText = xhr.responseText;
      const stillCurrent = !operation.signal.aborted;
      let body: unknown = null;
      try {
        body = JSON.parse(responseText);
      } catch {
        body = null;
      }
      settle(() => {
        if (!stillCurrent) {
          reject(new DOMException("upload canceled", "AbortError"));
          return;
        }
        if (status >= 200 && status < 300) {
          const result = uploadResultFrom(body);
          if ("conflict" in result) {
            reject(wikiErrorFrom(409, { error: "This file changed during upload", code: "conflict" }));
            return;
          }
          onProgress?.(100);
          resolve(result);
          return;
        }
        if (status === 401) notifyPrivateSessionRequired(status, body, operation);
        reject(wikiErrorFrom(status, uploadRefusalBody(body)));
      });
    };
    xhr.onerror = () => {
      settle(() => reject(new WikiError("upload failed — connection lost", 0)));
    };
    xhr.onabort = () => {
      settle(() => reject(new DOMException("upload canceled", "AbortError")));
    };
    xhr.send(form);
  });
}

export function uploadFile(
  dir: string,
  file: File,
  onProgress?: (percent: number) => void,
  operation?: PrivateOperation,
): Promise<UploadResult> {
  if (operation) return sendUpload(dir, file, onProgress, operation);
  return withPrivateOperation((owned) => sendUpload(dir, file, onProgress, owned));
}
