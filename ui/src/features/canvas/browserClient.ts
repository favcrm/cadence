import { ApiError, errorMessage } from "../../lib/apiError";
import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, usingPrivateOperation } from "../../lib/privateSession";

/**
 * CAD-2103: the session-backed browser, through the tenant board's own
 * routes (AgenticOS contract, agenticos-v2#1706). All three take the
 * conversation in view as `?conversation=`. Nothing here logs, caches or
 * keeps a viewer URL: `viewBrowser` hands it to the caller once.
 */

export type BrowserPhase = "none" | "active" | "handoff" | "ended";

export interface BrowserQuota {
  active: number;
  limit: number;
}

/** `GET /api/browser` for a deployment that has a browser. */
export interface BrowserSession {
  state: BrowserPhase;
  signedIn: boolean;
  /** The page the browser is on, as the board reports it. */
  url: string | null;
  /** The model's words to the person during a takeover — untrusted text. */
  instructions: string | null;
  /** Optional workspace concurrency snapshot when provided by the board. */
  quota?: BrowserQuota | null;
}

/** `unavailable` is `{state: "unavailable"}` or a 404 from a board that
 *  predates the routes: either way the board offers no Browser tab. */
export type BrowserRead = { kind: "unavailable" } | { kind: "session"; session: BrowserSession };

const PHASES: readonly string[] = ["none", "active", "handoff", "ended"];

function parseQuota(value: unknown): BrowserQuota | null {
  if (typeof value !== "object" || value === null) return null;
  const { active, limit } = value as Record<string, unknown>;
  if (
    typeof active !== "number" ||
    typeof limit !== "number" ||
    !Number.isSafeInteger(active) ||
    !Number.isSafeInteger(limit) ||
    active < 0 ||
    limit < 1
  ) {
    return null;
  }
  return { active, limit };
}

/** Parse a `GET /api/browser` body; null when it is not the contract's shape,
 *  so a malformed answer is an error and never an invented state. */
export function parseBrowserRead(body: unknown): BrowserRead | null {
  if (typeof body !== "object" || body === null) return null;
  const { state, signedIn, url, instructions, quota } = body as Record<string, unknown>;
  if (state === "unavailable") return { kind: "unavailable" };
  if (typeof state !== "string" || !PHASES.includes(state)) return null;
  if (signedIn !== undefined && signedIn !== null && typeof signedIn !== "boolean") return null;
  if (url !== undefined && url !== null && typeof url !== "string") return null;
  if (instructions !== undefined && instructions !== null && typeof instructions !== "string") return null;
  const parsedQuota = parseQuota(quota);
  return {
    kind: "session",
    session: {
      state: state as BrowserPhase,
      signedIn: signedIn === true,
      url: typeof url === "string" && url !== "" ? url : null,
      instructions: typeof instructions === "string" && instructions !== "" ? instructions : null,
      ...(parsedQuota ? { quota: parsedQuota } : {}),
    },
  };
}

/** A read or a view request that never answers must not stall polling for
 *  good: it is aborted after this long and reads as a failure. Stop is not
 *  bounded: a hung page can take minutes to stop. */
const READ_TIMEOUT_MS = 10_000;
const VIEW_TIMEOUT_MS = 15_000;

async function bounded<T>(ms: number, run: (signal: AbortSignal) => Promise<T>): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), ms);
  try {
    return await run(controller.signal);
  } finally {
    clearTimeout(timer);
  }
}

const route = (path: string, conversation: string): string =>
  `/api/browser${path}?conversation=${encodeURIComponent(conversation)}`;

/** Read the session. Throws on a network error, a refusal or a malformed body. */
export function readBrowser(conversation: string): Promise<BrowserRead> {
  return bounded(READ_TIMEOUT_MS, (signal) => usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(route("", conversation), { headers: sessionHeaders(), cache: "no-store", signal }, op);
    if (resp.status === 404) return { kind: "unavailable" } as const;
    if (!resp.ok) throw new Error(`${resp.status} ${resp.statusText}`);
    const body: unknown = await resp.json();
    op.assertCurrent();
    const read = parseBrowserRead(body);
    if (!read) throw new Error("Unreadable browser state");
    return read;
  }));
}

/** `POST /api/browser/stop`. Resolves only on `{status: "stopped"}`; a 503
 *  ("the provider could not stop it") and every other answer throw. */
export function stopBrowser(conversation: string): Promise<void> {
  return usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(route("/stop", conversation), { method: "POST", headers: sessionHeaders(), cache: "no-store" }, op);
    const body: unknown = await resp.json().catch(() => null);
    op.assertCurrent();
    if (!resp.ok) throw new ApiError(errorMessage(body, `${resp.status} ${resp.statusText}`), resp.status);
    if (typeof body !== "object" || body === null || (body as { status?: unknown }).status !== "stopped") {
      throw new Error("Unreadable stop answer");
    }
  });
}

export type ViewRead = { kind: "url"; url: string } | { kind: "not_in_handoff" };

/** An `https:` URL on another origin, or null. A frame must never be pointed at
 *  `javascript:`, `data:` or a plain-http page, nor at the board's own origin:
 *  the frame is allowed scripts and its own origin, which for the board's would
 *  be the board. Whatever the answer said. */
