import { agentIsUnassigned, agentIssueIds, agentMatchesProject, issueIndex } from "./scope";
import type { Agent, IssueCard } from "./types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const card = (id: string, project: string, status: string, owner?: string) =>
  ({ id, project, status, owner }) as IssueCard;
const agent = (alias: string, on: string[] = []) =>
  ({ alias, on, tasks: on.map((issue) => ({ task: `t-${issue}`, issue })) }) as unknown as Agent;

const issues = [
  card("CAD-258", "cadence", "doing", "opus-n"),
  card("CAD-259", "cadence", "review", "opus-n"),
  card("CAD-100", "cadence", "done", "old-dev"),
  card("CAD-101", "cadence", "ready", "planner"),
  card("KID-9", "kidult", "doing"),
];
const index = issueIndex(issues);

// Ownership: an owner of a doing/review issue is bound with no job at all —
// the CLI ISSUES rule (status_view in src/main.rs). This is the audit case
// of busy cadence workers showing 0 in the project's Agents view.
const worker = agent("opus-n");
equal(agentIssueIds(worker, index), ["CAD-258", "CAD-259"], "owned doing/review issues");
equal(agentMatchesProject(worker, "cadence", index), true, "owner bound to project");
equal(agentMatchesProject(worker, "kidult", index), false, "not another project");

// Owners of done/ready issues are not bound — same as the CLI column.
equal(agentIsUnassigned(agent("old-dev"), index), true, "done owner unbound");
equal(agentIsUnassigned(agent("planner"), index), true, "ready owner unbound");

// Dispatch: a job task on an issue binds without ownership.
const dispatched = agent("codex-a", ["KID-9"]);
equal(agentMatchesProject(dispatched, "kidult", index), true, "job-bound agent");
equal(agentIssueIds(dispatched, index), ["KID-9"], "job binding ids");

// Both sources de-duplicate.
equal(agentIssueIds(agent("opus-n", ["CAD-258"]), index), ["CAD-258", "CAD-259"], "dedupe");

// All projects includes everyone.
equal(agentMatchesProject(agent("nobody"), "all", index), true, "all projects");

console.log("scope checks passed");
