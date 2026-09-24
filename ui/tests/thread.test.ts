import { Resource } from "../src/lib/cache";
import { streamInto, type EventSourceLike } from "../src/lib/sse";
import {
  addPending,
  discardPending,
  EMPTY_THREAD,
  lastSeq,
  loadThread,
  mergePage,
  newMessageId,
  planAnchors,
  planRef,
  reduceFrame,
  settlePending,
  threadItems,
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

  // Loading pages: after what is held, until a short page; 404 is the
  // "master not started" state, not a failure; other errors throw.
  {
    const calls: number[] = [];
    const pages: Record<number, unknown[]> = {
      0: [entry(1, "operator", "message", "a"), entry(2, "agent", "turn_result", "b")],
      2: [entry(3, "agent", "turn_result", "c")],
    };
    const s = await loadThread(async (after) => {
      calls.push(after);
      return { thread: { id: "t1" }, entries: pages[after] ?? [] };
    }, null, 2);
    equal(calls, [0, 2], "pages until a short one");
    equal(s.entries.map((e) => e.seq), [1, 2, 3], "all pages merged");
    const held = addPending(s, "p", "keep me", 1);
    const again = await loadThread(async (after) => {
      calls.push(after);
      return { thread: { id: "t1" }, entries: [] };
    }, held);
    equal(calls[calls.length - 1], 3, "refetch starts after the held cursor");
    equal(again.pending.length, 1, "pending survives a refetch");
    const gone = await loadThread(() => Promise.reject(Object.assign(new Error("no agent"), { status: 404 })), null);
    equal([gone.missing, gone.exists], [true, false], "404 → missing");
    let threw = false;
    await loadThread(() => Promise.reject(Object.assign(new Error("down"), { status: 503 })), null).catch(() => {
      threw = true;
    });
    equal(threw, true, "a 503 is a failure");
  }

  // Plan cards: each epic once, at its first mention.
  {
    equal(planRef(entry(1, "agent", "tool_result", '{"epic": "D-2", "tickets": ["D-3"]}')), "D-2", "json epic");
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
          entry(2, "agent", "tool_result", '{"epic":"D-2"}'),
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
      masterStatus({ daemon: "unreachable", agents: [], totals: null } as AgentsPayload, false).kind,
      "offline",
      "daemon down",
    );
    equal(composerBlock(false, { kind: "running", state: "idle" }), null, "may send");
    const ro = composerBlock(true, { kind: "running", state: "idle" }) ?? "";
    equal(ro.includes("read-only"), true, "read-only reason");
    const absent = composerBlock(false, { kind: "absent" }) ?? "";
    equal(absent.includes("cadence master start"), true, "what to run");
  }
  console.log("thread checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
