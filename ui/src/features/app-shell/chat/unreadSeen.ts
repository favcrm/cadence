/** Browser-local reply watermarks. An entry's sequence, not a conversation's
 * any-write timestamp, determines whether an assistant reply is unseen.
 * Versioned keys cleanly retire the old epoch-timestamp representation. */
const PREFIX = "cadence.assistant.seen.v2";
export type SeenMap = Record<string, number>;

export function seenKey(installId: string): string {
  return `${PREFIX}.${installId}`;
}

function readMap(key: string): SeenMap {
  try {
    const raw: unknown = JSON.parse(window.localStorage.getItem(key) ?? "{}");
    if (raw === null || typeof raw !== "object" || Array.isArray(raw)) return {};
    const out: SeenMap = {};
    for (const [id, seq] of Object.entries(raw)) {
      if (typeof seq === "number" && Number.isSafeInteger(seq) && seq >= 0) out[id] = seq;
    }
    return out;
  } catch {
    return {};
  }
}

function advance(key: string, id: string, seq: number): boolean {
  if (!Number.isSafeInteger(seq) || seq <= 0) return false;
  try {
    const map = readMap(key);
    if ((map[id] ?? 0) >= seq) return false;
    map[id] = seq;
    window.localStorage.setItem(key, JSON.stringify(map));
    return true;
  } catch {
    return false;
  }
}

export function readSeen(installId: string): SeenMap {
  return readMap(seenKey(installId));
}

export function writeSeen(installId: string, id: string, seq: number): void {
  if (document.visibilityState !== "visible") return;
  const key = seenKey(installId);
  if (advance(key, id, seq)) window.dispatchEvent(new Event(`${key}:changed`));
}

export function isUnread(seen: SeenMap, id: string, replySeq: number): boolean {
  return replySeq > (seen[id] ?? 0);
}

/** Reserve this reply's one browser-local announcement before showing it. */
export function announceReply(installId: string, id: string, replySeq: number): boolean {
  if (document.visibilityState !== "visible") return false;
  return advance(`${seenKey(installId)}.announced`, id, replySeq);
}

/** Seen writes notify the current tab as well as other tabs. */
export function onSeenChanged(installId: string, listener: (seen: SeenMap) => void): () => void {
  const key = seenKey(installId);
  const changed = () => listener(readSeen(installId));
  const storage = (event: StorageEvent) => {
    if (event.key === key || event.key === null) changed();
  };
  window.addEventListener("storage", storage);
  window.addEventListener(`${key}:changed`, changed);
  return () => {
    window.removeEventListener("storage", storage);
    window.removeEventListener(`${key}:changed`, changed);
  };
}
