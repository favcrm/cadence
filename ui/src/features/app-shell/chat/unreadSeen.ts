/** Browser-local reply watermarks. An entry's sequence, not a conversation's
 * any-write timestamp, determines whether an assistant reply is unseen.
 *
 * localStorage get/set is not a transaction: two visible tabs can both read
 * the old map before either writes, losing one conversation's watermark or
 * reserving the same reply twice. Seen watermarks and announcement
 * reservations therefore live in one IndexedDB record per install, mutated
 * only inside a single readwrite transaction, which the browser serializes
 * across connections. IndexedDB is the baseline for every board origin;
 * Web Locks would exclude insecure-HTTP origins. A localStorage signal only
 * wakes observers — it never carries authoritative state.
 */
const DB_NAME = "cadence.assistant.unread";
const STORE = "watermarks";
const SIGNAL_PREFIX = "cadence.assistant.unread.signal";

export type SeenMap = Record<string, number>;

interface Watermarks {
  install: string;
  /** Highest seen reply sequence per conversation. */
  seen: SeenMap;
  /** Highest announced reply sequence per conversation. */
  announced: SeenMap;
}

function normalizeMap(raw: unknown): SeenMap {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) return {};
  const out: SeenMap = {};
  for (const [id, seq] of Object.entries(raw)) {
    if (typeof seq === "number" && Number.isSafeInteger(seq) && seq > 0) out[id] = seq;
  }
  return out;
}

function normalize(raw: unknown, install: string): Watermarks {
  const record = raw !== null && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  return { install, seen: normalizeMap(record.seen), announced: normalizeMap(record.announced) };
}

let opening: Promise<IDBDatabase | null> | null = null;

/** One connection per tab; `null` (no IndexedDB) fails every write closed. */
function database(): Promise<IDBDatabase | null> {
  if (opening !== null) return opening;
  opening = new Promise<IDBDatabase | null>((resolve) => {
    if (typeof indexedDB === "undefined") {
      resolve(null);
      return;
    }
    let request: IDBOpenDBRequest;
    try {
      request = indexedDB.open(DB_NAME, 1);
    } catch {
      resolve(null);
      return;
    }
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE, { keyPath: "install" });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => resolve(null);
  });
  return opening;
}

/** The persisted seen watermarks, or an empty map when storage is unavailable. */
export async function readSeen(installId: string): Promise<SeenMap> {
  const db = await database();
  if (db === null) return {};
  return new Promise<SeenMap>((resolve) => {
    let seen: SeenMap = {};
    let transaction: IDBTransaction;
    try {
      transaction = db.transaction(STORE, "readonly");
    } catch {
      resolve({});
      return;
    }
    transaction.oncomplete = () => resolve(seen);
    transaction.onerror = () => resolve({});
    transaction.onabort = () => resolve({});
    const request = transaction.objectStore(STORE).get(installId);
    request.onsuccess = () => {
      seen = normalize(request.result, installId).seen;
    };
  });
}

interface Mutation {
  commit: boolean;
  /** Undo a display if the transaction cannot commit. */
  rollback?: () => void;
}

type Change = (marks: Watermarks) => Mutation | null;

/**
 * One read/modify/write transaction. The change callback (and any delivery it
 * performs synchronously) runs inside the transaction's request callback, so
 * the decision is made on fresh state and no other connection can interleave.
 * Returns true only when the put committed; abort/error/put failure runs the
 * delivery's rollback and returns false.
 */
function transact(installId: string, change: Change): Promise<boolean> {
  return database().then((db) => {
    if (db === null) return false;
    return new Promise<boolean>((resolve) => {
      let written = false;
      let rollback: (() => void) | null = null;
      let finished = false;
      const fail = () => {
        if (finished) return;
        finished = true;
        rollback?.();
        resolve(false);
      };
      let transaction: IDBTransaction;
      try {
        transaction = db.transaction(STORE, "readwrite");
      } catch {
        resolve(false);
        return;
      }
      transaction.oncomplete = () => {
        if (finished) return;
        finished = true;
        resolve(written);
      };
      transaction.onerror = fail;
      transaction.onabort = fail;
      const store = transaction.objectStore(STORE);
      const request = store.get(installId);
      request.onsuccess = () => {
        const marks = normalize(request.result, installId);
        let mutation: Mutation | null;
        try {
          mutation = change(marks);
        } catch {
          mutation = null;
        }
        if (mutation === null) return;
        rollback = mutation.rollback ?? null;
        if (!mutation.commit) return;
        try {
          store.put(marks);
        } catch {
          try {
            transaction.abort();
          } catch {
            // The transaction is already aborting.
          }
          fail();
          return;
        }
        written = true;
      };
    });
  });
}

