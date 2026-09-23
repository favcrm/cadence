import type { Resource } from "./cache";

/**
 * A server-sent-events subscription that resumes where it left off.
 *
 * The browser's own reconnect already sends `Last-Event-ID`, but an
 * `EventSource` that closes for good (a non-200 answer, a server that went
 * away) never retries, and a new page has no id to send. So the helper
 * remembers the last `id:` it saw and, when the source is closed, reopens
 * it with `?after=<id>` — the query form of the same cursor that the
 * thread stream (`GET /api/threads/<alias>/stream`) accepts.
 *
 * No React here, and the `EventSource` is injectable, so the resume logic
 * is unit-tested in plain node (tests/sse.test.ts).
 */

export interface SseEvent {
  /** The frame's `event:` name (`message` for unnamed frames). */
  type: string;
  data: string;
  /** The frame's `id:`, or null when it carried none. */
  id: string | null;
}

/** The part of `EventSource` the helper uses. */
export interface EventSourceLike {
  readonly readyState: number;
  addEventListener(type: string, listener: (e: MessageEvent<string>) => void): void;
  close(): void;
  onerror: ((e: Event) => void) | null;
}

export interface SseOptions {
  url: string;
  /** Named events to listen for; `message` covers unnamed frames. */
  events: string[];
  onEvent: (event: SseEvent) => void;
  /** Resume point from earlier (say, the last entry already cached). */
  lastEventId?: string | null;
  /** Delay before reopening a closed source. */
  retryMs?: number;
  /** Told about every error; `closed` when the helper will reopen. */
  onError?: (closed: boolean) => void;
  /** Test seam; defaults to the browser's `EventSource`. */
  open?: (url: string) => EventSourceLike;
}

export interface SseSubscription {
  close(): void;
  lastEventId(): string | null;
}

/** `EventSource.CLOSED` without needing the DOM global in node. */
const CLOSED = 2;

/** `url` with `after=<id>` set (replacing any earlier cursor). */
export function withResume(url: string, lastEventId: string | null): string {
  if (!lastEventId) return url;
  const [path, query = ""] = url.split("?", 2);
  const q = new URLSearchParams(query);
  q.set("after", lastEventId);
  return `${path}?${q.toString()}`;
}

export function subscribeSse(opts: SseOptions): SseSubscription {
  const open = opts.open ?? ((url: string) => new EventSource(url) as EventSourceLike);
  const retryMs = opts.retryMs ?? 3_000;
  let lastId = opts.lastEventId ?? null;
  let source: EventSourceLike | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let closed = false;

  const connect = () => {
    timer = null;
    if (closed) return;
    const es = open(withResume(opts.url, lastId));
    source = es;
    for (const type of opts.events) {
      es.addEventListener(type, (e) => {
        if (source !== es) return;
        const id = e.lastEventId ? e.lastEventId : null;
        if (id) lastId = id;
        opts.onEvent({ type, data: e.data, id });
      });
    }
    es.onerror = () => {
      if (source !== es) return;
      // CONNECTING: the browser retries by itself, sending Last-Event-ID.
      const gone = es.readyState === CLOSED && !closed;
      opts.onError?.(gone);
      if (gone) {
        // Retire this source first: its late frames and errors are ignored.
        source = null;
        es.close();
        timer = setTimeout(connect, retryMs);
      }
    };
  };

  connect();
  return {
    close() {
      closed = true;
      if (timer !== null) clearTimeout(timer);
      timer = null;
      source?.close();
      source = null;
    },
    lastEventId: () => lastId,
  };
}

/**
 * Stream frames into a cache store: each event folds into the store's data
 * through `reduce`, so every screen reading the store sees it at once.
 */
export function streamInto<T>(
  resource: Resource<T>,
  reduce: (data: T | null, event: SseEvent) => T,
  opts: Omit<SseOptions, "onEvent">,
): SseSubscription {
  return subscribeSse({
    ...opts,
    onEvent: (event) => resource.write((data) => reduce(data, event)),
  });
}
