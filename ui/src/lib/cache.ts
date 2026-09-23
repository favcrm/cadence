/**
 * The board's data layer: one small store per API resource.
 *
 * Every screen reads the same shape, so "loading", "failed" and "empty"
 * mean the same thing everywhere:
 *
 * - `loading` — nothing has loaded yet and a request is in flight (a
 *   retry after a failure is a load again, not a failure).
 * - `ok` / `empty` — the last request succeeded; `empty` when the payload
 *   has no rows.
 * - `stale` — a refresh failed after an earlier success. The last good
 *   payload stays in `data`; `error` says why the refresh failed.
 * - `failed` — the request failed and nothing has ever loaded.
 *
 * Requests coalesce per resource: `refresh()` while one is in flight joins
 * it, and `invalidate()` (a change notification) while one is in flight
 * schedules exactly one trailing request, because the in-flight one may
 * have been answered from before the change.
 *
 * Stale-while-revalidate: `revalidate()` is what a screen calls when it
 * comes on screen. Loaded data stays visible while the refetch runs, and
 * data younger than `freshMs` is not refetched at all — so returning to a
 * slow screen (`/api/overview` takes seconds) paints at once.
 *
 * `QueryCache` keys the stores: `cache.resource(key, fetcher)` hands every
 * caller of one key the same store, so two components asking for the same
 * key share one request and one state.
 *
 * No React here — `useResource` adapts a store to a component — so the
 * state machine is unit-tested in plain node (see tests/cache.test.ts).
 */

export type ResourceStatus = "loading" | "ok" | "stale" | "failed" | "empty";

export interface ResourceState<T> {
  data: T | null;
  status: ResourceStatus;
  /** The last request's failure, kept while `stale` or `failed`. */
  error: string | null;
  /** Epoch ms of the last successful load. */
  asOf: number | null;
  /** A request is in flight (also true behind `ok`/`stale` rows). */
  inFlight: boolean;
}

export interface ResourceOptions<T> {
  /** A successful payload with nothing to show — status `empty`. */
  isEmpty?: (data: T) => boolean;
  /** How long a successful load counts as fresh for `revalidate()`. */
  freshMs?: number;
  now?: () => number;
}

function message(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}

export class Resource<T> {
  private state: ResourceState<T> = {
    data: null,
    status: "loading",
    error: null,
    asOf: null,
    inFlight: false,
  };
  private inflight: Promise<void> | null = null;
  private trailing = false;
  private readonly listeners = new Set<() => void>();
  private readonly fetcher: () => Promise<T>;
  private readonly isEmpty: (data: T) => boolean;
  private readonly freshMs: number;
  private readonly now: () => number;

  constructor(fetcher: () => Promise<T>, opts: ResourceOptions<T> = {}) {
    this.fetcher = fetcher;
    this.isEmpty = opts.isEmpty ?? (() => false);
    this.freshMs = opts.freshMs ?? 0;
    this.now = opts.now ?? Date.now;
  }

  /** Current snapshot. The object is replaced on every change. */
  get = (): ResourceState<T> => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /** Fetch now, or join the request already in flight. */
  refresh = (): Promise<void> => {
    if (this.inflight) return this.inflight;
    this.set({
      inFlight: true,
      // With nothing loaded a retry is a load, not a standing failure.
      status: this.state.data === null ? "loading" : this.state.status,
    });
    const run = this.fetcher().then(
      (data) => {
        this.set({
          data,
          status: this.isEmpty(data) ? "empty" : "ok",
          error: null,
          asOf: this.now(),
        });
      },
      (e: unknown) => {
        this.set({
          status: this.state.data === null ? "failed" : "stale",
          error: message(e),
        });
      },
    );
    this.inflight = run.finally(() => {
      this.inflight = null;
      if (this.trailing) {
        this.trailing = false;
        // Any number of invalidations during flight → one more request.
        void this.refresh();
      } else {
        this.set({ inFlight: false });
      }
    });
    return this.inflight;
  };

  /**
   * The data changed on the server. Fetch now; if a request is already in
   * flight it may predate the change, so queue one trailing request
   * (never more than one).
   */
  invalidate = (): Promise<void> => {
    if (this.inflight) {
      this.trailing = true;
      return this.inflight;
    }
    return this.refresh();
  };

