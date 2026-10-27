import { Resource } from "../src/lib/cache";
import { isPermanent, streamInto, subscribeSse, withResume, type EventSourceLike, type SseErrorState } from "../src/lib/sse";
import { allowPrivateReads } from "../src/lib/privateSession";
import { SESSION_HEADER, setSessionKey } from "../src/lib/sessionKey";
import type { SseSubscription } from "../src/lib/sse";

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
  allowPrivateReads();
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

  // The default transport must carry the tab credential as a header while
  // retaining browser same-origin cookie semantics, and parse real byte chunks.
  {
    const oldStorage = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");
    const oldFetch = globalThis.fetch;
    const storage = new Map<string, string>();
    Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => { storage.set(key, value); },
      removeItem: (key: string) => { storage.delete(key); },
    } });
    const requests: { url: string; init: RequestInit | undefined }[] = [];
    let stream!: ReadableStreamDefaultController<Uint8Array>;
    const body = new ReadableStream<Uint8Array>({ start(controller) { stream = controller; } });
    const seen: { type: string; data: string; id: string | null }[] = [];
    // ES2023 is this harness's lib target; Promise.withResolvers is unavailable.
    let delivered!: () => void;
    const framesDelivered = new Promise<void>((resolve) => { delivered = resolve; });
    let sub: SseSubscription | undefined;
    try {
      setSessionKey("private-stream-key");
      globalThis.fetch = async (input, init) => {
        requests.push({ url: String(input), init });
        return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
      };
      sub = subscribeSse({
        url: "/api/threads/member/stream",
        lastEventId: "4",
        events: ["entry", "message"],
        onEvent: (event) => { seen.push(event); if (seen.length === 3) delivered(); },
        schedule: () => { throw new Error("Closed stream must not schedule a retry"); },
      });
      equal(requests.length, 1, "default transport performs one fetch");
      equal(requests[0].url, "/api/threads/member/stream?after=4", "resume URL contains only the cursor, never the session key");
      equal(new Headers(requests[0].init?.headers).get(SESSION_HEADER), "private-stream-key", "stream sends this tab's second credential");
      equal(new Headers(requests[0].init?.headers).get("Accept"), "text/event-stream", "stream requests SSE");
      equal(requests[0].init?.credentials ?? "same-origin", "same-origin", "stream retains same-origin cookies without cross-origin credentials");
      equal(requests[0].init?.cache, "no-store", "stream response cannot enter the HTTP cache");
      const bytes = new TextEncoder().encode(": keepalive\r\nid: 7\r\nevent: entry\r\ndata: café\r\ndata: second\r\n\r\ndata: plain\n\nid: 8\nevent: entry\ndata: final\n\n");
      const cr = bytes.indexOf(13), utf8 = bytes.indexOf(0xc3);
      // Deliberately split CRLF and one UTF-8 character across chunks.
      for (const [from, to] of [[0, cr + 1], [cr + 1, utf8 + 1], [utf8 + 1, bytes.length]])
        stream.enqueue(bytes.slice(from, to));
      await framesDelivered;
      equal(seen, [
        { type: "entry", data: "café\nsecond", id: "7" },
        { type: "message", data: "plain", id: "7" },
        { type: "entry", data: "final", id: "8" },
      ], "default transport parses chunks, multiline data, named/default events and inherited cursors");
      equal(sub.lastEventId(), "8", "parsed stream cursor is available for resume");
      sub.close();
      equal(requests[0].init?.signal?.aborted, true, "close aborts the authenticated fetch");
      stream.enqueue(new TextEncoder().encode("event: entry\ndata: late\n\n"));
      stream.close();
      await Promise.resolve();
      await Promise.resolve();
      equal(seen.length, 3, "closed subscription ignores late bytes");

      // The default status probe must obey the same second-credential rule.
      requests.length = 0;
      const errors: SseErrorState[] = [];
      let stopped!: () => void;
      const refused = new Promise<void>((resolve) => { stopped = resolve; });
      globalThis.fetch = async (input, init) => {
        requests.push({ url: String(input), init });
        return new Response(null, { status: requests.length === 1 ? 503 : 403 });
      };
      sub = subscribeSse({
        url: "/api/threads/member/stream",
        events: ["entry"],
        onEvent: () => { throw new Error("Refused stream cannot deliver events"); },
        onError: (state) => { errors.push(state); if (state === "stopped") stopped(); },
        schedule: () => { throw new Error("Permanent refusal must not schedule a retry"); },
      });
      await refused;
      equal(requests.length, 2, "failed default stream is status-probed once");
      for (const request of requests) {
        equal(new Headers(request.init?.headers).get(SESSION_HEADER), "private-stream-key", "both stream and status probe send the tab key");
        equal(request.init?.credentials ?? "same-origin", "same-origin", "both requests retain same-origin cookie policy");
        equal(request.init?.signal?.aborted, true, "retired stream and header-only probe are aborted");
      }
      equal(errors, ["stopped"], "permanent authenticated probe refusal stops reconnection");
    } finally {
      sub?.close();
      globalThis.fetch = oldFetch;
      if (oldStorage) Object.defineProperty(globalThis, "sessionStorage", oldStorage);
      else Reflect.deleteProperty(globalThis, "sessionStorage");
    }
  }

  console.log("sse checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
