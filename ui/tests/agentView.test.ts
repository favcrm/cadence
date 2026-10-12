import {
  agentCategory,
  agentHoldsWorkButDead,
  agentMatchesFilter,
  agentMatchesSearch,
  agentStatus,
  lifecycleOf,
} from "../src/features/agents/agentView";
import { issueIndex } from "../src/lib/scope";
import type { Agent, IssueCard } from "../src/lib/types";

const base: Agent = {
  alias: "dev-api",
  provider: "pi",
  endpoint_kind: "pty",
  state: "idle",
  group: "team-pm",
  running: 0,
  queued: 0,
  unknown: 0,
  parked: 0,
  fenced: false,
  on: [],
};
function equal(actual: unknown, expected: unknown, what: string) {
  if (actual !== expected)
    throw new Error(`${what}: expected ${expected}, got ${actual}`);
}
const category = (fields: Partial<Agent>) =>
  agentCategory({ ...base, ...fields });
equal(category({}), "idle", "idle worker");
equal(
  category({ state: "stopped" }),
  "stopped",
  "stopped worker remains discoverable",
);
equal(
  category({ state: "starting" }),
  "working",
  "starting is active even without a message",
);
equal(category({ running: 1 }), "working", "running evidence");
equal(category({ queued: 2 }), "working", "queued worker");
equal(
  category({ provider: "inbox", inbox: true, queued: 840 }),
  "inbox",
  "mailbox queue is not working evidence",
);
equal(
  category({ fenced: true, inbox: true, queued: 1 }),
  "attention",
  "fence takes priority over mailbox",
);
for (const fields of [
  { unknown: 1 },
  { stalled: true, running: 1 },
  { state: "waiting_input", running: 1 },
  { state: "blocked_by_quota" },
  { quota: { state: "blocked" } },
  { usage_limit: { state: "blocked" } },
]) {
  equal(
    category(fields),
    "attention",
    "uncertain, waiting and blocked work stays visible",
  );
}
equal(
  category({ state: "provider_custom" }),
  "other",
  "unrecognized state is not fabricated",
);
equal(
  agentStatus({ ...base, state: "waiting_input" }),
  "needs input",
  "input is not automatically approval",
);
equal(
  agentStatus({ ...base, fenced: true, running: 1 }),
  "attention",
  "fence overrides busy label",
);
equal(
  agentStatus({ ...base, provider: "inbox", queued: 3 }),
  "inbox",
  "mailbox label",
);
equal(
  agentStatus({ ...base, quota: { state: "blocked" }, running: 1 }),
  "quota blocked",
  "blocked overrides running",
);

// CAD-1320: a dead endpoint still bound to work is the board's critical
// signal — not live, but not releasable either.
const holding = (fields: Partial<Agent>) =>
  agentHoldsWorkButDead({ ...base, ...fields });
equal(holding({}), false, "idle agent holds nothing");
equal(
  holding({ dead: true, queued: 2 }),
  true,
  "dead endpoint with undelivered mail",
);
equal(
  holding({
    state: "stopped",
    tasks: [{ task: "t", issue: "DEMO-1", task_state: "running", job: "j" }],
  }),
  true,
  "stopped agent still owns a task",
);
equal(holding({ fenced: true, on: ["DEMO-1"] }), true, "fence keeps the claim");
equal(holding({ state: "stopped" }), false, "stopped with nothing held");
equal(
  holding({ dead: true }),
  false,
  "dead with no work is just dead",
);
equal(holding({ queued: 2 }), false, "a live agent's queue is its own");

const lifecycle = (fields: Partial<Agent>) =>
  lifecycleOf({ ...base, ...fields });
equal(
  lifecycle({ dead: true, queued: 1 }),
  "dead-holding",
  "dead with work ranks first",
);
equal(
  lifecycle({ state: "idle", on: ["DEMO-1"] }),
  "idle-holding",
  "live agent parked on a claim",
);
equal(
  lifecycle({ provider: "inbox", inbox: true, queued: 40 }),
  "stale-inbox",
  "mailbox with unread mail",
);
equal(
  lifecycle({ provider: "inbox", inbox: true }),
  "normal",
  "empty mailbox is ordinary",
);
equal(lifecycle({ running: 1 }), "active", "running evidence is working");
equal(lifecycle({}), "normal", "plain idle is ordinary");
equal(
  agentMatchesFilter({ ...base, dead: true, queued: 1 }, "current"),
  true,
  "dead-holding stays on the current board",
);
equal(
  agentMatchesFilter({ ...base, dead: true, queued: 1 }, "holding"),
  true,
  "holding filter catches dead work",
);
equal(
  agentMatchesFilter({ ...base, state: "idle", on: ["DEMO-1"] }, "holding"),
  true,
  "holding filter catches a parked claim",
);
equal(
  agentMatchesFilter({ ...base }, "holding"),
  false,
  "holding filter passes plain idle",
);

const index = issueIndex([
  {
    id: "DEMO-12",
    project: "demo",
    status: "doing",
    owner: base.alias,
  } as IssueCard,
]);
equal(
  agentMatchesSearch(base, "  DEV-api DEMO-12 ", index),
  true,
  "case insensitive multi-term ownership search",
);
equal(
  agentMatchesSearch(base, "SITE-99", index),
  false,
  "foreign issue is not fabricated",
);
equal(
  agentMatchesSearch({ ...base, on: ["SITE-99"] }, "site-99", index),
  true,
  "exact dispatch binding",
);
equal(
  agentMatchesSearch(
    {
      ...base,
      tasks: [
        {
          task: "t1",
          issue: "DEMO-1",
          title: "Account settings",
          task_state: "running",
          job: "j1",
        },
      ],
    },
    "account settings",
    index,
  ),
  true,
  "task title search",
);
equal(
  agentMatchesSearch(
    {
      ...base,
      running_messages: [{ id: "m1", summary: "Reviewing navigation" }],
    },
    "navigation",
    index,
  ),
  true,
  "ad hoc work search",
);
console.log("agent view checks passed");
