/**
 * The operator session's second credential (CAD-313, review round 2 of
 * PR #249). The board's HttpOnly cookie alone is no session: every
 * request also carries `X-Cadence-Session: <key>`. The key lives in
 * THIS tab's `sessionStorage` — scoped to the exact origin, port
 * included — so a server on another port that receives the cookie
 * (cookies ignore ports) never sees it, and a cross-site page cannot
 * set the header. It survives a reload; a new tab signs in again.
 */

const STORE = "cadence.sessionKey";
export const SESSION_HEADER = "X-Cadence-Session";

export function sessionKey(): string | null {
  try {
    return sessionStorage.getItem(STORE);
  } catch {
    return null;
  }
}

export function setSessionKey(key: string | null): void {
  try {
    if (key) sessionStorage.setItem(STORE, key);
    else sessionStorage.removeItem(STORE);
  } catch {
    // Storage blocked: the tab simply stays signed out.
  }
}

/** The header to add to a board request, when this tab holds a key. */
export function sessionHeaders(): Record<string, string> {
  const key = sessionKey();
  return key ? { [SESSION_HEADER]: key } : {};
}