export function isUnread(seen: SeenMap, conversation: string, replySeq: number): boolean {
  return replySeq > (seen[conversation] ?? 0);
}

const listeners = new Map<string, Set<() => void>>();

/** Per-context identity for the wake signal. A storage event fires in other
 * tabs only when the stored string actually changes, so a counter alone lets
 * two tabs both write "1" and suppresses the second tab's wake. Web Crypto's
 * getRandomValues is available on insecure HTTP board origins (randomUUID and
 * subtle are not); the counter below still guarantees a change per wake. */
function signalIdentity(): string {
  try {
    if (typeof crypto !== "undefined" && typeof crypto.getRandomValues === "function") {
      const words = crypto.getRandomValues(new Uint32Array(2));
      return `${words[0].toString(36)}${words[1].toString(36)}`;
    }
  } catch {
    // Fall through to the non-cryptographic identity.
  }
  return Math.random().toString(36).slice(2);
}
const identity = signalIdentity();
let signalValue = 0;

/** Wake seen observers; the committed record remains the only authority. */
function wake(installId: string): void {
  const waiting = listeners.get(installId);
  if (waiting !== undefined) for (const listener of [...waiting]) listener();
  try {
    signalValue += 1;
    window.localStorage.setItem(`${SIGNAL_PREFIX}.${installId}`, `${identity}:${signalValue}`);
  } catch {
    // A blocked signal costs another tab one prompt refresh, never state.
  }
}

/**
 * Mark a conversation seen through `seq`. The visibility check runs inside
 * the transaction, so a tab hidden while the write was pending does not
 * consume the reply. Returns true only for a committed advancement.
 */
export async function writeSeen(installId: string, conversation: string, seq: number): Promise<boolean> {
  if (!Number.isSafeInteger(seq) || seq <= 0) return false;
  const advanced = await transact(installId, (marks) => {
    if (document.visibilityState !== "visible") return { commit: false };
    if ((marks.seen[conversation] ?? 0) >= seq) return { commit: false };
    marks.seen[conversation] = seq;
    return { commit: true };
  });
  if (advanced) wake(installId);
  return advanced;
}

/** Synchronously shows the toast and returns its rollback, or returns null
 * when the reply is no longer eligible (unmounted, room now foreground, …). */
export type Announce = () => (() => void) | null;

/**
 * Reserve and deliver this reply's one browser-local announcement.
 *
 * The delivery callback runs inside the same serialized readwrite transaction
 * that checks fresh seen/announced state and document visibility, so there is
 * no async yield in which an abandoned reservation could suppress another
 * tab's toast. Only a delivered callback advances `announced`; returning null
 * leaves no claim behind. If the put cannot commit, the callback's rollback
 * removes the shown toast and the call resolves false. Reservations never
 * wake seen observers.
 */
export async function announceReply(
  installId: string,
  conversation: string,
  replySeq: number,
  announce: Announce,
): Promise<boolean> {
  if (!Number.isSafeInteger(replySeq) || replySeq <= 0) return false;
  return transact(installId, (marks) => {
    if (document.visibilityState !== "visible") return { commit: false };
    if ((marks.seen[conversation] ?? 0) >= replySeq) return { commit: false };
    if ((marks.announced[conversation] ?? 0) >= replySeq) return { commit: false };
    const rollback = announce();
    if (rollback === null) return { commit: false };
    marks.announced[conversation] = replySeq;
    return { commit: true, rollback };
  });
}

/**
 * Observe persisted watermarks. Registration is synchronous; the first
 * snapshot arrives asynchronously, followed by same-tab commits and other
 * tabs' storage signals. Older snapshots are fenced so a slow read cannot
 * overwrite a newer one.
 */
export function onSeenChanged(installId: string, listener: (seen: SeenMap) => void): () => void {
  let active = true;
  let issued = 0;
  const deliver = () => {
    const ticket = ++issued;
    void readSeen(installId).then((seen) => {
      if (active && ticket === issued) listener(seen);
    });
  };
  const waiting = listeners.get(installId) ?? new Set<() => void>();
  listeners.set(installId, waiting);
  waiting.add(deliver);
  const key = `${SIGNAL_PREFIX}.${installId}`;
  const storage = (event: StorageEvent) => {
    if (event.key === key || event.key === null) deliver();
  };
  window.addEventListener("storage", storage);
  deliver();
  return () => {
    active = false;
    waiting.delete(deliver);
    if (waiting.size === 0) listeners.delete(installId);
    window.removeEventListener("storage", storage);
  };
}
