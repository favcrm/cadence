import type { Resource } from "./cache";

// Filled by vite `define` at build time — undefined in plain-node tests.
declare const __CADENCE_BUILD__: string | undefined;

/** The build id baked into this bundle ("unknown" where none was baked). */
export const UI_BUILD: string =
  typeof __CADENCE_BUILD__ === "string" ? __CADENCE_BUILD__ : "unknown";

/**
 * The `{build}` payload of a `hello` frame or `/api/version`'s body —
 * null when absent or unparseable.
 */
export function parseBuild(data: string | null | undefined): string | null {
  try {
    const v = JSON.parse(data ?? "")?.build;
    return typeof v === "string" && v.length > 0 ? v : null;
  } catch {
    return null;
  }
}

/**
 * true when the serving build is known to differ from this bundle's.
 * `unknown` on either side — git was absent at build time — is
 * cannot-tell and never prompts a reload.
 */
export function buildChanged(server: string | null | undefined, local: string): boolean {
  if (!server || !local) return false;
  if (server === "unknown" || local === "unknown") return false;
  if (server.endsWith("+unknown") || local.endsWith("+unknown")) return false;
  return server !== local;
}

/** `GET /api/version` — the serving build while the stream is down. */
export async function serverBuild(): Promise<string | null> {
  try {
    const resp = await fetch("/api/version");
    return resp.ok ? parseBuild(await resp.text()) : null;
  } catch {
    return null;
  }
}

/**
 * A server-sent-events subscription that resumes where it left off.
 *
 * The browser's own reconnect already sends `Last-Event-ID`, but an
 * `EventSource` that closes for good (a non-200 answer, a server that went
 * away) never retries, and a new page has no id to send. So the helper
 * remembers the last `id:` it saw and, when the source is closed, reopens
 * it with `?after=<id>` — the query form of the same cursor that the
 * thread stream (`GET /api/threads/<alias>/stream`) accepts. Reopens back
 * off exponentially (reset once a source opens), and a closed source is
 * probed first: a permanent 4xx (unknown alias, no such route) stops the
 * subscription instead of retrying forever.
 *
 * No React here, and the `EventSource` is injectable, so the resume logic
 * is unit-tested in plain node (tests/sse.test.ts).
 */

export interface SseEvent {
  /** The frame's `event:` name (`message` for unnamed frames). */
  type: string;
  data: string;
  /**
   * The stream's last event id as of this frame: the frame's own `id:`, or
   * — since browsers carry `lastEventId` over to frames without one — the
   * most recent earlier id; null before the stream sent any.
   */
  id: string | null;
}

/** The part of `EventSource` the helper uses. */
export interface EventSourceLike {
  readonly readyState: number;
  addEventListener(type: string, listener: (e: MessageEvent<string>) => void): void;
  close(): void;
  onerror: ((e: Event) => void) | null;
  onopen: ((e: Event) => void) | null;
}

/** What an error means for the subscription. */
export type SseErrorState =
  /** The browser is reconnecting by itself (sending Last-Event-ID). */
  | "reconnecting"
  /** The source closed; the helper reopens it after a backoff. */
  | "reopening"
  /** The server refused for good (a permanent 4xx); no more retries. */
  | "stopped";

export interface SseOptions {
  url: string;
  /** Named events to listen for; `message` covers unnamed frames. */
  events: string[];
  onEvent: (event: SseEvent) => void;
  /** Resume point from earlier (say, the last entry already cached). */
  lastEventId?: string | null;
  /** First delay before reopening a closed source; doubles per failure. */
  retryMs?: number;
  /** Cap on the reopen delay. */
  maxRetryMs?: number;
  /** Told about every error, with what happens next. */
  onError?: (state: SseErrorState) => void;
  /**
   * The serving build every `hello` frame announces — on the first
   * connect and every reconnect — and what `probeBuild` answers after
   * `buildProbeMs` down. The caller compares it against `UI_BUILD`.
   */
  onBuild?: (build: string) => void;
  /** Fetches the serving build while the stream stays down. */
  probeBuild?: () => Promise<string | null>;
  /** How long the stream may be down before `probeBuild` runs (~15 s). */
  buildProbeMs?: number;
  /** Test seam; defaults to the browser's `EventSource`. */
  open?: (url: string) => EventSourceLike;
  /**
   * Test seam: the HTTP status the URL answers now, or null when unknown
   * (network down). Defaults to a GET that is aborted once headers arrive.
   */
  probe?: (url: string) => Promise<number | null>;
  /** Test seam: run `fn` after `ms`; returns a cancel. Defaults to setTimeout. */
  schedule?: (fn: () => void, ms: number) => () => void;
}

