import { invalidatedBy, Resource, RESOURCE_NAMES } from "../src/lib/cache";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

/** A fetcher whose calls the test answers one at a time. */
function scripted<T>() {
  const pending: { resolve: (v: T) => void; reject: (e: unknown) => void }[] = [];
  let calls = 0;
  const fetcher = () => {
    calls += 1;
    return new Promise<T>((resolve, reject) => pending.push({ resolve, reject }));
  };
  return {
    fetcher,
    calls: () => calls,
    resolve: (v: T) => pending.shift()!.resolve(v),
    reject: (e: unknown) => pending.shift()!.reject(e),
  };
}

const flush = () => new Promise<void>((r) => setTimeout(r, 0));

async function main() {
  // loading → ok, with asOf stamped from the clock.
  {
    const s = scripted<number[]>();
    const r = new Resource(s.fetcher, { isEmpty: (d) => d.length === 0, now: () => 42 });
    equal(r.get().status, "loading", "initial status");
    const p = r.refresh();
    equal(r.get().status, "loading", "first request in flight");
    equal(r.get().inFlight, true, "in flight flag");
    s.resolve([1]);
    await p;
    await flush();
    equal(r.get().status, "ok", "loaded");
    equal(r.get().data, [1], "loaded data");
    equal(r.get().asOf, 42, "asOf");
    equal(r.get().inFlight, false, "settled");
  }

  // A successful empty payload is `empty`, not `failed` and not `loading`.
  {
    const s = scripted<number[]>();
    const r = new Resource(s.fetcher, { isEmpty: (d) => d.length === 0 });
    const p = r.refresh();
    s.resolve([]);
    await p;
    equal(r.get().status, "empty", "empty payload");
    equal(r.get().error, null, "empty has no error");
  }

  // ok → stale on a failed refresh: the last good payload stays.
  {
    const s = scripted<number[]>();
    const r = new Resource(s.fetcher, { now: () => 7 });
    let p = r.refresh();
    s.resolve([1, 2]);
    await p;
    p = r.refresh();
    equal(r.get().status, "ok", "refresh over data keeps ok while in flight");
    equal(r.get().data, [1, 2], "data kept while in flight");
    s.reject(new Error("503 overview timeout"));
    await p;
    equal(r.get().status, "stale", "failed refresh over data");
    equal(r.get().data, [1, 2], "stale keeps last good payload");
    equal(r.get().error, "503 overview timeout", "stale names the failure");
    equal(r.get().asOf, 7, "asOf is the last success");
    // The next success clears stale.
    p = r.refresh();
    s.resolve([3]);
    await p;
    equal(r.get().status, "ok", "recovered");
    equal(r.get().error, null, "recovered clears error");
  }

  // failed only when nothing ever loaded; a retry is a load again.
  {
    const s = scripted<number[]>();
    const r = new Resource(s.fetcher);
    let p = r.refresh();
    s.reject(new Error("connection refused"));
    await p;
    equal(r.get().status, "failed", "no data + failure");
    equal(r.get().data, null, "failed has no data");
    p = r.refresh();
    equal(r.get().status, "loading", "retry after failure is loading, not failed");
    s.resolve([9]);
    await p;
    equal(r.get().status, "ok", "retry succeeded");
  }

  // Coalescing: a second refresh while one is in flight joins it.
  {
    const s = scripted<number[]>();
    const r = new Resource(s.fetcher);
    const a = r.refresh();
    const b = r.refresh();
    const c = r.refresh();
    equal(s.calls(), 1, "concurrent refreshes share one request");
    equal(a === b && b === c, true, "joined callers share one promise");
    s.resolve([1]);
    await a;
    await flush();
    equal(s.calls(), 1, "no extra request after a joined refresh");
    equal(r.get().inFlight, false, "settled after join");
  }

  // Invalidations during flight coalesce into exactly one trailing request
  // (the in-flight answer may predate the change).
  {
    const s = scripted<number[]>();
    const r = new Resource(s.fetcher);
    const first = r.invalidate();
    void r.invalidate();
    void r.invalidate();
    void r.invalidate();
    equal(s.calls(), 1, "invalidations during flight do not stack requests");
    s.resolve([1]);
    await first;
    await flush();
    equal(s.calls(), 2, "one trailing request for many invalidations");
    equal(r.get().inFlight, true, "trailing request in flight");
    s.resolve([2]);
    await flush();
    await flush();
    equal(r.get().data, [2], "trailing request's answer wins");
    equal(r.get().inFlight, false, "settled after trailing");
    equal(s.calls(), 2, "nothing further queued");
  }

  // mutate applies an authoritative write to loaded data only.
  {
    const s = scripted<number[]>();
    const r = new Resource(s.fetcher, { isEmpty: (d) => d.length === 0 });
    r.mutate((d) => [...d, 1]);
    equal(r.get().data, null, "mutate before first load is a no-op");
    const p = r.refresh();
    s.resolve([]);
    await p;
    equal(r.get().status, "empty", "empty before write");
    r.mutate((d) => [...d, 5]);
    equal(r.get().status, "ok", "a created row leaves empty");
    equal(r.get().data, [5], "mutated data");
  }

  // Subscribers hear every transition; unsubscribe stops it.
  {
    const s = scripted<number>();
    const r = new Resource(s.fetcher);
    const seen: string[] = [];
    const off = r.subscribe(() => seen.push(r.get().status));
    const p = r.refresh();
    s.resolve(1);
    await p;
    off();
    void r.refresh();
    equal(seen.includes("ok"), true, "listener saw ok");
    const n = seen.length;
    await flush();
    equal(seen.length, n, "unsubscribed listener hears nothing");
  }

  // Stream frames: the server names resources; `{}` means refetch all.
  equal(
    invalidatedBy('{"resources":["agents","issue","overview"]}'),
    ["agents", "issue", "overview"],
    "named resources",
  );
  equal(invalidatedBy('{"resources":["monitors","overview"]}'), ["overview"], "unknown names dropped");
  equal(invalidatedBy("{}"), [...RESOURCE_NAMES], "old server frame refetches all");
  equal(invalidatedBy("not json"), [...RESOURCE_NAMES], "bad frame refetches all");
  equal(invalidatedBy(undefined), [...RESOURCE_NAMES], "missing data refetches all");
  equal(invalidatedBy('{"resources":[]}'), [], "explicit empty list refetches nothing");

  console.log("resource checks passed");
}

// An uncaught throw exits node non-zero, failing the script.
main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
