import { Resource } from "../src/lib/cache";
import { streamInto, type EventSourceLike } from "../src/lib/sse";
import {
  addPending,
  discardPending,
  EMPTY_THREAD,
  lastSeq,
  applyEarlier,
  fetchEarlier,
  loadThread,
  mergePage,
  PAGE,
  visibleWindow,
  WINDOW,
  type PageReader,
  newMessageId,
  planAnchors,
  planRef,
  reduceFrame,
  settlePending,
  threadItems,
  threadRuns,
  inFlightTurnKey,
  itemMessage,
  sendBefore,
  rowPaintKey,
  type ThreadState,
} from "../src/features/home/thread";
import { composerBlock, masterStatus } from "../src/features/home/master";
import type { AgentsPayload } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function entry(seq: number, role: string, kind: string, text: string, extra: Record<string, unknown> = {}) {
  return { seq, thread: "t1", role, kind, text, payload: null, message: null, created: "2026-09-24T10:00:00Z", ...extra };
}

/** An EventSource the test drives by hand (as in sse.test.ts). */
class FakeSource implements EventSourceLike {
  readyState = 0;
  onerror: ((e: Event) => void) | null = null;
  onopen: ((e: Event) => void) | null = null;
  private readonly listeners = new Map<string, ((e: MessageEvent<string>) => void)[]>();
  constructor(readonly url: string) {}
  addEventListener(type: string, listener: (e: MessageEvent<string>) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }
  close(): void {
    this.readyState = 2;
  }
  emit(type: string, data: string, id = ""): void {
    for (const l of this.listeners.get(type) ?? []) l({ data, lastEventId: id } as MessageEvent<string>);
  }
  fail(readyState: number): void {
    this.readyState = readyState;
    this.onerror?.({} as Event);
  }
}

const flush = () => new Promise<void>((r) => setTimeout(r, 0));

