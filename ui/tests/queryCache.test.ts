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

  // A local write during a fetch wins: the older answer is dropped and one
  // trailing request confirms against the server.
  {
    const s = scripted<string>();
    const r = new Resource(s.fetcher);
    const p = r.refresh();
    r.write(() => "v2");
    s.resolve("v1");
    await p;
    equal(r.get().data, "v2", "late fetch does not overwrite a write");
    equal(s.calls(), 2, "one trailing request after the dropped answer");
    s.resolve("v3");
    await flush();
    await flush();
    equal(r.get().data, "v3", "the trailing answer lands");
    equal(r.get().inFlight, false, "settled");
  }

  // The drawer case: save (write), a stream frame (write), then the reload
  // joins the in-flight fetch — the saved detail must survive.
  {
    const s = scripted<{ rev: number }>();
    const r = new Resource(s.fetcher);
    let p = r.refresh();
    s.resolve({ rev: 1 });
    await p;
    p = r.refresh(); // poll in flight, answered from before the save
    r.write(() => ({ rev: 2 })); // write response
    r.write((d) => ({ rev: (d?.rev ?? 0) + 1 })); // stream frame
    void r.invalidate(); // loadDetail joins the old request
    s.resolve({ rev: 1 });
    await p;
    equal(r.get().data, { rev: 3 }, "no revert to the pre-save payload");
    s.resolve({ rev: 3 }); // the one trailing request
    await flush();
    await flush();
    equal(s.calls(), 3, "joined reload + dropped answer → one trailing request");
    // A failure of a request older than a write does not mark it stale.
    p = r.refresh();
    r.write(() => ({ rev: 4 }));
    s.reject(new Error("late 503"));
    await p;
    equal(r.get().status, "ok", "an older failure does not stale fresh data");
    equal(r.get().error, null, "and records no error");
    equal(s.calls(), 4, "a failed older request queues nothing");
  }

  // Same for an optimistic mutate (a card drag) during a poll.
  {
    const s = scripted<number[]>();
    const r = new Resource(s.fetcher);
    let p = r.refresh();
    s.resolve([1]);
    await p;
    p = r.refresh();
    r.mutate((d) => [...d, 2]);
    s.resolve([1]);
    await p;
    equal(r.get().data, [1, 2], "optimistic change kept over an older answer");
  }

  // Invalidation refetches only stores someone watches; the others refetch
  // when a screen revalidates them. Request counts stay bounded.
  {
    const cache = new QueryCache({ maxIdle: 100 });
    let hits = 0;
    const issue = cache.family(
      "issue",
      (id: string) => {
        hits += 1;
        return Promise.resolve(id);
      },
      { freshMs: 60_000 },
    );
    for (let n = 0; n < 50; n++) await issue(`CAD-${n}`).refresh();
    const off = issue("CAD-7").subscribe(() => {});
    hits = 0;
    cache.invalidate("issue");
    cache.invalidate("issue");
    cache.invalidate("issue");
    await flush();
    await flush();
    equal(hits <= 2, true, `three invalidations of 50 stores cost ${hits} requests`);
    equal(hits >= 1, true, "the watched store refetched");
    hits = 0;
    await issue("CAD-3").revalidate();
    equal(hits, 1, "a marked store refetches when shown again");
    await issue("CAD-4").revalidate();
    await issue("CAD-4").revalidate();
    equal(hits, 2, "and only once");
    off();
    // A plain key is invalidated the same way.
    let plain = 0;
    const list = cache.resource("issues", () => Promise.resolve(++plain), { freshMs: 60_000 });
    await list.refresh();
    cache.invalidate("issues");
    await flush();
    equal(plain, 1, "unwatched list is only marked");
  }

  // Idle family stores are evicted beyond maxIdle, oldest first; watched
  // stores and plain keys stay.
  {
    const cache = new QueryCache({ maxIdle: 3 });
    const issue = cache.family("issue", (id: string) => Promise.resolve(id));
    const pinned = cache.resource("issues", () => Promise.resolve([]));
    const watched = issue("CAD-0");
    const off = watched.subscribe(() => {});
    for (let n = 1; n <= 10; n++) issue(`CAD-${n}`);
    equal(cache.peek("issue:CAD-1"), null, "oldest idle store evicted");
    equal(cache.peek("issue:CAD-10") !== null, true, "newest kept");
    equal(cache.peek("issue:CAD-8") !== null, true, "within the cap kept");
    equal(cache.peek("issue:CAD-7"), null, "beyond the cap evicted");
    equal(cache.peek("issue:CAD-0") === watched, true, "watched store kept");
    equal(cache.peek("issues") === pinned, true, "plain key kept");
    off();
  }

  console.log("query cache checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
