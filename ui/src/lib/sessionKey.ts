/**
 * The operator session's second credential (CAD-313, review round 2 of
 * PR #249). The board's HttpOnly cookie alone is no session: every
 * request also carries `X-Cadence-Session: <key>`. The key is scoped to
 * the exact origin, port included — so a server on another port that
 * receives the cookie (cookies ignore ports) never sees it, and a
 * cross-site page cannot set the header.
 *
 * It lives in `localStorage` so the same browser (a new tab, a reload,
 * or a browser restart) keeps the session the cookie already carries;
 * `sessionStorage` is read as a fallback so a tab opened before this
 * change still works, and written too so a store that blocks
 * `localStorage` degrades to the old per-tab behavior instead of
 * signing out. Clearing the key is what ends this browser's session.
 */

const STORE = "cadence.sessionKey";
export const SESSION_HEADER = "X-Cadence-Session";

export function sessionKey(): string | null {
  // `globalThis` rather than `window` so a storage shim (tests, embedded
  // hosts) works the same way; a blocked or absent store throws and is
  // skipped.
  for (const kind of ["local", "session"] as const) {
    try {
      const key = (kind === "local" ? globalThis.localStorage : globalThis.sessionStorage).getItem(STORE);
      if (key) return key;
    } catch {
      // Blocked or absent store: fall through to the next one.
    }
  }
  return null;
}

export function setSessionKey(key: string | null): void {
  const previous = sessionKey();
  for (const kind of ["local", "session"] as const) {
    try {
      const storage = kind === "local" ? globalThis.localStorage : globalThis.sessionStorage;
      if (key) storage.setItem(STORE, key);
      else storage.removeItem(STORE);
    } catch {
      // Storage blocked: the tab simply stays signed out.
    }
  }
  if (previous !== sessionKey() && typeof window !== "undefined") {
    window.dispatchEvent(new Event("cadence:session-changed"));
  }
}

/** The header to add to a board request, when this browser holds a key. */
export function sessionHeaders(): Record<string, string> {
  const key = sessionKey();
  return key ? { [SESSION_HEADER]: key } : {};
}
