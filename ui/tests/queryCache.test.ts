import { QueryCache, Resource } from "../src/lib/cache";

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
  // Dedupe: one key is one store, so two components share one request.
  {
    const cache = new QueryCache();
    const s = scripted<string[]>();
    const a = cache.resource("agents", s.fetcher);
    const b = cache.resource("agents", s.fetcher);
    equal(a === b, true, "same key → same store");
    const pa = a.revalidate();
    const pb = b.revalidate();
    equal(s.calls(), 1, "two consumers → one fetch");
    s.resolve(["pm"]);
    await pa;
    await pb;
    equal(b.get().data, ["pm"], "both consumers see the payload");
    equal(cache.peek("agents") === a, true, "peek finds the store");
    equal(cache.peek("nope"), null, "peek of an unknown key");
  }

  // Families key by argument: one store per id, shared per id.
  {
    const cache = new QueryCache();
    const seen: string[] = [];
    const issue = cache.family("issue", (id: string) => {
      seen.push(id);
      return Promise.resolve({ id });
    });
    equal(issue("CAD-1") === issue("CAD-1"), true, "same id → same store");
    equal(issue("CAD-1") === issue("CAD-2"), false, "other id → other store");
    await Promise.all([issue("CAD-1").revalidate(), issue("CAD-1").revalidate()]);
    equal(seen, ["CAD-1"], "concurrent callers of one id share a request");
  }

  // Stale-while-revalidate: data stays on screen while the refetch runs,
  // and fresh data is not refetched at all.
  {
    let now = 1_000;
    const s = scripted<number>();
    const r = new Resource(s.fetcher, { freshMs: 30_000, now: () => now });
    let p = r.revalidate();
    s.resolve(1);
    await p;
    now += 10_000;
    await r.revalidate();
    equal(s.calls(), 1, "fresh data is not refetched");
    now += 25_000;
    p = r.revalidate();
    equal(s.calls(), 2, "stale data is refetched");
    equal(r.get().status, "ok", "status stays ok while revalidating");
    equal(r.get().data, 1, "old data painted while revalidating");
    equal(r.get().inFlight, true, "revalidation shows as in flight");
    s.resolve(2);
    await p;
    await flush();
    equal(r.get().data, 2, "revalidated data replaces the old");
  }

  // Errors: failed without data, stale with data — and a stale store is
  // retried by revalidate even inside the fresh window.
  {
    let now = 0;
    const s = scripted<number>();
    const r = new Resource(s.fetcher, { freshMs: 60_000, now: () => now });
    let p = r.revalidate();
    s.reject(new Error("504 overview"));
    await p;
    equal(r.get().status, "failed", "first load failed");
    equal(r.get().error, "504 overview", "failure names the error");
    p = r.revalidate();
    equal(r.get().status, "loading", "retry after failure is a load");
    s.resolve(5);
    await p;
    now += 1;
    p = r.refresh();
    s.reject(new Error("503"));
    await p;
    equal(r.get().status, "stale", "failed refresh over data");
    equal(r.get().data, 5, "stale keeps the payload");
    p = r.revalidate();
    equal(s.calls(), 4, "a stale store revalidates inside the fresh window");
    s.resolve(6);
    await p;
    equal(r.get().status, "ok", "recovered");
  }

  // write works before the first load (a stream frame) and counts as fresh.
  {
    let now = 5;
    const s = scripted<number[]>();
    const r = new Resource(s.fetcher, { freshMs: 1_000, now: () => now });
    r.write((d) => [...(d ?? []), 1]);
    equal(r.get().data, [1], "write before load");
    equal(r.get().status, "ok", "write is a success");
    await r.revalidate();
    equal(s.calls(), 0, "written data is fresh");
    now += 2_000;
    void r.revalidate();
    equal(s.calls(), 1, "and goes stale like a fetch");
  }

  // Cache invalidation reaches loaded stores of a key or a family only.
  {
    const cache = new QueryCache();
    const hits: string[] = [];
    const make = (key: string) => cache.resource(key, () => {
      hits.push(key);
      return Promise.resolve(key);
    });
    await make("issue:CAD-1").refresh();
    await make("issue:CAD-2").refresh();
    make("issue:CAD-3"); // never loaded
    await make("issues").refresh();
    hits.length = 0;
    cache.invalidate("issue");
    await flush();
    equal(hits.sort(), ["issue:CAD-1", "issue:CAD-2"], "loaded family members only");
  }

  console.log("query cache checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
