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

export const SESSION_KEY_STORE = "cadence.sessionKey";
export const SESSION_HEADER = "X-Cadence-Session";

export function sessionKey(): string | null {
  // `globalThis` rather than `window` so a storage shim (tests, embedded
  // hosts) works the same way; a blocked or absent store throws and is
  // skipped.
  for (const kind of ["local", "session"] as const) {
    try {
      const key = (kind === "local" ? globalThis.localStorage : globalThis.sessionStorage).getItem(SESSION_KEY_STORE);
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
      if (key) storage.setItem(SESSION_KEY_STORE, key);
      else storage.removeItem(SESSION_KEY_STORE);
    } catch {
      // Storage blocked: the tab simply stays signed out.
    }
  }
  if (previous !== sessionKey() && typeof window !== "undefined") {
    window.dispatchEvent(new Event("cadence:session-changed"));
  }
}

/**
 * Apply a `storage` event for the shared key from another tab. The browser
 * has already updated `localStorage`; a removal must also drop this tab's
 * per-tab fallback so it cannot resurrect the credential, and a replacement
 * must drop that fallback so it cannot shadow the new value. Returns true
 * when the event concerns this credential.
 */
export function applySharedSessionChange(event: Pick<StorageEvent, "key" | "newValue">): boolean {
  if (event.key !== null && event.key !== SESSION_KEY_STORE) return false;
  // The event says the shared store changed, but its `newValue` can be
  // historical: a suspended tab may process an old removal after another
  // tab has already signed in again. Never write the event back. Drop only
  // this tab's per-tab fallback; the caller re-reads the current durable
  // value and verifies that.
  try {
    globalThis.sessionStorage.removeItem(SESSION_KEY_STORE);
  } catch {
    // Blocked store: nothing to drop.
  }
  return true;
}

/** The header to add to a board request, when this browser holds a key. */
export function sessionHeaders(): Record<string, string> {
  const key = sessionKey();
  return key ? { [SESSION_HEADER]: key } : {};
}
