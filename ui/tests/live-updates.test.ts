import { LiveUpdates, patchRows } from "../src/lib/liveUpdates";
import { Resource } from "../src/lib/cache";
function equal(a: unknown, b: unknown, what: string) { if (JSON.stringify(a) !== JSON.stringify(b)) throw new Error(`${what}: ${JSON.stringify(a)}`); }
let now = 0;
let timer: (() => void) | null = null;
let syncs = 0;
const invalidations: string[] = [];
const patches: string[] = [];
const live = new LiveUpdates({
  now: () => now,
  resync: () => syncs++,
  invalidate: (key) => invalidations.push(key),
  patch: (event) => patches.push(event.type),
  schedule: (fn) => { timer = fn; return 1 as unknown as ReturnType<typeof setTimeout>; },
  cancel: () => { timer = null; },
});
const emit = (type: string, data: unknown) => live.event({ type, data: JSON.stringify(data), id: null });
const flush = () => { const fn = timer; timer = null; fn?.(); };
live.opened();
emit("hello", { entities: true });
for (let i = 0; i < 60; i++) { now += 1_000; if (i % 15 === 0) emit("heartbeat", {}); equal(live.healthy(), true, "idle heartbeat suppresses full polling"); }
equal(syncs, 0, "healthy idle stream needs no resync");
for (let i = 0; i < 20; i++) emit("issues", { resources: ["projects", "overview"] });
equal(invalidations, [], "burst waits for one batch");
flush();
equal(invalidations, ["projects", "overview"], "20 events cause two resource requests");
emit("issue", { id: "CAD-1", rev: "1", op: "upsert", issue: { id: "CAD-1" } });
emit("issue", { id: "CAD-1", rev: "2", op: "delete" });
flush();
equal(patches, ["issue", "issue"], "authoritative patches applied immediately");
equal(invalidations.slice(2), ["issue:CAD-1"], "only changed detail fetched once");
live.failed(); equal(live.healthy(), false, "error re-enables fallback");
live.opened(); equal(syncs, 1, "reconnect resyncs once");
now += 45_000; equal(live.healthy(), false, "silent stream expires");
emit("heartbeat", {}); equal(syncs, 2, "long gap resync");
equal(live.healthy(), true, "heartbeat restores health");
emit("monitoring", { resources: ["overview"] });
live.close(); equal(timer, null, "unmount cancels invalidation");
equal(patchRows([{ id: "1" }, { id: "2" }], "1", "delete", { id: "1" }, (r) => r.id), [{ id: "2" }], "delete one row");
equal(patchRows([{ id: "1", rev: 1 }], "1", "upsert", { id: "1", rev: 2 }, (r) => r.id), [{ id: "1", rev: 2 }], "update one row");
// A stream patch landing during an older collection GET must survive it.
async function race() {
  let resolve!: (rows: { id: string; rev: number }[]) => void;
  let calls = 0;
  const resource = new Resource(() => { calls++; return new Promise<{ id: string; rev: number }[]>((r) => { resolve = r; }); });
  resource.write(() => [{ id: "1", rev: 1 }]);
  const request = resource.refresh();
  resource.mutate((rows) => patchRows(rows, "1", "upsert", { id: "1", rev: 2 }, (r) => r.id));
  resolve([{ id: "1", rev: 1 }]); await request;
  equal(resource.get().data, [{ id: "1", rev: 2 }], "older GET cannot undo patch");
  equal(calls, 2, "race schedules one trailing reconciliation");
  resolve([{ id: "1", rev: 2 }]);
  console.log("live updates checks passed");
}
race().catch((error) => { setTimeout(() => { throw error; }); });
