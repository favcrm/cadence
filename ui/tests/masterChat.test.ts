/**
 * CAD-551 units: the slash catalog/parser, the turn-state derivation
 * (master_state live → agents row → composer submit), and the tool-step
 * pairing the steps disclosure renders.
 */
import {
  parseSlash,
  slashMatches,
  SLASH_COMMANDS,
  turnState,
} from "../src/features/home/master";
import {
  openStep,
  stepSummary,
  toolSteps,
  type ThreadEntry,
} from "../src/features/home/thread";
import type { Agent, MasterState } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function ok(cond: boolean, what: string): void {
  if (!cond) throw new Error(`failed: ${what}`);
}

function entry(
  seq: number,
  kind: string,
  text: string,
  payload: Record<string, unknown> | null = null,
): ThreadEntry {
  return { seq, role: "agent", kind, text, payload, message: "m1", created: null };
}

// ---- parseSlash / slashMatches ----

equal(parseSlash("/state"), { name: "state", arg: "" }, "bare verb");
equal(parseSlash("/model fake/model-2"), { name: "model", arg: "fake/model-2" }, "verb+arg");
equal(parseSlash("/effort  high "), { name: "effort", arg: "high" }, "arg trimmed");
equal(parseSlash("/MODEL x"), { name: "model", arg: "x" }, "verb lowercased");
equal(parseSlash("hello"), null, "plain text is not a command");
equal(parseSlash("/"), null, "slash alone is not a command");
equal(parseSlash("/123"), null, "digits are not a command name");
equal(parseSlash(" /state"), null, "leading space disables the verb");

ok(slashMatches("s").map((c) => c.name).join(",").includes("stop"), "prefix filter");
equal(slashMatches("zzz"), [], "no match");
equal(slashMatches(""), SLASH_COMMANDS, "empty prefix lists all");

// help stays composer-local: the board allowlist must never carry it.
ok(SLASH_COMMANDS.some((c) => c.name === "help"), "help exists");
ok(
  SLASH_COMMANDS.every((c) => c.name !== "help" || c.kind === "read"),
  "help is read-kind (composer-local)",
);

// ---- turnState ----

const masterRow: Agent = {
  alias: "master",
  provider: "pi",
  endpoint_kind: "worker",
  state: "running",
  group: "g",
  group_root: false,
  running: 1,
  queued: 2,
  unknown: 0,
  parked: 0,
  fenced: false,
  on: [],
  message: { id: "m-run", summary: "row says", created: "2026-09-24T10:00:00Z" },
};

const liveWorking: MasterState = {
  alias: "master",
  provider: "pi",
  endpoint_kind: "worker",
  live: true,
  commands: ["stop"],
  turn: { state: "working", message: "m-live", summary: "live says", since: 1700 },
  queued: 3,
};

// Live state wins over the agents row.
let t = turnState(liveWorking, masterRow, true);
ok(t.kind === "working" && t.message === "m-live" && t.since === 1700, "live working wins");
ok(t.kind === "working" && t.queued === 3, "live queued count");

const liveQueued: MasterState = { ...liveWorking, turn: { state: "queued", message: "m-q" } };
t = turnState(liveQueued, masterRow, false);
equal(t.kind, "queued", "queued turn");

// No live payload: the agents row's counters carry the state.
t = turnState(undefined, masterRow, false);
ok(t.kind === "working" && t.message === "m-run", "agents-row working fallback");
ok(
  t.kind === "working" &&
    typeof t.since === "number" &&
    t.since === Date.parse("2026-09-24T10:00:00Z") / 1000,
  "row created → epoch seconds",
);

t = turnState(undefined, { ...masterRow, running: 0, queued: 1, message: null }, false);
equal(t.kind, "queued", "agents-row queued fallback");

t = turnState(undefined, { ...masterRow, running: 0, queued: 0, message: null }, true);
equal(t.kind, "submitting", "composer send in flight");

t = turnState(undefined, { ...masterRow, running: 0, queued: 0, message: null }, false);
equal(t.kind, "idle", "nothing in flight");

// A compacting session surfaces on the working state.
t = turnState({ ...liveWorking, session: { compacting: true } }, masterRow, false);
ok(t.kind === "working" && t.compacting === true, "compacting flag");

// ---- stepSummary ----

equal(stepSummary("Read /a/b.ts"), "Read /a/b.ts", "one-liner passes");
equal(stepSummary("Read /a\nextra detail"), "Read /a", "newline cut to first line");
equal(stepSummary(null), "", "null text");
ok(stepSummary("x".repeat(200)).length === 118, "long line trimmed to 117 + ellipsis");

// ---- toolSteps ----

const calls = [
  entry(1, "tool_call", "Read /a", { tool: "read", tool_use_id: "u1" }),
  entry(2, "tool_result", "ok", { tool_use_id: "u1", is_error: false }),
  entry(3, "tool_call", "Bash rm", { tool: "bash", tool_use_id: "u2" }),
  entry(4, "tool_result", "denied", { tool_use_id: "u2", is_error: true, refused: true }),
];

let steps = toolSteps(calls);
equal(steps.length, 2, "two steps paired by id");
equal(steps[0].done, true, "first answered");
equal(steps[0].refused, false, "first not refused");
equal(steps[1].done && steps[1].refused && steps[1].error, true, "second refused");
equal(steps[1].result?.seq, 4, "refusal paired with its call");

// Positional pairing when ids are absent.
steps = toolSteps([
  entry(1, "tool_call", "A"),
  entry(2, "tool_call", "B"),
  entry(3, "tool_result", "a", { is_error: false }),
]);
equal(steps.length, 2, "two positional calls");
ok(steps[0].done && !steps[1].done, "result answers the earliest open call");

// A result naming no known call stands alone at the end.
steps = toolSteps([
  entry(1, "tool_call", "A", { tool_use_id: "u1" }),
  entry(2, "tool_result", "orphan", { tool_use_id: "zzz", is_error: true }),
]);
equal(steps.length, 2, "orphan result kept");
equal(steps[1].call.kind, "tool_result", "orphan carries itself");

// ---- openStep ----

equal(
  openStep([entry(1, "tool_call", "A", { tool_use_id: "u1" })])?.text,
  "A",
  "trailing open call",
);
equal(
  openStep([
    entry(1, "tool_call", "A", { tool_use_id: "u1" }),
    entry(2, "tool_result", "ok", { tool_use_id: "u1" }),
  ]),
  null,
  "answered call is no step",
);
equal(
  openStep([
    entry(1, "tool_call", "A", { tool_use_id: "u1" }),
    entry(2, "assistant_text", "thinking…"),
  ]),
  null,
  "text ends the tool run",
);
// Parallel calls: the second is still open after the first's result.
equal(
  openStep([
    entry(1, "tool_call", "A", { tool_use_id: "u1" }),
    entry(2, "tool_call", "B", { tool_use_id: "u2" }),
    entry(3, "tool_result", "ok", { tool_use_id: "u1" }),
  ])?.text,
  "B",
  "the still-open parallel call",
);
equal(openStep([entry(1, "assistant_text", "hi")]), null, "no tools at all");

console.log("masterChat.test.ts: all assertions passed");
