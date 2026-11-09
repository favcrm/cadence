import type { MasterState } from "../src/lib/types";
import { turnBarState } from "../src/features/app-shell/chat/turnBarState";
import type { ThreadEntry } from "../src/features/home/thread";

function equal(a: unknown, b: unknown, what: string) {
  if (JSON.stringify(a) !== JSON.stringify(b)) throw new Error(`${what}: ${JSON.stringify(a)}`);
}

const FAILURE =
  "The conversation could not be compacted. Start a fresh conversation or split the task and retry.";

const master = (extra: Partial<MasterState>): MasterState =>
  ({ alias: "master", provider: "cadence", endpoint_kind: "durable_object", live: true, ...extra }) as MasterState;

const bar = (state: MasterState | null, entries: ThreadEntry[] = []) =>
  turnBarState(
    { master: state, entries, pendingMessages: [], awaitingApproval: false, reconnecting: false },
    "current",
  );

// The runtime reports a failed blocking compaction on the master state, and it
// never reaches the transcript: the bar is the only place that can name it.
equal(
  bar(master({ compaction_failure: FAILURE })),
  { kind: "failed", reason: FAILURE, message: null },
  "an unanswered compaction failure is shown with no retry target",
);
equal(
  bar(master({ compaction_failure: "  " })),
  { kind: "idle" },
  "a blank sentence is not a failure",
);
equal(bar(master({})), { kind: "idle" }, "no failure field reads as idle");

// A running turn keeps its own state: the failure is about a settled run.
equal(
  bar(
    master({
      compaction_failure: FAILURE,
      turn: { state: "working", message: "m1", conversation: "current" },
    }),
  ),
  { kind: "working", step: null, since: null, stopMessage: "m1" },
  "a running turn outranks a stale failure",
);

// A stored failed turn keeps its message, so the bar can still offer Retry.
const stored: ThreadEntry[] = [
  { seq: 1, role: "operator", kind: "message", text: "hello", message: "m1" },
  {
    seq: 2,
    role: "agent",
    kind: "turn_result",
    text: "",
    created: new Date(2_000).toISOString(),
    payload: { status: "failed", error: "The model request failed." },
  },
];
equal(
  bar(master({ compaction_failure: FAILURE }), stored),
  { kind: "failed", reason: "The model request failed.", message: null },
  "a stored turn result keeps its own reason",
);
