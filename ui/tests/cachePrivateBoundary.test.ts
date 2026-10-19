import { QueryCache, Resource } from "../src/lib/cache";
import {
  allowPrivateReads,
  pausePrivateReads,
  privateReadsAllowed,
  resetPrivateSession,
} from "../src/lib/privateSession";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

/** Host admission and retirement must survive replacing generic cache internals. */
async function main(): Promise<void> {
  const originallyAllowed = privateReadsAllowed();
  let unsubscribe = () => {};
  try {
    const cache = new QueryCache();
    let directCalls = 0;
    let cachedCalls = 0;
    let reply = Promise.resolve("synthetic-initial-observation");
    const direct = new Resource(async () => {
      directCalls += 1;
      return "synthetic-direct-observation";
    });
    const cached = cache.resource("synthetic-private-boundary", () => {
      cachedCalls += 1;
      return reply;
    }, { now: () => 42 });
    const observed: Array<string | null> = [];
    unsubscribe = cached.subscribe(() => { observed.push(cached.get().data); });

    // Real Native admission, not a mocked predicate or an authenticated API.
    pausePrivateReads();
    const pausedDirect = direct.get();
    const pausedCached = cached.get();
    await Promise.all([direct.refresh(), cached.refresh()]);
    equal(directCalls, 0, "paused Host Resource must not invoke its fetcher");
    equal(cachedCalls, 0, "paused Host QueryCache resource must not invoke its fetcher");
    equal(direct.get(), pausedDirect, "refused direct read leaves its snapshot untouched");
    equal(cached.get(), pausedCached, "refused cached read leaves its snapshot untouched");

    allowPrivateReads();
    await cached.refresh();
    equal(cachedCalls, 1, "an admitted Host cache read actually invokes its fetcher");
    equal(cached.get().data, "synthetic-initial-observation", "admitted data is observable");

    // A pause keeps the last same-session observation, but retires pending work.
    const beforePause = deferred<string>();
    reply = beforePause.promise;
    const pausedRequest = cached.refresh();
    equal(cachedCalls, 2, "the request is admitted before pause");
    equal(cached.get().inFlight, true, "the admitted request is pending");
    pausePrivateReads();
    const afterPause = cached.get();
    equal(afterPause.data, "synthetic-initial-observation", "pause retains the existing observation");
    equal(afterPause.inFlight, false, "pause retires the pending cache request");
    beforePause.resolve("synthetic-retired-at-pause");
    await pausedRequest;
    equal(cached.get(), afterPause, "a late paused result cannot replace the retained observation");
    equal(cachedCalls, 2, "settling retired work must not start another fetch while paused");

    // Reset drops observations; re-admission must not revive the old request.
    allowPrivateReads();
    const beforeReset = deferred<string>();
    reply = beforeReset.promise;
    const resetRequest = cached.refresh();
    equal(cachedCalls, 3, "the old-session request is admitted before reset");
    equal(cached.get().inFlight, true, "the old-session request is pending");
    resetPrivateSession();
    const afterReset = cached.get();
    equal(afterReset.data, null, "Native reset drops the Host cache observation");
    equal(afterReset.asOf, null, "Native reset retires success freshness");
    allowPrivateReads();
    beforeReset.resolve("synthetic-retired-at-reset");
    await resetRequest;
    equal(cached.get(), afterReset, "re-admission cannot let an old-session result repopulate the cache");
    equal(cachedCalls, 3, "the retired old-session request schedules no replacement work");
    equal(observed.includes("synthetic-retired-at-pause"), false, "no subscriber sees the paused result");
    equal(observed.includes("synthetic-retired-at-reset"), false, "no subscriber sees the reset result");

    // A blanket no-op is not a refusal implementation: current reads must work.
    reply = Promise.resolve("synthetic-replacement-observation");
    await cached.refresh();
    equal(cachedCalls, 4, "a legitimate replacement-session read invokes its fetcher");
    equal(cached.get().data, "synthetic-replacement-observation", "the current session can populate the same resource handle after reset");
    equal(cached.get().status, "ok", "the replacement session has an admitted observation");
    equal(cached.get().asOf, 42, "replacement-session freshness belongs to its successful read");
    console.log("Host cache private-boundary acceptance passed");
  } finally {
    unsubscribe();
    resetPrivateSession();
    if (originallyAllowed) allowPrivateReads();
  }
}

// Match the plain-Node cache checks: an uncaught failure exits non-zero.
main().catch((error: unknown) => {
  console.error(error);
  queueMicrotask(() => { throw error; });
});
