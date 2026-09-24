import { ApiError } from "../../lib/api";
import { sessionHeaders, setSessionKey } from "../../lib/sessionKey";
import { parseLoginNonce } from "./gate";

/**
 * The login link's nonce, taken out of the address bar before anything
 * renders: the fragment is stripped at once (`history.replaceState`), so
 * it is not left in the URL, the history entry or a screenshot. Read
 * once by the login screen.
 */
let captured: string | null = null;

export function captureLoginNonce(): void {
  if (location.pathname !== "/login" || !location.hash) return;
  captured = parseLoginNonce(location.hash);
  history.replaceState(null, "", "/login");
}

export function takeLoginNonce(): string | null {
  const nonce = captured;
  captured = null;
  return nonce;
}

async function sessionPost(path: string, body: object): Promise<unknown> {
  const resp = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
    body: JSON.stringify(body),
  });
  const parsed = await resp.json().catch(() => null);
  if (!resp.ok) {
    throw new ApiError(parsed?.error ?? `${resp.status} ${resp.statusText}`, resp.status, parsed ?? undefined);
  }
  return parsed;
}

/**
 * `POST /api/session` — exchange the nonce for the session: the server
 * sets the HttpOnly cookie and answers `{session_key}`, which this tab
 * keeps in `sessionStorage` and sends on every request.
 */
export async function openSession(nonce: string): Promise<void> {
  const out = (await sessionPost("/api/session", { nonce })) as { session_key?: unknown } | null;
  const key = typeof out?.session_key === "string" ? out.session_key : null;
  if (!key) throw new ApiError("the board answered no session key — upgrade the board", 500);
  setSessionKey(key);
}

/** `POST /api/session/logout` — end this browser's session. */
export async function closeSession(): Promise<void> {
  try {
    await sessionPost("/api/session/logout", {});
  } finally {
    setSessionKey(null);
  }
}