async function main() {
  // Rendering by kind: operator message, commentary, collapsed tool run,
  // the answer; the daemon's system line; pending last.
  {
    let s = mergePage(null, {
      thread: { id: "t1" },
      entries: [
        entry(1, "operator", "message", "plan a feature", { message: "m1" }),
        entry(2, "agent", "assistant_text", "Looking at the tracker first."),
        entry(3, "agent", "tool_call", "Bash: cadence issue ls"),
        entry(4, "agent", "tool_result", "12 issues", { payload: { is_error: false } }),
        entry(5, "agent", "tool_call", "Bash: cadence plan propose demo"),
        entry(6, "agent", "turn_result", "Here is the plan."),
        entry(7, "system", "message", "Since 2026-09-23T00:00:00Z: …"),
      ],
    });
    s = addPending(s, "m2", "and a second", 1);
    const items = threadItems(s);
    equal(
      items.map((i) => i.type),
      ["operator", "commentary", "tools", "answer", "system", "pending"],
      "items by kind",
    );
    const tools = items[2];
    equal(tools.type === "tools" ? tools.entries.map((e) => e.seq) : null, [3, 4, 5], "one collapsed tool run");
    equal(s.exists, true, "a page with a thread exists");
    equal(lastSeq(s), 7, "cursor");
  }

  // No thread yet: `thread: null` and no entries — the three-examples state.
  {
    const s = mergePage(null, { thread: null, entries: [] });
    equal([s.exists, s.entries.length], [false, 0], "no thread yet");
    equal(threadItems(s), [], "nothing to render");
  }

  // Reconnect: frames merge by seq — a replayed frame is not repeated, an
  // out-of-order one is placed in order, and the reopened source resumes
  // with ?after=<last id>, so no gap either.
  {
    const store = new Resource<ThreadState>(() => Promise.resolve(EMPTY_THREAD));
    store.write(() => mergePage(null, { thread: { id: "t1" }, entries: [entry(1, "operator", "message", "hi", { message: "a" })] }));
    const opened: FakeSource[] = [];
    const due: (() => void)[] = [];
    const sub = streamInto(store, reduceFrame, {
      url: "/api/threads/master/stream",
      events: ["entry"],
      lastEventId: String(lastSeq(store.get().data)),
      open: (url) => {
        const s = new FakeSource(url);
        opened.push(s);
        return s;
      },
      probe: () => Promise.resolve(200),
      schedule: (fn) => {
        due.push(fn);
        return () => {};
      },
    });
    equal(opened[0].url, "/api/threads/master/stream?after=1", "resumes after the loaded entries");
    opened[0].emit("entry", JSON.stringify(entry(2, "agent", "assistant_text", "thinking")), "2");
    opened[0].emit("entry", JSON.stringify(entry(3, "agent", "turn_result", "done")), "3");
    opened[0].emit("entry", "not json", "");
    opened[0].fail(2);
    await flush();
    due.shift()!();
    equal(opened[1].url, "/api/threads/master/stream?after=3", "reopened after the last id");
    // A server that replays the boundary, then new entries out of order.
    opened[1].emit("entry", JSON.stringify(entry(3, "agent", "turn_result", "done")), "3");
    opened[1].emit("entry", JSON.stringify(entry(5, "agent", "turn_result", "five")), "5");
    opened[1].emit("entry", JSON.stringify(entry(4, "operator", "message", "four", { message: "b" })), "4");
    equal(
      store.get().data!.entries.map((e) => e.seq),
      [1, 2, 3, 4, 5],
      "no duplicates, no gaps, seq order",
    );
    // Frames of other types are ignored by the reducer.
    equal(reduceFrame(store.get().data, { type: "error", data: "{}", id: null }), store.get().data, "error frame");
    sub.close();
  }

  // Optimistic send: shown at once, marked sent by the POST, retired by
  // the stored operator entry carrying the same message id; a failure
  // keeps the text with its error, a retry reuses the id, discard drops it.
  {
    let s = mergePage(null, { thread: { id: "t1" }, entries: [entry(1, "agent", "turn_result", "hello")] });
    s = addPending(s, "ui-1", "build it", 10);
    equal(threadItems(s).map((i) => i.type), ["answer", "pending"], "optimistic entry shows");
    s = settlePending(s, "ui-1", { ok: true });
    equal(s.pending[0].state, "sent", "POST accepted");
    s = reduceFrame(s, { type: "entry", data: JSON.stringify(entry(2, "operator", "message", "build it", { message: "ui-1" })), id: "2" });
    equal(s.pending, [], "stored entry retires the pending one");
    equal(threadItems(s).map((i) => i.type), ["answer", "operator"], "shown once");

    s = addPending(s, "ui-2", "again", 20);
    s = settlePending(s, "ui-2", { ok: false, error: "board is read-only" });
    equal([s.pending[0].state, s.pending[0].error], ["failed", "board is read-only"], "failure kept");
    s = addPending(s, "ui-2", "again", 30);
    equal([s.pending.length, s.pending[0].state], [1, "sending"], "retry reuses the id");
    s = discardPending(s, "ui-2");
    equal(s.pending, [], "discarded");
    // An agent's (not the operator's) entry with the same id retires nothing.
    s = addPending(s, "ui-3", "x", 40);
    s = mergePage(s, { entries: [entry(9, "system", "message", "x", { message: "ui-3" })] });
    equal(s.pending.length, 1, "only the operator entry reconciles");
    const id = newMessageId(() => 0.5, () => 1000);
    equal(/^ui-[0-9a-z]+-[0-9a-z]+$/.test(id), true, "message id shape");
  }

  // Loading: the NEWEST page first (tail), never the history from the
  // start; a refetch catches up forward, a long gap reopens on the tail;
  // earlier pages fold in on demand; 404 is "master not started".
  {
    const all = Array.from({ length: 10_000 }, (_, i) => entry(i + 1, i % 2 ? "agent" : "operator", i % 2 ? "turn_result" : "message", `m${i + 1}`));
    const calls: string[] = [];
    const server = (): PageReader => ({
      after: async (after, limit) => {
        calls.push(`after:${after}`);
        return { thread: { id: "t1" }, entries: all.filter((e) => e.seq > after).slice(0, limit) };
      },
      tail: async (limit) => {
        calls.push(`tail:${limit}`);
        return { thread: { id: "t1" }, entries: all.slice(-limit), more_before: all.length > limit };
      },
      before: async (before, limit) => {
        calls.push(`before:${before}`);
        const older = all.filter((e) => e.seq < before);
        return { thread: { id: "t1" }, entries: older.slice(-limit), more_before: older.length > limit };
      },
    });
    const read = server();
    let s = await loadThread(read, null);
    equal(calls, [`tail:${PAGE}`], "opens on the tail");
    equal([s.entries[0].seq, lastSeq(s), s.moreBefore], [10_000 - PAGE + 1, 10_000, true], "newest page");
    // Stream resumes from the newest seq, not from 0.
    equal(lastSeq(s), 10_000, "resume cursor is the newest seq");

    // The rendered window stays bounded however long the thread is.
    const everything = mergePage(null, { thread: { id: "t1" }, entries: all });
    const items = threadItems(everything);
    const { shown, hidden } = visibleWindow(items);
    equal(shown.length <= WINDOW, true, "rendered items stay within the window");
    equal(shown.length + hidden, items.length, "hidden above");
    equal(shown[shown.length - 1], items[items.length - 1], "window ends at the newest");
    equal(visibleWindow(items.slice(0, 3)).hidden, 0, "short thread shows all");

    // Earlier: one page below the first held seq, merged into the
    // current state (a frame that landed meanwhile is kept).
    const older = await fetchEarlier(read, s)!;
    equal(calls[calls.length - 1], `before:${10_000 - PAGE + 1}`, "before the first held seq");
    const live = reduceFrame(s, { type: "entry", data: JSON.stringify(entry(10_001, "agent", "turn_result", "new")), id: "10001" });
    s = applyEarlier(live, older);
    equal([s.entries.length, s.entries[0].seq, lastSeq(s), s.moreBefore], [2 * PAGE + 1, 10_000 - 2 * PAGE + 1, 10_001, true], "earlier merged, live kept");
    const first = mergePage(null, { thread: { id: "t" }, entries: [entry(1, "operator", "message", "a")] });
    equal(fetchEarlier(read, first), null, "nothing before seq 1");

    // Refetch with entries held: forward catch-up; a long gap reopens on
    // the tail, keeping pending messages.
    calls.length = 0;
    const near = addPending(mergePage(null, { thread: { id: "t1" }, entries: all.slice(-5, -2) }), "p", "keep me", 1);
    const caught = await loadThread(read, near);
    equal(calls, [`after:${10_000 - 2}`], "one forward page");
    equal([lastSeq(caught), caught.pending.length], [10_000, 1], "caught up, pending kept");
    calls.length = 0;
    const far = addPending(mergePage(null, { thread: { id: "t1" }, entries: all.slice(0, 3) }), "p", "keep me", 1);
    const reopened = await loadThread(read, far);
    equal(calls, ["after:3", `after:${3 + PAGE}`, `tail:${PAGE}`], "gap too long → tail");
    equal([reopened.entries[0].seq, reopened.pending.length], [10_000 - PAGE + 1, 1], "reopened, pending kept");

    // A daemon without backward reads answers a forward page: kept, and
    // "more before" is unknown rather than wrong.
    const old: PageReader = {
      after: async () => ({ thread: { id: "t" }, entries: [] }),
      tail: async () => ({ thread: { id: "t" }, entries: all.slice(0, 3) }),
      before: async () => ({ thread: { id: "t" }, entries: [] }),
    };
    const fallback = await loadThread(old, null);
    equal([fallback.entries.length, fallback.moreBefore], [3, null], "old daemon degrades");

    const fail = (status: number): PageReader => {
      const no = () => Promise.reject(Object.assign(new Error("x"), { status }));
      return { after: no, tail: no, before: no };
    };
    const gone = await loadThread(fail(404), null);
    equal([gone.missing, gone.exists], [true, false], "404 → missing");
    let threw = false;
    await loadThread(fail(503), null).catch(() => {
      threw = true;
    });
    equal(threw, true, "a 503 is a failure");
  }

  // CAD-1853: paging past rows a projection filtered out. The daemon reports
  // what it SCANNED, so a page that kept nothing still advances, and the
  // forward catch-up trusts the raw-page answer instead of the visible count.
  {
    const calls: string[] = [];
    const filtered: PageReader = {
      after: async (after, limit) => {
        calls.push(`after:${after}`);
        if (after === 100) {
          // A full raw page the projection reduced to one entry.
          return {
            thread: { id: "t1" },
            entries: [entry(101, "agent", "turn_result", "kept")].slice(0, limit),
            cursor: 101,
            more_after: true,
          };
        }
        return {
          thread: { id: "t1" },
          entries: [entry(102, "agent", "turn_result", "next")].slice(0, limit),
          cursor: 102,
          more_after: false,
        };
      },
      tail: async () => {
        calls.push("tail");
        // The newest page is entirely another source's: nothing kept, but the
        // scan still stopped at 901.
        return {
          thread: { id: "t1" },
          entries: [],
          cursor: 1000,
          scanned_before: 901,
          more_before: true,
        };
      },
      before: async (before, limit) => {
        calls.push(`before:${before}`);
        if (before === 901) {
          return {
            thread: { id: "t1" },
            entries: [entry(850, "operator", "message", "older")].slice(0, limit),
            cursor: 899,
            scanned_before: 801,
            more_before: true,
          };
        }
        return {
          thread: { id: "t1" },
          entries: [],
          cursor: 800,
          scanned_before: 701,
          more_before: false,
        };
      },
    };

    // The forward catch-up keeps going on `more_after` even though the kept
    // page was short: the RAW scan filled the page.
    const held = mergePage(null, {
      thread: { id: "t1" },
      entries: [entry(100, "operator", "message", "start")],
    });
    const caught = await loadThread(filtered, held, 2);
    equal(calls, ["after:100", "after:101"], "a full raw page keeps catching up");
    equal(lastSeq(caught), 102, "catch-up ends on the daemon's exhausted page");
    calls.length = 0;

    // A fully filtered tail still carries a backward boundary: older history
    // stays reachable even with nothing held.
    let s = await loadThread(filtered, null, 100);
    equal(
      [s.entries.length, s.older, s.moreBefore],
      [0, 901, true],
      "a filtered tail keeps its scanned boundary",
    );
    const older = await fetchEarlier(filtered, s, 100)!;
    equal(calls, ["tail", "before:901"], "earlier continues below the scan, not from an empty page");
    s = applyEarlier(s, await older);
    equal(
      [s.entries.map((e) => e.seq), s.older, s.moreBefore],
      [[850], 801, true],
      "a kept page moves the boundary down",
    );
    // The next press continues below the SCAN (801), never from the held entry
    // (850): otherwise a partially filtered page is read again.
    const bottom = await fetchEarlier(filtered, s, 100)!;
    equal(calls[calls.length - 1], "before:801", "the next press continues below the scan");
    s = applyEarlier(s, await bottom);
    equal(
      [s.entries.map((e) => e.seq), s.older, s.moreBefore],
      [[850], 701, false],
      "a fully filtered page still advances and reports exhaustion",
    );
  }

  // CAD-1853: availability is independent of rows and never retains cached
  // cloud history. Server provenance, not kind/alias/text, selects eviction.
  {
    const ordinary = entry(20, "operator", "message", "cloud_agent is just text");
    const cloud = entry(10, "agent", "turn_result", "private", { payload: { cloud_agent: {} } });
    const request = entry(11, "operator", "message", "cloud request", { payload: { source: "cloud_agent" } });
    let s = addPending(mergePage(null, {
      thread: { id: "t1" }, entries: [cloud, request, ordinary], reply_seq: 50,
    }), "waiting", "ordinary pending", 1);
    s = { ...s, cursor: 100, older: 10, moreBefore: true };
    s = mergePage(s, { entries: [], cloud_withheld: "revoked" });
    equal(
      [s.cloudWithheld, s.entries.map((e) => e.seq), s.pending.map((p) => p.message), s.cursor, s.older, s.replySeq],
      ["revoked", [20], ["waiting"], 100, 10, 50],
      "denial evicts both cloud provenance shapes but preserves ordinary history, pending and watermarks",
    );
    s = reduceFrame(s, { type: "entry", data: JSON.stringify(cloud), id: "10" });
    equal(s.entries.map((e) => e.seq), [20], "a replay cannot put cloud history back while withheld");
    s = reduceFrame(s, {
      type: "entry", data: JSON.stringify({ ...ordinary, text: "ordinary delivery updated" }), id: "20",
    });
    equal(s.entries[0].text, "ordinary delivery updated", "overlapping entries still update while cloud is denied");
    for (const data of ['{"cloud_withheld":"future-code"}', "{}", "null", "not json", '{"cloud_withheld":false}', '{"cloud_withheld":""}']) {
      s = reduceFrame(s, { type: "cloud_withheld", data, id: null });
      equal(s.cloudWithheld != null && s.entries.every((e) => e.seq === 20), true, "unknown/malformed status never restores cloud access");
    }
    s = reduceFrame(s, { type: "cloud_withheld", data: '{"cloud_withheld":null}', id: null });
    equal([s.cloudWithheld, s.cloudRefreshNeeded], [null, true], "explicit null clears the notice and requests restoration");
    const calls: string[] = [];
    s = await loadThread({
      after: async () => { throw new Error("restoration must not skip evicted rows via catch-up"); },
      tail: async () => {
        calls.push("tail");
        return { thread: { id: "t1" }, entries: [cloud, request], cursor: 100, more_before: true, scanned_before: 10 };
      },
      before: async () => { throw new Error("unexpected earlier read"); },
    }, s);
    equal(
      [calls, s.cloudWithheld, s.cloudRefreshNeeded, s.entries.map((e) => e.seq), s.pending.map((p) => p.message), s.replySeq],
      [["tail"], null, false, [10, 11, 20], ["waiting"], 50],
      "restoration rereads the tail and merges ordinary rows/pending without lowering the complete reply watermark",
    );
    const empty = reduceFrame(null, { type: "cloud_withheld", data: '{"cloud_withheld":"unreachable"}', id: null });
    equal([empty.cloudWithheld, empty.entries, empty.pending], ["unreachable", [], []], "status-only frames work before any entry");
    const clean = mergePage(empty, { thread: { id: "t1" }, entries: [ordinary] });
    equal([clean.cloudWithheld, clean.entries.map((e) => e.seq)], [null, [20]], "a clean page also clears unavailable status");
  }

  // Plan cards: each epic once, at its first mention.
  {
    equal(planRef(entry(1, "agent", "tool_result", '{"epic": "D-2", "tickets": ["D-3"]}')), "D-2", "json epic");
    equal(planRef(entry(1, "agent", "tool_result", '{"id": "D-2", "plan": {"state": "proposed"}}')), null, "plan block without epic key");
    equal(planRef(entry(1, "agent", "tool_result", '{"epic": "D-2", "plan": {"state": "proposed"}}')), "D-2", "plan show shape");
    equal(planRef(entry(1, "agent", "tool_result", '{"epic": "D-8", "stage": "build"}')), null, "epic stage output is not a plan");
    equal(planRef(entry(1, "agent", "tool_call", "Bash: cadence plan show CAD-12 --json")), "CAD-12", "plan command");
    equal(planRef(entry(1, "agent", "turn_result", "I proposed plan D-7 for you.")), "D-7", "prose");
    equal(planRef(entry(1, "agent", "turn_result", "D-7 is done.")), null, "an id alone is not a plan");
    equal(planRef(entry(1, "operator", "message", "approve plan D-7")), null, "the operator's words are not a card");
    equal(planRef(entry(1, "system", "message", "x", { payload: { epic: "D-9" } })), "D-9", "payload epic");
    const items = threadItems(
      mergePage(null, {
        thread: { id: "t" },
        entries: [
          entry(1, "agent", "tool_call", "Bash: cadence plan propose demo"),
          entry(2, "agent", "tool_result", '{"epic":"D-2","tickets":["D-3"]}'),
          entry(3, "agent", "turn_result", "Plan D-2 is ready for you."),
        ],
      }),
    );
    equal([...planAnchors(items).entries()], [["e1", "D-2"]], "one card for D-2");
  }

  // The composer: disabled with a reason while read-only or the master
  // is not running.
  {
    const agents = (rows: Partial<AgentsPayload["agents"][number]>[]): AgentsPayload =>
      ({ daemon: "reachable", agents: rows, totals: null }) as unknown as AgentsPayload;
    equal(masterStatus(null, false).kind, "unknown", "not loaded");
    equal(masterStatus(agents([]), false).kind, "absent", "no master");
    equal(masterStatus(agents([{ alias: "master", state: "idle" }]), true).kind, "absent", "404 wins");
    equal(masterStatus(agents([{ alias: "master", state: "stopped" }]), false).kind, "stopped", "stopped");
    equal(masterStatus(agents([{ alias: "master", state: "idle" }]), false), { kind: "running", state: "idle" }, "running");
    equal(
      masterStatus({ daemon: "unreachable", agents: [], totals: null, by_issue: {}, refresh_error: false, snapshot_age_secs: 0, stale: false } as AgentsPayload, false).kind,
      "offline",
      "daemon down",
    );
    equal(composerBlock(null, { kind: "running", state: "idle" }), null, "may send");
    const ro = composerBlock("The board is read-only.", { kind: "running", state: "idle" }) ?? "";
    equal(ro.includes("read-only"), true, "the board's reason comes first");
    const absent = composerBlock(null, { kind: "absent" }) ?? "";
    equal(absent.includes("cadence master start"), true, "what to run");
  }
  console.log("thread checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});

// CAD-574 — an Ask-master send's refs ride the pending entry so a
// retry with the same message id sends the same refs (the daemon's
// idempotency compares content — retrying without them would 409).
{
  let s = mergePage(null, { thread: { id: "t1" }, entries: [] });
  const refs = [{ kind: "pr", id: "acme/app#187" }];
  s = addPending(s, "ui-9", "PR #187 has had no verdict — what should we do?", 10, refs);
  equal(s.pending[0].refs, refs, "refs kept on the pending entry");
  s = settlePending(s, "ui-9", { ok: false, error: "offline" });
  s = addPending(s, "ui-9", "PR #187 has had no verdict — what should we do?", 20, refs);
  equal([s.pending.length, s.pending[0].refs?.length], [1, 1], "retry keeps refs");
  s = discardPending(s, "ui-9");
}

// The worker-transcript render grouping: contiguous agent items paint as one
// turn. `threadItems` stays the flat contract (the canvas feed, the working
// state, plan anchors and the window all read it), so the grouping is pure.
{
  const s = mergePage(null, {
    thread: { id: "t1" },
    entries: [
      entry(1, "operator", "message", "show the board", { message: "m1" }),
      entry(2, "agent", "thinking", "Reading.", { message: "m1" }),
      entry(3, "agent", "assistant_text", "Board read.", { message: "m1" }),
      entry(4, "agent", "tool_call", "Bash: cadence board read", { message: "m1" }),
      entry(5, "agent", "artifact", "", {
        message: "m1",
        payload: { id: "art-a", version: 1, kind: "csv", title: "Board", bytes: 10 },
      }),
      entry(6, "agent", "tool_result", "2 issues", { message: "m1" }),
      entry(7, "agent", "turn_result", "Two issues.", { message: "m1" }),
      entry(8, "agent", "artifact", "", {
        message: "m1",
        payload: { id: "art-b", version: 1, kind: "md", title: "Notes", bytes: 10 },
      }),
      entry(9, "agent", "thinking", "Next turn.", { message: "m2" }),
      entry(10, "agent", "turn_result", "Second answer.", { message: "m2" }),
      entry(11, "system", "message", "note"),
    ],
  });
  const items = threadItems(s);
  equal(
    items.map((i) => i.type),
    ["operator", "thinking", "commentary", "tools", "artifact", "answer", "artifact", "thinking", "answer", "system"],
    "grouping does not change the flat items",
  );
  const runs = threadRuns(items);
  equal(
    runs.map((r) => [r.turn, r.key]),
    [[false, "e1"], [true, "e2"], [true, "e9"], [false, "e11"]],
    "one run per turn; every other item is its own row",
  );
  const first = runs[1];
  equal(
    first.turn ? first.items.map((i) => i.key) : null,
    ["e2", "e3", "e4", "e5", "e7", "e8"],
    "the turn keeps its reasoning, tool run, answer and cards",
  );
  const second = runs[2];
  equal(second.turn ? second.items.map((i) => i.type) : null, ["thinking", "answer"], "a turn after an answer starts fresh");
  equal(
    runs.flatMap((r) => (r.turn ? r.items : [r.item])).map((i) => i.key),
    items.map((i) => i.key),
    "grouping loses and reorders nothing",
  );
  equal(threadRuns([]), [], "no items, no runs");
  // A card on its own is not a turn: it keeps the plain card row.
  const cards = threadItems(
    mergePage(null, {
      thread: { id: "t2" },
      entries: [
        entry(1, "agent", "artifact", "", { payload: { id: "art-x", version: 1, kind: "md", title: "X", bytes: 1 } }),
      ],
    }),
  );
  equal(threadRuns(cards).map((r) => r.turn), [false], "an artifact-only run is not a turn");
  // A hire proposal stays in its render run so Home can keep the existing
  // flat card row instead of inventing a trace-only turn.
  const hired = threadItems(
    mergePage(null, {
      thread: { id: "t3" },
      entries: [
        entry(1, "operator", "message", "hire someone", { message: "m3" }),
        entry(2, "agent", "thinking", "Consider the team.", { message: "m3" }),
        entry(3, "agent", "hire_proposal", "Hire Ana", { message: "m3" }),
      ],
    }),
  );
  const hiredRuns = threadRuns(hired);
  equal(hiredRuns.map((r) => r.turn), [false, true], "hire: one render run with the proposal");
  equal(
    hiredRuns[1].turn ? hiredRuns[1].items.map((i) => i.type) : null,
    ["thinking", "hire"],
    "hire: the proposal stays in the turn",
  );

  // The operator turn id an item belongs to.
  equal(
    [itemMessage(items[0]), itemMessage(items[3]), itemMessage(items[5])],
    ["m1", "m1", "m1"],
    "items name their operator turn",
  );
  const pending = threadItems(addPending(s, "ui-p", "hi", 1));
  equal(itemMessage(pending[pending.length - 1]), "ui-p", "a pending row names its message");
  equal(rowPaintKey(pending[pending.length - 1]), "user:ui-p", "a pending row shares its paint key with the stored send");
  const storedUser = threadItems(
    mergePage(null, { thread: { id: "t1" }, entries: [entry(1, "operator", "message", "hi", { message: "ui-p" })] }),
  )[0];
  equal(rowPaintKey(storedUser), "user:ui-p", "the stored operator row keeps that paint key");
}

{
  const open = threadItems(
    mergePage(null, {
      thread: { id: "t-flight" },
      entries: [
        entry(1, "operator", "message", "what is outstanding?", { message: "ui-1" }),
        entry(2, "agent", "tool_call", "board_pending_actions"),
        entry(3, "agent", "tool_call", "board_list_issues {\"limit\":50}"),
      ],
    }),
  );
  equal(inFlightTurnKey(open, "ui-1"), "e2", "an untagged tool run after the send is that turn");
  equal(inFlightTurnKey(open, "server-id"), "e2", "a working id that matches no row still adopts the untagged run");
  equal(inFlightTurnKey(open, null), null, "a submit with no id does not claim an older run");
  const answered = threadItems(
    mergePage(null, {
      thread: { id: "t-flight" },
      entries: [
        entry(1, "operator", "message", "what is outstanding?", { message: "ui-1" }),
        entry(2, "agent", "tool_call", "board_pending_actions"),
        entry(3, "agent", "turn_result", "Nothing is waiting."),
      ],
    }),
  );
  equal(inFlightTurnKey(answered, "ui-1"), null, "a landed answer is not still in flight");
  const tagged = threadItems(
    mergePage(null, {
      thread: { id: "t-flight" },
      entries: [
        entry(1, "operator", "message", "older", { message: "m-old" }),
        entry(2, "agent", "tool_call", "board_read", { message: "m-old" }),
      ],
    }),
  );
  equal(inFlightTurnKey(tagged, "m-other"), null, "a run tagged for another message is not adopted");
  const waiting = threadItems(
    mergePage(null, {
      thread: { id: "t-flight" },
      entries: [entry(1, "operator", "message", "hello", { message: "ui-1" })],
    }),
  );
  equal(inFlightTurnKey(waiting, "ui-1"), null, "a send with no assistant rows yet has no in-flight run");
  // A row inside the candidate run that names another send belongs to a
  // different turn: it is never adopted, even under a matching user row, and
  // an untagged opener does not launder it.
  const interleaved = threadItems(
    mergePage(null, {
      thread: { id: "t-flight" },
      entries: [
        entry(1, "operator", "message", "mine", { message: "m-a" }),
        entry(2, "agent", "thinking", "hers", { message: "m-b" }),
      ],
    }),
  );
  equal(inFlightTurnKey(interleaved, "m-a"), null, "a row tagged to another send is not adopted under a matching user row");
  const mixed = threadItems(
    mergePage(null, {
      thread: { id: "t-flight" },
      entries: [
        entry(1, "operator", "message", "mine", { message: "m-a" }),
        entry(2, "agent", "thinking", "untagged"),
        entry(3, "agent", "tool_call", "board_read", { message: "m-b" }),
      ],
    }),
  );
  equal(inFlightTurnKey(mixed, "m-a"), null, "an untagged opener followed by another send's row is not adopted");
  const sameId = threadItems(
    mergePage(null, {
      thread: { id: "t-flight" },
      entries: [
        entry(1, "operator", "message", "mine", { message: "m-a" }),
        entry(2, "agent", "thinking", "untagged"),
        entry(3, "agent", "tool_call", "board_read", { message: "m-a" }),
      ],
    }),
  );
  equal(inFlightTurnKey(sameId, "m-a"), "e2", "an untagged opener joins rows tagged with the same send");
  // A system row between the send and its first assistant rows is
  // transparent; a queued user row after it does not hide the run.
  const gap = threadItems(
    mergePage(null, {
      thread: { id: "t-flight" },
      entries: [
        entry(1, "operator", "message", "mine", { message: "m-a" }),
        entry(2, "system", "message", "note"),
        entry(3, "operator", "message", "queued", { message: "m-b" }),
        entry(4, "agent", "thinking", "working"),
      ],
    }),
  );
  equal(inFlightTurnKey(gap, "m-a"), "e4", "a system gap lets the tail run be adopted");
  equal(sendBefore(gap, [gap[3]], new Set(["m-b"])), "m-a", "a queued user row is skipped when naming the run's send");
}
