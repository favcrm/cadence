import { Resource } from "../src/lib/cache";
import { isPermanent, streamInto, subscribeSse, withResume, type EventSourceLike, type SseErrorState } from "../src/lib/sse";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

/** An EventSource the test drives by hand. */
class FakeSource implements EventSourceLike {
  readyState = 0;
  onerror: ((e: Event) => void) | null = null;
  onopen: ((e: Event) => void) | null = null;
  closed = false;
  private readonly listeners = new Map<string, ((e: MessageEvent<string>) => void)[]>();
  constructor(readonly url: string) {}
  addEventListener(type: string, listener: (e: MessageEvent<string>) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }
  close(): void {
    this.closed = true;
    this.readyState = 2;
  }
  emit(type: string, data: string, id = ""): void {
    for (const l of this.listeners.get(type) ?? []) {
      l({ data, lastEventId: id } as MessageEvent<string>);
    }
  }
  opened(): void {
    this.readyState = 1;
    this.onopen?.({} as Event);
  }
  fail(readyState: number): void {
    this.readyState = readyState;
    this.onerror?.({} as Event);
  }
}

const flush = () => new Promise<void>((r) => setTimeout(r, 0));

async function main() {
  equal(withResume("/api/threads/pm/stream", null), "/api/threads/pm/stream", "no cursor");
  equal(withResume("/api/threads/pm/stream", "7"), "/api/threads/pm/stream?after=7", "cursor added");
  equal(withResume("/x?after=3&y=1", "9"), "/x?after=9&y=1", "cursor replaced");

  // Tracks ids, resumes a closed source with ?after=, ignores the old one,
  // backs off exponentially up to the cap and resets once a source opens.
  {
    const opened: FakeSource[] = [];
    const seen: string[] = [];
    const states: SseErrorState[] = [];
    const delays: number[] = [];
    const due: (() => void)[] = [];
    const sub = subscribeSse({
      url: "/api/threads/pm/stream",
      events: ["entry"],
      lastEventId: "4",
      retryMs: 100,
      maxRetryMs: 300,
      onEvent: (e) => seen.push(`${e.type}:${e.id}:${e.data}`),
      onError: (state) => states.push(state),
      probe: async () => 503,
      schedule: (fn, ms) => {
        delays.push(ms);
        due.push(fn);
        return () => {};
      },
      open: (url) => {
        const s = new FakeSource(url);
        opened.push(s);
        return s;
      },
    });
    const fire = async () => {
      await flush();
      due.shift()?.();
    };
    equal(opened[0].url, "/api/threads/pm/stream?after=4", "first open resumes from the given id");
    opened[0].emit("entry", "a", "5");
    opened[0].emit("entry", "b");
    equal(sub.lastEventId(), "5", "an id-less frame keeps the cursor");
    // CONNECTING: the browser reconnects by itself — no second source.
    opened[0].fail(0);
    await flush();
    equal(opened.length, 1, "no manual reopen while the browser retries");
    opened[0].emit("entry", "c", "6");
    // CLOSED: the helper reopens with the cursor, once.
    opened[0].fail(2);
    opened[0].fail(2);
    await fire();
    equal(opened.length, 2, "exactly one reopen after a close");
    equal(opened[1].url, "/api/threads/pm/stream?after=6", "reopen resumes after the last id");
    opened[0].emit("entry", "late", "99");
    equal(sub.lastEventId(), "6", "the dead source is ignored");
    opened[1].fail(2);
    await fire();
    opened[2].fail(2);
    await fire();
    opened[3].fail(2);
    await fire();
    equal(delays, [100, 200, 300, 300], "exponential backoff with a cap");
    opened[4].opened();
    opened[4].fail(2);
    await fire();
    equal(delays[4], 100, "an open resets the backoff");
    equal(seen, ["entry:5:a", "entry:null:b", "entry:6:c"], "frames delivered in order");
    equal(states.slice(0, 2), ["reconnecting", "reopening"], "error states");
    sub.close();
    equal(opened[5].closed, true, "close closes the live source");
    opened[5].fail(2);
    await flush();
    equal(opened.length, 6, "no reopen after close");
  }

  // A permanent 4xx stops the subscription; timeouts and 5xx keep retrying.
  {
    const opened: FakeSource[] = [];
    const states: SseErrorState[] = [];
    subscribeSse({
      url: "/api/threads/ghost/stream",
      events: ["entry"],
      onEvent: () => {},
      onError: (state) => states.push(state),
      probe: async () => 404,
      schedule: (fn) => {
        fn();
        return () => {};
      },
      open: (url) => {
        const s = new FakeSource(url);
        opened.push(s);
        return s;
      },
    });
    opened[0].fail(2);
    await flush();
    await flush();
    equal(states, ["stopped"], "404 stops");
    equal(opened.length, 1, "no reopen after a permanent refusal");
    equal(isPermanent(404) && isPermanent(410) && isPermanent(400), true, "4xx permanent");
    equal(isPermanent(408) || isPermanent(429) || isPermanent(503) || isPermanent(null), false, "retryable");
  }

  // streamInto folds frames into a cache store.
  {
    const r = new Resource<string[]>(() => Promise.resolve([]));
    let source: FakeSource | null = null;
    const sub = streamInto(r, (data, e) => [...(data ?? []), e.data], {
      url: "/s",
      events: ["entry"],
      open: (url) => (source = new FakeSource(url)),
    });
    source!.emit("entry", "hi", "1");
    source!.emit("entry", "there", "2");
    equal(r.get().data, ["hi", "there"], "frames land in the store");
    equal(r.get().status, "ok", "a streamed store is loaded");
    sub.close();
  }

  // A frame that lands while a fetch is in flight is not reverted by it.
  {
    let answer: (v: string[]) => void = () => {};
    let calls = 0;
    const r = new Resource<string[]>(() => {
      calls += 1;
      return new Promise((resolve) => (answer = resolve));
    });
    let source: FakeSource | null = null;
    const sub = streamInto(r, (data, e) => [...(data ?? []), e.data], {
      url: "/s",
      events: ["entry"],
      open: (url) => (source = new FakeSource(url)),
    });
    const p = r.refresh();
    source!.emit("entry", "live", "1");
    answer([]); // the page fetched before the frame
    await p;
    equal(r.get().data, ["live"], "stream frame survives the older fetch");
    equal(calls, 2, "one trailing fetch confirms");
    answer(["live"]);
    await flush();
    await flush();
    equal(r.get().data, ["live"], "trailing answer agrees");
    sub.close();
  }

  console.log("sse checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
