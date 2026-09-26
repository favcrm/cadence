import {
  buildChanged,
  parseBuild,
  subscribeSse,
  UI_BUILD,
  type EventSourceLike,
  type SseErrorState,
} from "../src/lib/sse";
import { composerField, stashDraft, takeDraft, type StorageLike } from "../src/lib/draft";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

/** An EventSource the test drives by hand. */
class FakeSource implements EventSourceLike {
  readyState = 0;
  onerror: ((e: Event) => void) | null = null;
  onopen: ((e: Event) => void) | null = null;
  closed = false;
  private readonly listeners = new Map<string, ((e: MessageEvent<string>) => void)[]>();
  constructor(readonly url: string) {}
  addEventListener(type: string, listener: (e: MessageEvent<string>) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }
  close(): void {
    this.closed = true;
    this.readyState = 2;
  }
  emit(type: string, data: string, id = ""): void {
    for (const l of this.listeners.get(type) ?? []) {
      l({ data, lastEventId: id } as MessageEvent<string>);
    }
  }
  opened(): void {
    this.readyState = 1;
    this.onopen?.({} as Event);
  }
  fail(readyState: number): void {
    this.readyState = readyState;
    this.onerror?.({} as Event);
  }
}

const flush = () => new Promise<void>((r) => setTimeout(r, 0));

/** A sessionStorage-shaped box, optionally throwing on every access. */
function fakeStore(fails = false): StorageLike & { map: Map<string, string> } {
  const map = new Map<string, string>();
  const wrap = <T>(fn: () => T): T => {
    if (fails) throw new Error("denied");
    return fn();
  };
  return {
    map,
    getItem: (k) => wrap(() => map.get(k) ?? null),
    setItem: (k, v) => wrap(() => void map.set(k, v)),
    removeItem: (k) => wrap(() => void map.delete(k)),
  };
}

