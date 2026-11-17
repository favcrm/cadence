/**
 * A non-secret, per-browser memory of "this browser has signed in before".
 * It decides whether a signed-out hosted gate says "Session ended" instead of
 * a first-visit welcome, and whether a resume is worth attempting. It is never
 * sent anywhere, holds no credential, and is cleared on explicit sign-out or a
 * definitive refusal.
 */
const PRESENCE_STORE = "cadence.hostedPresence";

/** In-memory fallback so a blocked or absent localStorage still behaves. */
let seen = false;

export function hostedPresence(): boolean {
  try {
    if (globalThis.localStorage?.getItem(PRESENCE_STORE) === "1") return true;
  } catch {
    // Blocked or absent store: fall through to memory.
  }
  return seen;
}

export function markHostedPresence(): void {
  seen = true;
  try {
    globalThis.localStorage?.setItem(PRESENCE_STORE, "1");
  } catch {
    // Memory keeps this tab's behavior.
  }
}

export function clearHostedPresence(): void {
  seen = false;
  try {
    globalThis.localStorage?.removeItem(PRESENCE_STORE);
  } catch {
    // Nothing durable to clear.
  }
}
