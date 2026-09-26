import { sessionHeaders } from "../../lib/sessionKey";
import type { WikiVersion } from "./history";
import type { SearchHit } from "./search";

/**
 * The wiki store's HTTP surface (CAD-580) as this screen reads it. One
 * file owns the shapes and the request builders, so a contract change is
 * a one-file edit (the questions CAD-581 posted on CAD-580 are answered
 * there; the builders below are the assumptions until they land):
 *
 *   GET  /api/wiki/ls?path=       a folder's entries — or one entry's kind
 *   GET  /api/wiki/file?path=     text as JSON; blobs streamed (Range)
 *   PUT  /api/wiki/file           {path, text, if_rev}
 *   POST /api/wiki/upload         multipart, one file per request
 *   POST /api/wiki/mkdir|mv|rm    {path}, {path, to}, {path}
 *   GET  /api/wiki/search?q=      full-text search
 *   GET  /api/wiki/history?path=  git log; &from=&to= for one diff
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
  return body as T;
}

function get<T>(url: string): Promise<T> {
  return fetch(url, { headers: sessionHeaders() }).then(json<T>);
}

function send<T>(method: "PUT" | "POST", url: string, body: object): Promise<T> {
  return fetch(url, {
    method,
    headers: {
      "Content-Type": "application/json",
      "X-Cadence-Board": "1",
      ...sessionHeaders(),
    },
    body: JSON.stringify(body),
  }).then(json<T>);
}

export function lsQuery(path: string): string {
  return `/api/wiki/ls?path=${encodeURIComponent(path)}`;
}

export function fileQuery(path: string): string {
  return `/api/wiki/file?path=${encodeURIComponent(path)}`;
}

/** The blob URL for `<img>`/`<video>`/`<iframe>` — the cookie authenticates. */
export function wikiFileUrl(path: string): string {
  return fileQuery(path);
}

/**
 * The raw bytes of a text page for the Download action: the file route
 * answers JSON by default (CAD-580), and `raw=1` asks for the file
 * itself. A contract question is open on CAD-580; if it settles on
 * another flag this is the one place to change.
 */
export function wikiRawUrl(path: string): string {
  return `${fileQuery(path)}&raw=1`;
}

export function searchQuery(query: string, type?: string): string {
  const params = new URLSearchParams({ q: query });
  if (type && type !== "all") params.set("type", type);
  return `/api/wiki/search?${params.toString()}`;
}

export function historyQuery(path: string, from?: string, to?: string): string {
  const params = new URLSearchParams({ path });
  if (from) params.set("from", from);
  if (to) params.set("to", to);
  return `/api/wiki/history?${params.toString()}`;
}

export function uploadQuery(dir: string): string {
  return `/api/wiki/upload?path=${encodeURIComponent(dir)}`;
}

export const mkdirBody = (path: string) => ({ path });
export const mvBody = (from: string, to: string) => ({ path: from, to });
export const rmBody = (path: string) => ({ path });

export const wiki = {
  ls: (path: string) => get<WikiListing>(lsQuery(path)),
  file: (path: string) => get<WikiPage>(fileQuery(path)),
  save: (path: string, text: string, ifRev: string) =>
    send<WikiPage>("PUT", "/api/wiki/file", { path, text, if_rev: ifRev }),
  mkdir: (path: string) => send<WikiListing>("POST", "/api/wiki/mkdir", mkdirBody(path)),
  mv: (from: string, to: string) => send<WikiListing>("POST", "/api/wiki/mv", mvBody(from, to)),
  rm: (path: string) => send<{ path: string; trash?: string }>("POST", "/api/wiki/rm", rmBody(path)),
  search: (query: string, type?: string) => get<WikiSearchResponse>(searchQuery(query, type)),
  history: (path: string, from?: string, to?: string) =>
    get<WikiHistoryResponse>(historyQuery(path, from, to)),
  restore: (path: string, rev: string) =>
    send<WikiPage>("POST", "/api/wiki/restore", { path, rev }),
};

/**
 * One upload, with progress (fetch cannot report it). The file part is
 * `file`; the target folder travels in the query and as a `path` field,
 * so either convention the server settles on works. The server's own
 * refusal — size, MIME, secret scan — surfaces as this file's error.
 */
export function uploadFile(
  dir: string,
  file: File,
  onProgress?: (percent: number) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const form = new FormData();
    form.append("path", dir);
    form.append("file", file, file.name);
    const xhr = new XMLHttpRequest();
    xhr.open("POST", uploadQuery(dir));
    xhr.setRequestHeader("X-Cadence-Board", "1");
    for (const [key, value] of Object.entries(sessionHeaders())) xhr.setRequestHeader(key, value);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable && onProgress) onProgress((e.loaded / e.total) * 100);
    };
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        onProgress?.(100);
        resolve();
        return;
      }
      let body: unknown = null;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {
        body = null;
      }
      reject(wikiErrorFrom(xhr.status, body));
    };
    xhr.onerror = () => reject(new WikiError("upload failed — connection lost", 0));
    xhr.send(form);
  });
}
