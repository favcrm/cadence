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
  private readonly now: () => number;

  constructor(fetcher: () => Promise<T>, opts: ResourceOptions<T> = {}) {
    this.fetcher = fetcher;
    this.isEmpty = opts.isEmpty ?? (() => false);
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
