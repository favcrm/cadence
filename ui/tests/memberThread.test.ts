import { Resource } from "../src/lib/cache";
import { memberHistory, memberWrite, type MemberThreadState } from "../src/features/home/memberThread";

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
  // CAD-1485 round 5 (fixed by 8c0c5b5b): a local write while the FIRST
  // history read is in flight bumps `Resource.generation`, superseding
  // that read. The Resource queues a trailing read only when a
  // superseded request RESOLVES (cacheCore's success branch); a
  // superseded REJECTION drops on the same generation check and
  // schedules nothing — so with no successful read yet the panel would
  // sit on "Reading the thread…" forever: the dropped failure is never
  // recorded, the poll is gated on `loaded`, and nothing else
  // invalidates `memberThread`. `memberWrite` therefore calls
  // `invalidate()` after every pre-load write, marking the flight
  // trailing so the Resource's finally runs exactly one fresh read
  // whether the dropped request resolved or rejected.
  // Negative control: remove the `invalidate()` block from memberWrite
  // — the fetcher is then called exactly once and the store never
  // converges.
  {
    const s = scripted<MemberThreadState>();
    const store = new Resource<MemberThreadState>(s.fetcher);
    // 1. Start the first history read.
    const first = store.refresh();
    // 2. One fetch in flight.
    equal(s.calls(), 1, "first refresh started one read");
    // 3. A local write lands while the read is in flight — an optimistic
    //    pending row — bumping the Resource generation.
    memberWrite(store, (cur) =>
      ({
        ...(cur ?? { exists: true, entries: [], pending: [] }),
        pending: [
          { message: "m1", text: "hello", at: 1, state: "sending" },
        ],
      }) as MemberThreadState,
    );
    // 4. A local write never proves a read: `loaded` stays false even
    //    though `store.write` stamped status=ok on the payload.
    equal(memberHistory(store.get()).loaded, false, "local write does not stamp loaded");
    // 5. The superseded read now REJECTS — its failure is dropped by the
    //    generation check and would record nothing.
    s.reject(new Error("connection refused"));
    // 6. Settle the original refresh and let the finally tick run.
    await first;
    await flush();
    // 7. The superseded rejection queues exactly one fresh read via the
    //    trailing path (cacheCore.ts:164-167); the dropped failure is
    //    not recorded as this read's error, and `loaded` is still false.
    equal(s.calls(), 2, "superseded rejection queued one trailing read");
    equal(store.get().error, null, "dropped rejection was not recorded as this read's error");
    equal(memberHistory(store.get()).loaded, false, "still no successful read");
    // 8. Resolve the trailing read with a loader-stamped result — the
    //    only thing that can flip `loaded`.
    s.resolve({ exists: true, entries: [], pending: [], history: { loaded: true } } as MemberThreadState);
    await flush();
    await flush();
    equal(memberHistory(store.get()).loaded, true, "the trailing read stamps loaded");
    equal(s.calls(), 2, "nothing further queued");
  }

  console.log("memberThread checks passed");
}

// An uncaught throw exits node non-zero, failing the script.
main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