export interface SseSubscription {
  close(): void;
  lastEventId(): string | null;
}

/** `EventSource.CLOSED` without needing the DOM global in node. */
const CLOSED = 2;

/** A status no retry will change: 4xx except timeout and rate limiting. */
export function isPermanent(status: number | null): boolean {
  return status !== null && status >= 400 && status < 500 && status !== 408 && status !== 429;
}

async function probeStatus(url: string): Promise<number | null> {
  const abort = new AbortController();
  try {
    const resp = await fetch(url, { signal: abort.signal, headers: { Accept: "text/event-stream" } });
    return resp.status;
  } catch {
    return null;
  } finally {
    abort.abort();
  }
}

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
  const probe = opts.probe ?? probeStatus;
  const schedule =
    opts.schedule ??
    ((fn: () => void, ms: number) => {
      const t = setTimeout(fn, ms);
      return () => clearTimeout(t);
    });
  const firstDelay = opts.retryMs ?? 1_000;
  const maxDelay = opts.maxRetryMs ?? 30_000;
  let delay = firstDelay;
  let lastId = opts.lastEventId ?? null;
  let source: EventSourceLike | null = null;
  let cancelTimer: (() => void) | null = null;
  let closed = false;
  let streamDown = false;
  let downGen = 0;
  let cancelProbe: (() => void) | null = null;

  // The stream just went down: if it stays down past `buildProbeMs`,
  // ask the server its build outright — a tab whose reconnects never
  // land still learns it is stale (CAD-573).
  const armBuildProbe = () => {
    if (streamDown || !opts.probeBuild) return;
    streamDown = true;
    const gen = ++downGen;
    cancelProbe = schedule(async () => {
      cancelProbe = null;
      if (closed || !streamDown || gen !== downGen) return;
      const build = await opts.probeBuild!().catch(() => null);
      if (closed || !streamDown || gen !== downGen || build === null) return;
      opts.onBuild?.(build);
    }, opts.buildProbeMs ?? 15_000);
  };

  const connect = () => {
    cancelTimer = null;
    if (closed) return;
    const url = withResume(opts.url, lastId);
    const es = open(url);
    source = es;
    es.onopen = () => {
      if (source !== es) return;
      delay = firstDelay;
      // Up again — a pending down-probe is stale.
      streamDown = false;
      downGen++;
      cancelProbe?.();
      cancelProbe = null;
    };
    if (opts.onBuild) {
      // `hello` is the server's first frame on every (re)connect.
      es.addEventListener("hello", (e) => {
        if (source !== es) return;
        const build = parseBuild(e.data);
        if (build !== null) opts.onBuild!(build);
      });
    }
    for (const type of opts.events) {
      es.addEventListener(type, (e) => {
        if (source !== es) return;
        const id = e.lastEventId ? e.lastEventId : null;
        if (id) lastId = id;
        opts.onEvent({ type, data: e.data, id });
      });
    }
    es.onerror = () => {
      if (source !== es || closed) return;
      armBuildProbe();
      // CONNECTING: the browser retries by itself, sending Last-Event-ID.
      if (es.readyState !== CLOSED) {
        opts.onError?.("reconnecting");
        return;
      }
      // Retire this source first: its late frames and errors are ignored.
      source = null;
      es.close();
      void probe(url).then((status) => {
        if (closed) return;
        if (isPermanent(status)) {
          closed = true;
          opts.onError?.("stopped");
          return;
        }
        opts.onError?.("reopening");
        cancelTimer = schedule(connect, delay);
        delay = Math.min(delay * 2, maxDelay);
      });
    };
  };

  connect();
  return {
    close() {
      closed = true;
      cancelTimer?.();
      cancelTimer = null;
      cancelProbe?.();
      cancelProbe = null;
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