async function main() {
  // ---- the build id, parsed and compared ----
  equal(parseBuild('{"build":"0.1.0+abc123"}'), "0.1.0+abc123", "hello data parses");
  equal(parseBuild('{"build":"0.1.0+abc123","x":1}'), "0.1.0+abc123", "extra fields ignored");
  equal(parseBuild("{}"), null, "no build field");
  equal(parseBuild('{"build":""}'), null, "empty build field");
  equal(parseBuild('{"build":5}'), null, "non-string build field");
  equal(parseBuild("not json"), null, "unparseable");
  equal(parseBuild(null), null, "null data");

  equal(buildChanged("0.1.0+b", "0.1.0+a"), true, "different builds → the banner shows");
  equal(buildChanged("0.1.0+a", "0.1.0+a"), false, "matching build → no banner");
  equal(buildChanged(null, "0.1.0+a"), false, "no answer → no banner");
  equal(buildChanged("", "0.1.0+a"), false, "empty answer → no banner");
  equal(buildChanged("unknown", "0.1.0+a"), false, "server unknown → cannot tell");
  equal(buildChanged("0.1.0+unknown", "0.1.0+a"), false, "server +unknown → cannot tell");
  equal(buildChanged("0.1.0+b", "0.1.0+unknown"), false, "bundle +unknown → cannot tell");
  equal(buildChanged("0.1.0+b", "unknown"), false, "bundle unknown → cannot tell");
  equal(UI_BUILD, "unknown", "no define in plain-node tests reads as cannot-tell");

  // ---- hello frames announce the build on every (re)connect ----
  {
    const opened: FakeSource[] = [];
    const builds: string[] = [];
    const due: (() => void)[] = [];
    const sub = subscribeSse({
      url: "/api/stream",
      events: ["issues"],
      onEvent: () => {},
      onBuild: (b) => builds.push(b),
      probe: async () => 503,
      schedule: (fn) => {
        due.push(fn);
        return () => {};
      },
      open: (url) => {
        const s = new FakeSource(url);
        opened.push(s);
        return s;
      },
    });
    opened[0].emit("hello", '{"build":"0.1.0+aaa"}');
    // The browser's own reconnect re-opens the SAME source — the new
    // server says its build again, and a second hello reports it.
    opened[0].emit("hello", '{"build":"0.1.0+bbb"}');
    // A hello without a build, and a dead source's hello, never report.
    opened[0].emit("hello", '{"x":1}');
    opened[0].fail(2);
    await flush();
    due.shift()!(); // the helper's reopen connects a fresh source
    const dead = opened[0];
    opened[1].emit("hello", '{"build":"0.1.0+ccc"}');
    dead.emit("hello", '{"build":"0.1.0+zzz"}');
    equal(builds, ["0.1.0+aaa", "0.1.0+bbb", "0.1.0+ccc"], "each live connect's hello reports");
    // …and the client compares each one against its bundle.
    equal(buildChanged(builds[2], "0.1.0+aaa"), true, "a rebuilt server mismatches the bundle");
    sub.close();
  }

  // ---- down ~15 s: /api/version answers for a stuck reconnect ----
  {
    const opened: FakeSource[] = [];
    const builds: string[] = [];
    const states: SseErrorState[] = [];
    const due: [ms: number, fn: () => void][] = [];
    let probes = 0;
    const sub = subscribeSse({
      url: "/api/stream",
      events: ["issues"],
      onEvent: () => {},
      onError: (s) => states.push(s),
      onBuild: (b) => builds.push(b),
      probeBuild: async () => {
        probes += 1;
        return "0.1.0+new";
      },
      buildProbeMs: 50,
      open: (url) => {
        const s = new FakeSource(url);
        opened.push(s);
        return s;
      },
      schedule: (fn, ms) => {
        due.push([ms, fn]);
        return () => {};
      },
    });
    // Healthy stream, one error, still down at the probe mark.
    opened[0].opened();
    opened[0].fail(0); // CONNECTING — the browser retries by itself
    equal(states, ["reconnecting"], "the browser's own retry shows reconnecting");
    equal(due.length, 1, "the down-probe is armed");
    equal(due[0][0], 50, "armed for the build-probe delay");
    due.shift()![1]();
    await flush();
    equal(probes, 1, "the probe ran once the stream stayed down");
    equal(builds, ["0.1.0+new"], "its answer reaches onBuild");
    // Still down: a second error does not re-arm.
    opened[0].fail(0);
    equal(due.length, 0, "one probe per down-episode");
    // Up again: a pending probe is cancelled and stale answers dropped.
    opened[0].opened();
    opened[0].fail(0);
    equal(due.length, 1, "a fresh down re-arms the probe");
    const stale = due.shift()![1];
    opened[0].opened(); // the stream lands before the timer fires
    stale();
    await flush();
    equal(probes, 1, "a reopened stream cancels the pending probe");
    // A probe with no answer reports nothing.
    let probes2 = 0;
    const sub2 = subscribeSse({
      url: "/api/stream",
      events: ["issues"],
      onEvent: () => {},
      onBuild: (b) => builds.push(b),
      probeBuild: async () => {
        probes2 += 1;
        return null; // /api/version missing or unreachable
      },
      buildProbeMs: 50,
      open: (url) => {
        const s = new FakeSource(url);
        opened.push(s);
        return s;
      },
      schedule: (fn, ms) => {
        due.push([ms, fn]);
        return () => {};
      },
    });
    opened.at(-1)!.fail(0);
    due.shift()![1]();
    await flush();
    equal(probes2, 1, "the null probe still ran");
    equal(builds, ["0.1.0+new"], "nothing reported — cannot tell, no banner");
    sub2.close();
    sub.close();
  }

  // ---- the composer draft survives the reload ----
  {
    const store = fakeStore();
    const doc = {
      querySelector: (sel: string) =>
        sel === "[data-composer] textarea"
          ? ({ value: "fix the reload banner" } as unknown as HTMLTextAreaElement)
          : null,
    };
    // The Reload click stashes exactly what the composer holds…
    stashDraft(store, composerField(doc)?.value);
    // …and the next load reads it back exactly once.
    equal(takeDraft(store), "fix the reload banner", "the draft survives the reload");
    equal(takeDraft(store), null, "the stash is single-use");
    equal(store.map.size, 0, "storage is clean after the take");
    // An empty composer leaves nothing behind (and clears a stale stash).
    stashDraft(store, "stale");
    stashDraft(store, "");
    equal(takeDraft(store), null, "an empty draft clears the stash");
    // A denied store never throws — reload still fires.
    const denied = fakeStore(true);
    stashDraft(denied, "x");
    equal(takeDraft(denied), null, "blocked storage reads as no draft");
    // No composer mounted reads as no draft to stash.
    const empty = { querySelector: () => null };
    equal(composerField(empty), null, "no composer on this route");
  }

  console.log("buildReload checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
