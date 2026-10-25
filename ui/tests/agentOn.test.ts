import type { Agent, AgentsPayload } from "../src/lib/types";
import { agentOn, normalizeAgent, normalizeAgentsPayload } from "../src/lib/agentOn";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

const agent = (row: Partial<Agent>) => row as Agent;

equal(agentOn(agent({ on: ["ACME-1", "ACME-2"] })), ["ACME-1", "ACME-2"], "a bound row passes through");
// The hosted tenant sent no `on`; iterating it blanked the board (CAD-1833).
equal(agentOn(agent({ on: undefined })), [], "a row without on has no bound issues");
equal(agentOn(agent({ on: null as never })), [], "a null on is not a list");
equal(agentOn(agent({ on: "ACME-1" as never })), [], "a non-array on is not a list");

// The exact row the hosted tenant served before CAD-1833: no `on`, no counters.
const minimal = {
  alias: "master",
  provider: "pi",
  endpoint_kind: "durable_object",
  state: "running",
  state_label: "running",
  group: "master",
  group_root: true,
  role: "master",
} as Agent;
const row = normalizeAgent(minimal);
equal(row.on, [], "normalized row has an empty binding list");
equal([row.running, row.queued, row.unknown, row.parked], [0, 0, 0, 0], "normalized counters are zero, not NaN");
equal(row.fenced, false, "normalized fenced is false");
equal(row.inbox, null, "normalized inbox is null");
const kept = normalizeAgent({ ...minimal, running: 2, on: ["X-1"] });
equal(kept.running, 2, "a present counter is kept");
equal(kept.on, ["X-1"], "a present binding list is kept");

const payload = normalizeAgentsPayload({
  daemon: "reachable",
  agents: [minimal],
  totals: { running: 1, queued: 0, fenced: 0, parked: 0, inboxes: 0 },
  bindings: {},
} as unknown as AgentsPayload);
equal(payload.agents.length, 1, "payload keeps its rows");
equal(payload.agents[0].on, [], "payload rows are normalized");

console.log("agent-on checks passed");
