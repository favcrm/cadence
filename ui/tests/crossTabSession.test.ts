import { applySharedSessionChange, SESSION_KEY_STORE, sessionKey, setSessionKey } from "../src/lib/sessionKey";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function memoryStorage(): Storage {
  const data = new Map<string, string>();
  return {
    get length() { return data.size; },
    clear() { data.clear(); },
    getItem(key: string) { return data.get(key) ?? null; },
    key(index: number) { return [...data.keys()][index] ?? null; },
    removeItem(key: string) { data.delete(key); },
    setItem(key: string, value: string) { data.set(key, value); },
  };
}

Object.defineProperty(globalThis, "localStorage", { value: memoryStorage(), configurable: true, writable: true });
Object.defineProperty(globalThis, "sessionStorage", { value: memoryStorage(), configurable: true, writable: true });
Object.defineProperty(globalThis, "window", { value: { dispatchEvent: () => true }, configurable: true, writable: true });

// A login writes the durable store and keeps the per-tab fallback for a tab
// that opened before this change, or for a browser that blocks localStorage.
setSessionKey("old");
equal(sessionKey(), "old", "durable key is effective");
equal(localStorage.getItem(SESSION_KEY_STORE), "old", "written to localStorage");
equal(sessionStorage.getItem(SESSION_KEY_STORE), "old", "written to sessionStorage");

// Another tab logs out. The browser has already applied the removal; the
// event must only drop this tab's stale fallback.
sessionStorage.setItem(SESSION_KEY_STORE, "stale");
localStorage.removeItem(SESSION_KEY_STORE);
equal(applySharedSessionChange({ key: SESSION_KEY_STORE, newValue: null }), true, "removal applies");
equal(sessionKey(), null, "removal clears the effective key");
equal(localStorage.getItem(SESSION_KEY_STORE), null, "removal leaves the durable store as the browser set it");
equal(sessionStorage.getItem(SESSION_KEY_STORE), null, "removal clears the fallback");

// A suspended tab can process an old removal after another tab has already
// signed in again. The historical event must never delete the newer key.
localStorage.setItem(SESSION_KEY_STORE, "newer");
sessionStorage.setItem(SESSION_KEY_STORE, "stale");
equal(applySharedSessionChange({ key: SESSION_KEY_STORE, newValue: null }), true, "delayed removal applies");
equal(sessionKey(), "newer", "delayed removal leaves the newer durable key");
equal(localStorage.getItem(SESSION_KEY_STORE), "newer", "delayed removal does not rewrite durable storage");
equal(sessionStorage.getItem(SESSION_KEY_STORE), null, "delayed removal still drops the fallback");

// Another tab replaces the key: the durable value wins and the fallback is
// dropped, so no stale per-tab copy shadows it.
setSessionKey("old");
sessionStorage.setItem(SESSION_KEY_STORE, "stale");
localStorage.setItem(SESSION_KEY_STORE, "new"); // the browser applies it before the event
equal(applySharedSessionChange({ key: SESSION_KEY_STORE, newValue: "new" }), true, "replacement applies");
equal(sessionKey(), "new", "replacement key is effective");
equal(sessionStorage.getItem(SESSION_KEY_STORE), null, "replacement drops the fallback");

// A delayed clear event follows the same rule: inspect the current store,
// never replay the historical clear over a newer login.
localStorage.setItem(SESSION_KEY_STORE, "newer");
sessionStorage.setItem(SESSION_KEY_STORE, "stale");
equal(applySharedSessionChange({ key: null, newValue: null }), true, "delayed clear applies");
equal(sessionKey(), "newer", "delayed clear leaves the newer durable key");

// An actual clear leaves no durable key, and the fallback goes too.
setSessionKey("old");
localStorage.clear();
equal(applySharedSessionChange({ key: null, newValue: null }), true, "clear applies");
equal(sessionKey(), null, "clear removes the key");
equal(sessionStorage.getItem(SESSION_KEY_STORE), null, "clear removes the fallback");

// An unrelated storage key never touches the session.
setSessionKey("old");
equal(applySharedSessionChange({ key: "cadence.other", newValue: "x" }), false, "unrelated key ignored");
equal(sessionKey(), "old", "unrelated key leaves the session");

console.log("cross-tab session checks passed");