function httpsUrl(value: unknown): string | null {
  if (typeof value !== "string") return null;
  try {
    const url = new URL(value);
    if (typeof location !== "undefined" && url.origin === location.origin) return null;
    return url.protocol === "https:" ? value : null;
  } catch {
    return null;
  }
}

/**
 * `POST /api/browser/view`: the live view's URL, a bearer credential valid
 * for minutes to start one connection. Called only on the person's action;
 * the caller holds it in memory for the frame and nowhere else.
 */
export function viewBrowser(conversation: string): Promise<ViewRead> {
  return bounded(VIEW_TIMEOUT_MS, (signal) => usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(route("/view", conversation), { method: "POST", headers: sessionHeaders(), cache: "no-store", signal }, op);
    if (resp.status === 409) return { kind: "not_in_handoff" } as const;
    if (!resp.ok) throw new Error(`${resp.status} ${resp.statusText}`);
    const body: unknown = await resp.json();
    op.assertCurrent();
    const url = httpsUrl(typeof body === "object" && body !== null ? (body as { url?: unknown }).url : undefined);
    if (!url) throw new Error("Unreadable live view answer");
    return { kind: "url", url } as const;
  }));
}

export type StartBrowserResult =
  | { kind: "session"; session: BrowserSession }
  | { kind: "limit"; quota?: BrowserQuota | null }
  | { kind: "unsupported" };

/** `POST /api/browser/start`: user-initiated start into human handoff (`human_used=1`). */
export function startBrowser(conversation: string, url?: string): Promise<StartBrowserResult> {
  return bounded(VIEW_TIMEOUT_MS, (signal) => usingPrivateOperation(undefined, async (op) => {
    const trimmed = url?.trim();
    const init: RequestInit = {
      method: "POST",
      headers: { ...sessionHeaders(), ...(trimmed ? { "Content-Type": "application/json" } : {}) },
      cache: "no-store",
      signal,
      ...(trimmed ? { body: JSON.stringify({ url: trimmed }) } : {}),
    };
    const resp = await privateFetch(route("/start", conversation), init, op);
    if (resp.status === 404 || resp.status === 405) return { kind: "unsupported" } as const;
    const body: unknown = await resp.json().catch(() => null);
    op.assertCurrent();
    if (resp.status === 429) {
      const errObj = typeof body === "object" && body !== null ? (body as { error?: unknown }).error : null;
      const quota = typeof errObj === "object" && errObj !== null ? parseQuota((errObj as { quota?: unknown }).quota) : null;
      return { kind: "limit", quota } as const;
    }
    if (!resp.ok) throw new ApiError(errorMessage(body, `${resp.status} ${resp.statusText}`), resp.status);
    const read = parseBrowserRead(body);
    if (!read || read.kind !== "session") throw new Error("Unreadable browser state");
    return { kind: "session", session: read.session } as const;
  }));
}

export type TakeoverBrowserResult =
  | { kind: "session"; session: BrowserSession }
  | { kind: "no_session" }
  | { kind: "unsupported" };

/** `POST /api/browser/handoff`: user-initiated takeover while the assistant is reading. */
export function takeoverBrowser(conversation: string): Promise<TakeoverBrowserResult> {
  return bounded(VIEW_TIMEOUT_MS, (signal) => usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(route("/handoff", conversation), { method: "POST", headers: sessionHeaders(), cache: "no-store", signal }, op);
    if (resp.status === 404 || resp.status === 405) return { kind: "unsupported" } as const;
    if (resp.status === 409) return { kind: "no_session" } as const;
    const body: unknown = await resp.json().catch(() => null);
    op.assertCurrent();
    if (!resp.ok) throw new ApiError(errorMessage(body, `${resp.status} ${resp.statusText}`), resp.status);
    const read = parseBrowserRead(body);
    if (!read || read.kind !== "session") throw new Error("Unreadable browser state");
    return { kind: "session", session: read.session } as const;
  }));
}

export interface BrowserPreview {
  url: string;
  data: string;
  mimeType: "image/jpeg";
}

/** `GET /api/browser/preview`: sanitized read-only JPEG preview while active. */
export function previewBrowser(conversation: string): Promise<BrowserPreview | null> {
  return bounded(READ_TIMEOUT_MS, (signal) => usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(route("/preview", conversation), { headers: sessionHeaders(), cache: "no-store", signal }, op);
    if (resp.status === 204 || resp.status === 404 || resp.status === 405 || resp.status === 409) {
      return null;
    }
    if (!resp.ok) return null;
    const body: unknown = await resp.json().catch(() => null);
    op.assertCurrent();
    if (typeof body !== "object" || body === null) return null;
    const { url, data, mimeType } = body as Record<string, unknown>;
    if (
      typeof url !== "string" ||
      typeof data !== "string" ||
      data.length === 0 ||
      !/^[A-Za-z0-9+/=]+$/.test(data) ||
      mimeType !== "image/jpeg"
    ) {
      return null;
    }
    return { url, data, mimeType: "image/jpeg" };
  }));
}
