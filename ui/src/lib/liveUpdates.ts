import { invalidatedBy, type ResourceName } from "./cache";
import type { SseEvent } from "./sse";

/**
 * What one connection's `hello` claims about its push coverage (CAD-2228).
 * `unstipulated` is an older or native feed that names no liveness: the
 * timestamp rule below is its whole contract, unchanged. `stream` claims every
 * projection is pushed. `degraded` means part of the board (the host names the
 * projections in `degraded`) has no push source at all, so the recovery read
 * stays in charge for the WHOLE connection — local frames keep arriving and
 * keep `last` fresh, which must not be mistaken for full coverage.
 */
type Coverage = "unstipulated" | "stream" | "degraded";

/** Master changes invalidate immediately; other legacy resource invalidations
 * share a trailing batch. Entity patches apply synchronously.
 */
export class LiveUpdates {
  private timer: ReturnType<typeof setTimeout> | null = null;
  private readonly pending = new Set<string>();
  private live = false;
  private last = 0;
  private entities = false;
  private coverage: Coverage = "unstipulated";
  constructor(private readonly opts: {
    invalidate: (key: string) => void;
    patch: (event: SseEvent, data: Record<string, unknown>) => void;
    resync: () => void;
    onDegraded?: (degraded: boolean) => void;
    now?: () => number;
    schedule?: (fn: () => void) => ReturnType<typeof setTimeout>;
    cancel?: (timer: ReturnType<typeof setTimeout>) => void;
  }) {}
  private now = () => (this.opts.now ?? Date.now)();
  opened = () => {
    // The watcher baselines at subscription, not at the initial HTTP
    // read. First open must close that interval just like reconnects.
    this.opts.resync();
    this.live = true;
    this.last = this.now();
    // Coverage is per connection: a previous feed's claim never carries into
    // a new one, so a native `hello` without liveness keeps today's behaviour.
    this.coverage = "unstipulated";
  };
  failed = () => { this.live = false; };
  healthy = () => this.live && this.coverage !== "degraded" && this.now() - this.last < 45_000;
  event = (event: SseEvent) => {
    if (this.live && this.now() - this.last >= 45_000) this.opts.resync();
    this.last = this.now();
    let data: Record<string, unknown>;
    try {
      data = JSON.parse(event.data) as Record<string, unknown>;
    } catch {
      if (event.type === "hello") {
        this.coverage = "degraded";
        this.opts.onDegraded?.(true);
      }
      this.opts.resync();
      return;
    }
    if (!data || typeof data !== "object" || (event.type === "hello" && Array.isArray(data))) {
      if (event.type === "hello") {
        this.coverage = "degraded";
        this.opts.onDegraded?.(true);
      }
      this.opts.resync();
      return;
    }
    if (event.type === "hello") {
      this.entities = data.entities === true;
      // The host's own list of projections this feed cannot push. A non-empty
      // or malformed list withholds health no matter what `liveness` claims;
      // only an absent list or an explicitly empty one lets the liveness
      // value say. Legacy hello (no liveness, no degraded) keeps today's rule.
      const gapped = Object.hasOwn(data, "degraded")
        ? !Array.isArray(data.degraded) || data.degraded.length !== 0
        : false;
      const claims = Object.hasOwn(data, "liveness");
      this.coverage = gapped || (claims && data.liveness !== "stream")
        ? "degraded"
        : claims ? "stream" : "unstipulated";
      this.opts.onDegraded?.(this.coverage === "degraded");
      return;
    }
    if (event.type === "heartbeat") return;
    if (this.entities && event.type === "plan" && typeof data.id === "string") {
      this.queue(`issue:${data.id}`);
      return;
    }
    if (this.entities && ["issue", "agent", "agent_meta"].includes(event.type)) {
      this.opts.patch(event, data);
      if (event.type === "issue" && typeof data.id === "string") this.queue(`issue:${data.id}`);
      if (event.type === "agent_meta") this.queue("lane");
      return;
    }
    for (const key of invalidatedBy(event.data)) {
      // CAD-1773: an agent row moves on every daemon event (`event_cursor`),
      // and the `agents` frame names `apps` for the app detail's checks. The
      // installed-app list (`GET /api/apps`) holds no agent state, so an
      // agent change must not refetch it once a second; installs still
      // arrive on the `issues` frame.
      if (key === "apps" && event.type === "agents") continue;
      if (key === "master") this.opts.invalidate(key);
      else this.queue(key);
    }
  };
  private queue(key: ResourceName | string) {
    this.pending.add(key);
    if (this.timer !== null) return;
    this.timer = (this.opts.schedule ?? ((fn) => setTimeout(fn, 250)))(() => {
      this.timer = null;
      const keys = [...this.pending];
      this.pending.clear();
      for (const key of keys) this.opts.invalidate(key);
    });
  }
  close = () => {
    if (this.timer !== null) (this.opts.cancel ?? clearTimeout)(this.timer);
    this.timer = null;
    this.pending.clear();
    this.live = false;
  };
}

export function patchRows<T>(rows: T[], id: string, op: unknown, row: T, key: (row: T) => string): T[] {
  if (op === "delete") return rows.filter((value) => key(value) !== id);
  if (op !== "upsert") return rows;
  const index = rows.findIndex((value) => key(value) === id);
  if (index < 0) return [...rows, row];
  return rows.map((value, i) => i === index ? row : value);
}
