import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, privateReadsAllowed, privateSessionGeneration, usingPrivateOperation, type PrivateOperation } from "../../lib/privateSession";
import type { WikiVersion } from "./history";
import type { SearchHit } from "./search";
import { normalizeListing, normalizePage, normalizeSearch, normalizeHistory, normalizeTrashLandings, normalizeTrashItems } from "./wire";

/**
 * The wiki store's HTTP surface (CAD-580) as this screen reads it. One
 * module owns requests; wire.ts adapts the daemon’s response shapes:
 *
 *   GET  /api/wiki/ls?path=       a folder's entries — or one entry's kind
 *   GET  /api/wiki/file?path=     text as JSON; blobs streamed (Range)
 *   PUT  /api/wiki/file           {path, text, if_rev}
 *   POST /api/wiki/transfer       the durable import job (see transfer.ts)
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
  /** Hosted blobs: the Worker's extraction state (pending, indexed, empty, failed, unsupported). */
  status?: string;
  status_reason?: string;
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
  status?: string;
  status_reason?: string;
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
  status?: string;
  status_reason?: string;
  /** Set when this page was read with an explicit `at` selector. */
  historical?: boolean;
}

export interface WikiSearchResponse {
  hits?: SearchHit[];
  results?: SearchHit[];
  /**
   * The hosted Worker's fallback disclosure: no page matched every term, so
   * the hits match some of them. Absent on the native daemon, which does not
   * fall back, and never invented here.
   */
  partial?: boolean;
}

export interface WikiHistoryResponse {
  path: string;
  rev?: string;
  entries?: WikiVersion[];
  history?: WikiVersion[];
  diff?: string;
  canCompare?: boolean;
  /** CAD-1915: false when the server cannot read a non-current revision (hosted Worker log). */
  canOpen?: boolean;
}

/**
 * One deleted landing, as the operator-only listing answers it (CAD-2018).
 *
 * `landing_id` is the board's row identity and is always present. `from` is
 * the exact opaque `.trash/…` value the board accepts as a restore source;
 * it is null when the board cannot prove the landing's exact origin — a
 * native landing whose root predates the fixed layout. `path` is the
 * original logical wiki path, null for the same reason. A null `from` is
 * never turned into a `.trash/…` string and never asked for items.
 *
 * `kind`, `item_count`, `size`, `deleted_at`, `actor` and `reason` are null only
 * when the board cannot prove them — the screen never fills a gap in.
 */
export interface TrashLanding {
  landing_id: string;
  from: string | null;
  path: string | null;
  kind: WikiKind | null;
  deleted_at: number | null;
  actor: string | null;
  item_count: number | null;
  size: number | null;
  restorable: boolean;
  reason: string | null;
}

export interface TrashLandings {
  landings: TrashLanding[];
  next_cursor: string | null;
}

/** One path inside a landing: original logical path, no contents. */
export interface TrashEntry {
  path: string;
  kind: WikiKind;
  size: number | null;
}

export interface TrashItems {
  from: string;
  entries: TrashEntry[];
  next_cursor: string | null;
}

/** Both trash routes default to 50 rows and refuse more than 100. */
export const TRASH_PAGE = 50;

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

/**
 * The one listing answer that means "this folder does not exist yet", for the
 * project Context's first-time empty state. Both hosts are recognised by the
 * shape they actually send, and nothing else is:
 *
 * - hosted Worker: `404` with `code: "not_found"` (`wiki ls` only answers it
 *   for a folder that has neither a row nor a descendant);
 * - native daemon: `400` whose message is exactly its missing-directory
 *   refusal for this path — the daemon's other refusals (a file, an
 *   unreadable path, a lock) keep their own text and are not this case.
 *
 * A 403, a 5xx, a network failure or a message that merely mentions a
 * directory stays an error.
 */
export function isMissingWikiFolder(error: unknown, path: string): boolean {
  if (!(error instanceof WikiError)) return false;
  if (error.status === 404 && error.code === "not_found") return true;
  return error.status === 400 && error.message === `wiki ls '${path}': no such directory`;
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
    status: entry.status,
    status_reason: entry.status_reason,
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

/** `GET /api/wiki/trash` — operator-only landings in the board's stable
 *  order. `cursor` is the board's opaque token, never rebuilt here. */
export function trashQuery(cursor?: string | null, limit = TRASH_PAGE): string {
  const params = new URLSearchParams({ limit: String(limit) });
  if (cursor) params.set("cursor", cursor);
  return `/api/wiki/trash?${params.toString()}`;
}

/** `GET /api/wiki/trash/items` — one landing's original paths, no contents. */
export function trashItemsQuery(from: string, cursor?: string | null, limit = TRASH_PAGE): string {
  const params = new URLSearchParams({ from, limit: String(limit) });
  if (cursor) params.set("cursor", cursor);
  // Native query parsing percent-decodes, but does not interpret '+' as a
  // space. Preserve literal pluses (%2B) and spell spaces as %20 for both hosts.
  return `/api/wiki/trash/items?${params.toString().replaceAll("+", "%20")}`;
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
  /** CAD-2018: the operator's deleted landings. A board without the route
   *  answers 404, which the screen reports as "not available here". */
  trash: (cursor?: string | null) =>
    get<Parameters<typeof normalizeTrashLandings>[0]>(trashQuery(cursor)).then(normalizeTrashLandings),
  trashItems: (from: string, cursor?: string | null) =>
    get<Parameters<typeof normalizeTrashItems>[0]>(trashItemsQuery(from, cursor))
      .then((value) => normalizeTrashItems(value, from)),
};
