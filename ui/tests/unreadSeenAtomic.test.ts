/**
 * CAD-1700: browser-local reply watermarks advance atomically.
 *
 * Real localStorage reads current shared data; the lost update is the gap
 * between a tab's read and its write, not event-delivery lag. To overlap
 * those windows deterministically, the first write's getItem captures the
 * old value and synchronously re-enters the second context's whole public
 * write before returning the captured value: getA → getB → setB → setA.
 * The fixed async API has no such hook, so both calls are started before
 * either is awaited and their real IndexedDB readwrite transactions compete.
 */
import type * as HappyDOM from "happy-dom";
import type * as SeenModule from "../src/features/app-shell/chat/unreadSeen";
declare function require<T>(name: string): T;

const { indexedDB } = require<{ indexedDB: IDBFactory }>("fake-indexeddb");
const { Window: BrowserWindow } = require<typeof HappyDOM>("happy-dom");
const { setImmediate: nextTurn } = require<{ setImmediate: () => Promise<void> }>("node:timers/promises");

const win = new BrowserWindow({ url: "http://localhost/" });
for (const name of ["window", "document", "navigator", "Event", "StorageEvent", "localStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
Object.defineProperty(globalThis, "indexedDB", { value: indexedDB, configurable: true });

const { readSeen, writeSeen, announceReply, onSeenChanged } =
  require<typeof SeenModule>("../src/features/app-shell/chat/unreadSeen");

/**
 * One origin's localStorage. Every read sees the current shared value. A
 * one-shot hook runs inside a read, after the value is captured, so a second
 * context can commit its whole read/modify/write before the first writes.
 * Like a browser, setItem fires a storage event only when the stored string
 * actually changes.
 */
class OriginStorage {
  private values = new Map<string, string>();
  private hook: (() => void) | null = null;
  arm(hook: () => void): void {
    this.hook = hook;
  }
  getItem(key: string): string | null {
    const value = this.values.get(key) ?? null;
    const hook = this.hook;
    this.hook = null;
    if (hook !== null) hook();
    return value;
  }
  setItem(key: string, value: string): void {
    const previous = this.values.get(key) ?? null;
    this.values.set(key, value);
    if (previous === value) return;
    // Omitted init fields default to null in the event; the happy-dom type
    // does not accept null literals here.
    win.dispatchEvent(new win.StorageEvent("storage", previous === null
      ? { key, newValue: value }
      : { key, oldValue: previous, newValue: value }));
  }
  removeItem(key: string): void {
    const previous = this.values.get(key) ?? null;
    if (previous === null) return;
    this.values.delete(key);
    win.dispatchEvent(new win.StorageEvent("storage", { key, oldValue: previous }));
  }
  clear(): void {
    this.values.clear();
    win.dispatchEvent(new win.StorageEvent("storage"));
  }
  key(index: number): string | null {
    return [...this.values.keys()][index] ?? null;
  }
  get length(): number {
    return this.values.size;
  }
}

/**
 * Start both contexts before awaiting either. The old synchronous store
 * re-enters `second` from inside `first`'s captured read; the fixed async
 * store ignores that hook, so `second` starts explicitly right after `first`
 * and both transactions are then awaited together.
 */
async function overlap(
  storage: OriginStorage,
  first: () => unknown,
  second: () => unknown,
): Promise<unknown[]> {
  let started = false;
  let other: unknown = undefined;
  const startOther = () => {
    if (started) return;
    started = true;
    other = second();
  };
  storage.arm(startOther);
  const mine = first();
  startOther();
  return Promise.all([Promise.resolve(mine), Promise.resolve(other)]);
}

function useStorage(storage: OriginStorage): void {
  Object.defineProperty(win, "localStorage", { value: storage, configurable: true });
}

function visibility(state: "visible" | "hidden"): void {
  Object.defineProperty(win.document, "visibilityState", { value: state, configurable: true });
}

async function drain(): Promise<void> {
  for (let turn = 0; turn < 12; turn++) await nextTurn();
}

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

async function main(): Promise<void> {
  visibility("visible");

  // Independent conversations, overlapping read/modify/write windows.
  const race = new OriginStorage();
  useStorage(race);
  await overlap(
    race,
    () => writeSeen("install-race", "alpha", 5),
    () => writeSeen("install-race", "beta", 3),
  );
  const raced = await readSeen("install-race");
  assert(
    raced.alpha === 5 && raced.beta === 3,
    "one tab's seen write cannot erase another conversation's watermark",
  );

  // One shared toast reservation across competing visible contexts.
  const reserved = new OriginStorage();
  useStorage(reserved);
  const outcomes = await overlap(
    reserved,
    () => announceReply("install-reserve", "general", 7),
    () => announceReply("install-reserve", "general", 7),
  );
  assert(
    outcomes.filter((value) => value === true).length === 1,
    "competing tabs reserve exactly one announcement for the same reply",
  );

  // Two real module instances model two JS contexts on one origin. A tab only
  // publishes a wake signal when its stored string changes, so a per-context
  // counter that restarts at 1 would make the second tab's write a no-op and
  // leave the first tab's watermark stale.
  const origin = new OriginStorage();
  useStorage(origin);
  const requireForReload = require as unknown as {
    <T>(id: string): T;
    resolve(id: string): string;
    cache: Record<string, unknown>;
  };
  const modulePath = requireForReload.resolve("../src/features/app-shell/chat/unreadSeen");
  const freshSeen = (): typeof SeenModule => {
    delete requireForReload.cache[modulePath];
    return requireForReload<typeof SeenModule>(modulePath);
  };
  const left = freshSeen();
  const right = freshSeen();
  const leftSnapshots: Record<string, number>[] = [];
  const rightSnapshots: Record<string, number>[] = [];
  const stopLeft = left.onSeenChanged("install-tabs", (seen) => leftSnapshots.push(seen));
  const stopRight = right.onSeenChanged("install-tabs", (seen) => rightSnapshots.push(seen));
  await drain();
  await left.writeSeen("install-tabs", "left", 1);
  await right.writeSeen("install-tabs", "right", 1);
  await drain();
  assert(
    leftSnapshots.at(-1)?.right === 1,
    "the first tab observes the second tab's committed watermark through its wake signal",
  );
  assert(
    rightSnapshots.at(-1)?.left === 1,
    "the second tab observes the first tab's committed watermark through its wake signal",
  );
  stopLeft();
  stopRight();

  // Monotonic watermarks and already-seen replies.
  const mono = new OriginStorage();
  useStorage(mono);
  const advanced: unknown = await writeSeen("install-mono", "general", 5);
  assert(advanced === true, "a new sequence commits an advancement");
  const regressed: unknown = await writeSeen("install-mono", "general", 3);
  assert(regressed === false, "a lower sequence cannot regress a watermark");
  await writeSeen("install-mono", "general", 4);
  assert((await readSeen("install-mono")).general === 5, "the watermark stays at its maximum");
  assert(
    (await announceReply("install-mono", "general", 5)) === false,
    "an already-seen reply is never announced",
  );
  assert(
    (await announceReply("install-mono", "general", 4)) === false,
    "an older reply than the seen watermark is never announced",
  );

  // Visibility is checked at the write, not merely before the first await.
  visibility("visible");
  // `unknown` keeps the awaited result comparable under both the old (void)
  // and the fixed (Promise<boolean>) signature.
  const hiddenSeen: unknown = writeSeen("install-hidden", "general", 4);
  visibility("hidden");
  assert(
    (await hiddenSeen) === false,
    "a tab hidden before the write commits does not advance seen state",
  );
  visibility("visible");
  const hiddenMap = await readSeen("install-hidden");
  assert(
    (hiddenMap.general ?? 0) === 0,
    "the hidden tab left no watermark behind",
  );

  visibility("visible");
  const hiddenAnnounce: unknown = announceReply("install-hidden-announce", "general", 9);
  visibility("hidden");
  assert(
    (await hiddenAnnounce) === false,
    "a tab hidden before the reservation commits does not reserve the toast",
  );
  visibility("visible");
  assert(
    (await announceReply("install-hidden-announce", "general", 9)) === true,
    "the foreground tab can still reserve the announcement",
  );

  // Observers receive the persisted snapshot asynchronously on subscribe and
  // are woken by later commits; cleanup stops delivery.
  visibility("visible");
  await writeSeen("install-observe", "general", 2);
  const snapshots: Record<string, number>[] = [];
  const stop = onSeenChanged("install-observe", (seen) => snapshots.push(seen));
  await drain();
  assert(
    snapshots.length >= 1 && snapshots[0].general === 2,
    "an observer starts with an async snapshot of persisted watermarks",
  );
  // A reservation is deduplicated in the store; it must not wake seen
  // observers (a self-inflicted wake would retire its own display).
  await announceReply("install-observe", "announced-only", 3);
  const beforeReservation = snapshots.length;
  await drain();
  assert(
    snapshots.length === beforeReservation,
    "an announcement reservation does not wake seen observers",
  );
  await writeSeen("install-observe", "other", 1);
  await drain();
  assert(snapshots.at(-1)?.other === 1, "a same-tab commit wakes the observer with fresh watermarks");
  const delivered = snapshots.length;
  stop();
  await writeSeen("install-observe", "ignored", 1);
  await drain();
  assert(snapshots.length === delivered, "cleanup stops observer delivery");

  await win.happyDOM.close();
  console.log("atomic unread storage checks passed");
}

void main();