  /**
   * Stale-while-revalidate: fetch unless the last success is younger than
   * `freshMs`. Joins a request in flight; never blanks loaded data.
   */
  revalidate = (): Promise<void> => {
    if (this.inflight) return this.inflight;
    const { asOf } = this.state;
    if (asOf !== null && this.state.status !== "stale" && this.now() - asOf < this.freshMs) {
      return Promise.resolve();
    }
    return this.refresh();
  };

  /**
   * Replace the data from outside a fetch — a stream frame or a write
   * response that carries the whole value. Unlike `mutate` it also works
   * before the first load, and it counts as a fresh success.
   */
  write = (fn: (data: T | null) => T): void => {
    const data = fn(this.state.data);
    this.set({
      data,
      status: this.isEmpty(data) ? "empty" : "ok",
      error: null,
      asOf: this.now(),
    });
  };

  /**
   * Apply an authoritative local change (a write response) to loaded data.
   * A no-op before the first load — the next fetch brings it.
   */
  mutate = (fn: (data: T) => T): void => {
    if (this.state.data === null) return;
    const data = fn(this.state.data);
    // Loaded data means ok, empty or stale; a write does not clear stale.
    const status = this.isEmpty(data)
      ? "empty"
      : this.state.status === "stale"
        ? "stale"
        : "ok";
    this.set({ data, status });
  };

  private set(patch: Partial<ResourceState<T>>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }
}

/**
 * Keyed stores: one `Resource` per key, created on first use and shared by
 * every later caller. The fetcher and options of the first call win — a key
 * names one server resource, so later callers would pass the same ones.
 */
export class QueryCache {
  private readonly entries = new Map<string, Resource<unknown>>();

  resource<T>(key: string, fetcher: () => Promise<T>, opts?: ResourceOptions<T>): Resource<T> {
    let entry = this.entries.get(key) as Resource<T> | undefined;
    if (!entry) {
      entry = new Resource(fetcher, opts);
      this.entries.set(key, entry as Resource<unknown>);
    }
    return entry;
  }

  /** A family of keys under one prefix: `family("issue", api.issue)("CAD-1")`. */
  family<A extends string, T>(
    prefix: string,
    fetcher: (arg: A) => Promise<T>,
    opts?: ResourceOptions<T>,
  ): (arg: A) => Resource<T> {
    return (arg) => this.resource(`${prefix}:${arg}`, () => fetcher(arg), opts);
  }

  /** The store for `key` if anything created it, else null. */
  peek<T>(key: string): Resource<T> | null {
    return (this.entries.get(key) as Resource<T> | undefined) ?? null;
  }

  /**
   * Invalidate every loaded store whose key is `key` or starts with
   * `key:`. Stores nothing has loaded stay idle until a screen asks.
   */
  invalidate(key: string): void {
    for (const [k, entry] of this.entries) {
      if ((k === key || k.startsWith(`${key}:`)) && entry.get().asOf !== null) {
        void entry.invalidate();
      }
    }
  }
}

/** Resource names the server's `/api/stream` events carry in `resources`. */
export const RESOURCE_NAMES = [
  "issues",
  "projects",
  "agents",
  "overview",
  "issue",
] as const;
export type ResourceName = (typeof RESOURCE_NAMES)[number];

/**
 * Which resources one stream event invalidates. The server names them in
 * the event data (`{"resources":["agents","overview"]}`); a server that
 * predates the field sends `{}`, and then everything is refetched — the
 * old behaviour, never a missed update.
 */
export function invalidatedBy(data: string | null | undefined): ResourceName[] {
  try {
    const parsed = JSON.parse(data ?? "") as { resources?: unknown };
    if (Array.isArray(parsed.resources)) {
      const known = new Set<string>(RESOURCE_NAMES);
      return parsed.resources.filter(
        (r): r is ResourceName => typeof r === "string" && known.has(r),
      );
    }
  } catch {
    // Unparseable data — fall through to the conservative answer.
  }
  return [...RESOURCE_NAMES];
}
