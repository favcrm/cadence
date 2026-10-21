/**
 * Client-side "new reply" state (CAD-1700): the last-seen `updated` epoch
 * per conversation, kept in localStorage under one key per install. A
 * conversation is unread when its newest entry (`updated`) is later than
 * what this browser last saw. Opening it writes the seen value; the
 * `storage` event carries that to the other tabs. No server state.
 */

const PREFIX = "cadence.assistant.seen";

export function seenKey(installId: string): string {
  return `${PREFIX}.${installId}`;
}

export type SeenMap = Record<string, number>;

export function readSeen(installId: string): SeenMap {
  try {
    const raw = window.localStorage.getItem(seenKey(installId));
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: SeenMap = {};
    for (const [id, value] of Object.entries(parsed)) {
      if (typeof value === "number" && Number.isFinite(value)) out[id] = value;
    }
    return out;
  } catch {
    return {};
  }
}

export function writeSeen(installId: string, id: string, updated: number): void {
  try {
    const seen = readSeen(installId);
    if ((seen[id] ?? 0) >= updated) return;
    seen[id] = updated;
    window.localStorage.setItem(seenKey(installId), JSON.stringify(seen));
  } catch {
    // Storage unavailable (private mode, quota): the dot simply stays until
    // the conversation is opened in a session that can persist it.
  }
}

/** True when the conversation has a newer entry than this browser has seen. */
export function isUnread(seen: SeenMap, id: string, updated: number): boolean {
  return updated > (seen[id] ?? 0);
}

/**
 * Subscribe to seen-state written by another tab for this install. Returns
 * an unsubscribe. The listener receives the freshly read map.
 */
export function onSeenChanged(installId: string, listener: (seen: SeenMap) => void): () => void {
  const key = seenKey(installId);
  const onStorage = (event: StorageEvent) => {
    if (event.key === key) listener(readSeen(installId));
  };
  window.addEventListener("storage", onStorage);
  return () => window.removeEventListener("storage", onStorage);
}
