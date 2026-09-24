import { ApiError } from "../../lib/api";
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

async function sessionPost(path: string, body: object): Promise<void> {
  const resp = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Cadence-Board": "1" },
    body: JSON.stringify(body),
  });
  if (!resp.ok) {
    const parsed = await resp.json().catch(() => null);
    throw new ApiError(parsed?.error ?? `${resp.status} ${resp.statusText}`, resp.status, parsed ?? undefined);
  }
}

/** `POST /api/session` — exchange the nonce for the session cookie. */
export const openSession = (nonce: string) => sessionPost("/api/session", { nonce });

/** `POST /api/session/logout` — end this browser's session. */
export const closeSession = () => sessionPost("/api/session/logout", {});
