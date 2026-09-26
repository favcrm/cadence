import { invalidatedBy, type ResourceName } from "./cache";
import type { SseEvent } from "./sse";

/** One trailing batch for legacy resource invalidations; entity patches are
 * synchronous, so a later fetch cannot silently overwrite newer data.
 */
export class LiveUpdates {
  private timer: ReturnType<typeof setTimeout> | null = null;
  private readonly pending = new Set<string>();
  private seen = false;
  private live = false;
  private last = 0;
  private entities = false;
  constructor(private readonly opts: {
    invalidate: (key: string) => void;
    patch: (event: SseEvent, data: Record<string, unknown>) => void;
    resync: () => void;
    now?: () => number;
    schedule?: (fn: () => void) => ReturnType<typeof setTimeout>;
    cancel?: (timer: ReturnType<typeof setTimeout>) => void;
  }) {}
  private now = () => (this.opts.now ?? Date.now)();
  opened = () => {
    if (this.seen) this.opts.resync();
    this.seen = true;
    this.live = true;
    this.last = this.now();
  };
  failed = () => { this.live = false; };
  healthy = () => this.live && this.now() - this.last < 45_000;
  event = (event: SseEvent) => {
    if (this.live && this.now() - this.last >= 45_000) this.opts.resync();
    this.last = this.now();
    let data: Record<string, unknown>;
    try { data = JSON.parse(event.data) as Record<string, unknown>; } catch { this.opts.resync(); return; }
    if (!data || typeof data !== "object") { this.opts.resync(); return; }
    if (event.type === "hello") {
      this.entities = data.entities === true;
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
    for (const key of invalidatedBy(event.data)) this.queue(key);
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
