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
equal(syncs, 1, "first open reconciles once; steady healthy idle needs no more resync");
for (let i = 0; i < 20; i++) {
  emit("issues", { resources: [] });
  emit("aggregates", { resources: [] });
}
flush();
equal(invalidations, [], "unchanged title-only aggregate snapshots cause no projects or overview reads");
for (let i = 0; i < 20; i++) emit("aggregates", { resources: ["projects", "overview"] });
equal(invalidations, [], "burst waits for one batch");
flush();
equal(invalidations, ["projects", "overview"], "20 events cause two resource requests");
emit("issue", { id: "CAD-1", rev: "1", op: "upsert", issue: { id: "CAD-1" } });
emit("issue", { id: "CAD-1", rev: "2", op: "delete" });
flush();
equal(patches, ["issue", "issue"], "authoritative patches applied immediately");
equal(invalidations.slice(2), ["issue:CAD-1"], "only changed detail fetched once");
live.failed(); equal(live.healthy(), false, "error re-enables fallback");
live.opened(); equal(syncs, 2, "reconnect resyncs once");
now += 45_000; equal(live.healthy(), false, "silent stream expires");
emit("heartbeat", {}); equal(syncs, 3, "long gap resync");
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
}
// An initial GET predates the watcher's first subscription baseline. No
// entity event can recover this change: it is already in that baseline.
async function startupGap(initialAlreadyLanded: boolean) {
  let now = 0;
  const replies: ((rows: { id: string; rev: number }[]) => void)[] = [];
  const resource = new Resource(() => new Promise<{ id: string; rev: number }[]>((resolve) => { replies.push(resolve); }));
  const initial = resource.refresh();
  if (initialAlreadyLanded) {
    replies[0]([{ id: "1", rev: 1 }]);
    await initial;
  }
  const updates = new LiveUpdates({
    now: () => now,
    // Same ordering as App's reconciliation: invalidate first, then its
    // regular refresh joins. Joining alone would not queue a trailing GET.
    resync: () => { void resource.invalidate(); void resource.refresh(); },
    invalidate: () => { throw new Error("there are no diff events in this scenario"); },
    patch: () => { throw new Error("change was absorbed into the subscription baseline"); },
  });
  updates.opened();
  if (!initialAlreadyLanded) {
    equal(replies.length, 1, "first open coalesces with the outstanding initial GET");
    replies[0]([{ id: "1", rev: 1 }]);
    await initial;
  }
  equal(replies.length, 2, "first open requires a post-subscription read even when initial GET is outstanding");
  replies[1]([{ id: "1", rev: 2 }]);
  await Promise.resolve();
  await Promise.resolve();
  equal(resource.get().data, [{ id: "1", rev: 2 }], "first subscription closes startup baseline gap");
  for (let i = 0; i < 4; i++) {
    now += 15_000;
    updates.event({ type: "heartbeat", data: "{}", id: null });
    equal(updates.healthy(), true, "healthy idle after startup reconciliation");
  }
  equal(replies.length, 2, "idle heartbeats cause no additional reads after startup reconciliation");
  updates.close();
}
async function main() {
  await race();
  await startupGap(true);
  await startupGap(false);
  console.log("live updates checks passed");
}
main().catch((error) => { setTimeout(() => { throw error; }); });
