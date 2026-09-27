import {
  agentCategory,
  agentMatchesSearch,
  agentStatus,
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
