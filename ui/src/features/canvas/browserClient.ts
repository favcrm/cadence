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

/** `GET /api/browser` for a deployment that has a browser. */
export interface BrowserSession {
  state: BrowserPhase;
  signedIn: boolean;
  /** The page the browser is on, as the board reports it. */
  url: string | null;
  /** The model's words to the person during a takeover — untrusted text. */
  instructions: string | null;
}

/** `unavailable` is `{state: "unavailable"}` or a 404 from a board that
 *  predates the routes: either way the board offers no Browser tab. */
export type BrowserRead = { kind: "unavailable" } | { kind: "session"; session: BrowserSession };

const PHASES: readonly string[] = ["none", "active", "handoff", "ended"];

/** Parse a `GET /api/browser` body; null when it is not the contract's shape,
 *  so a malformed answer is an error and never an invented state. */
export function parseBrowserRead(body: unknown): BrowserRead | null {
  if (typeof body !== "object" || body === null) return null;
  const { state, signedIn, url, instructions } = body as Record<string, unknown>;
  if (state === "unavailable") return { kind: "unavailable" };
  if (typeof state !== "string" || !PHASES.includes(state)) return null;
  if (signedIn !== undefined && signedIn !== null && typeof signedIn !== "boolean") return null;
  if (url !== undefined && url !== null && typeof url !== "string") return null;
  if (instructions !== undefined && instructions !== null && typeof instructions !== "string") return null;
  return {
    kind: "session",
    session: {
      state: state as BrowserPhase,
      signedIn: signedIn === true,
      url: typeof url === "string" && url !== "" ? url : null,
      instructions: typeof instructions === "string" && instructions !== "" ? instructions : null,
    },
  };
}

const route = (path: string, conversation: string): string =>
  `/api/browser${path}?conversation=${encodeURIComponent(conversation)}`;

/** Read the session. Throws on a network error, a refusal or a malformed body. */
export function readBrowser(conversation: string, signal?: AbortSignal): Promise<BrowserRead> {
  return usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(route("", conversation), { headers: sessionHeaders(), cache: "no-store", signal }, op);
    if (resp.status === 404) return { kind: "unavailable" } as const;
    if (!resp.ok) throw new Error(`${resp.status} ${resp.statusText}`);
    const body: unknown = await resp.json();
    op.assertCurrent();
    const read = parseBrowserRead(body);
    if (!read) throw new Error("Unreadable browser state");
    return read;
  });
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
  return usingPrivateOperation(undefined, async (op) => {
    const resp = await privateFetch(route("/view", conversation), { method: "POST", headers: sessionHeaders(), cache: "no-store" }, op);
    if (resp.status === 409) return { kind: "not_in_handoff" } as const;
    if (!resp.ok) throw new Error(`${resp.status} ${resp.statusText}`);
    const body: unknown = await resp.json();
    op.assertCurrent();
    const url = httpsUrl(typeof body === "object" && body !== null ? (body as { url?: unknown }).url : undefined);
    if (!url) throw new Error("Unreadable live view answer");
    return { kind: "url", url } as const;
  });
}
