/**
 * CAD-1811 review regression: only the exact decision opening may close or
 * advance the shared detail. The result is recorded either way.
 */
import type { HomeNeed } from "../src/features/home/needs";
import { applyDecisionDone, type HomeDecisionOpen } from "../src/lib/workItems";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

function need(id: string, action: HomeNeed["action"] = { type: "plan", epic: id }): HomeNeed {
  return {
    key: `issue:${id}`, kind: "plan", label: "Plan", title: `${id} plan`, shortTitle: null, why: null,
    owner: "operator", age: 10, project: "demo", subject: { kind: "issue", id }, summary: null, escalatedBy: null,
    command: "cadence x", link: null, issue: id, agent: null, action,
  };
}

function opening(token: number, reviewHead: string | null, next: HomeDecisionOpen | null, recorded: string[]): HomeDecisionOpen {
  return {
    need: need("D-1"), index: 1, total: 1, token, reviewHead,
    next: (text) => { recorded.push(text); return next; },
    ask: () => {},
  };
}

const selection = (decision: HomeDecisionOpen | null, issueId: string | null = "D-1", origin = "home") =>
  ({ decision, issueId, origin });

{
  const recorded: string[] = [];
  const done = opening(1, null, null, recorded);
  const outcome = applyDecisionDone(done, "plan approved", selection(done));
  assert(outcome.kind === "closed", "the exact opening may close the detail");
  assert(recorded.length === 1 && recorded[0] === "plan approved", "the result is recorded");
}
{
  const recorded: string[] = [];
  const done = opening(1, null, null, recorded);
  const current = opening(2, null, null, []);
  const outcome = applyDecisionDone(done, "plan approved", selection(current));
  assert(outcome.kind === "ignored", "a different opening token is ignored");
  assert(recorded.length === 1, "the ignored result is still recorded");
}
{
  const recorded: string[] = [];
  const done = opening(1, null, null, recorded);
  const outcome = applyDecisionDone(done, "x", selection(done, "D-9"));
  assert(outcome.kind === "ignored", "a different selected issue is ignored");
}
{
  const recorded: string[] = [];
  const done = opening(1, null, null, recorded);
  const outcome = applyDecisionDone(done, "x", selection(done, "D-1", "myWork"));
  assert(outcome.kind === "ignored", "the same token on another origin is ignored");
}
{
  const recorded: string[] = [];
  const done = opening(1, "c".repeat(40), null, recorded);
  const sameTokenNewHead = { ...done, reviewHead: "d".repeat(40) };
  const outcome = applyDecisionDone(done, "x", selection(sameTokenNewHead));
  assert(outcome.kind === "ignored", "a new reviewed head on the same opening is ignored");
}
{
  const recorded: string[] = [];
  const next = opening(2, null, null, []);
  const done = opening(1, null, next, recorded);
  const outcome = applyDecisionDone(done, "x", selection(done));
  assert(outcome.kind === "advanced" && outcome.next === next, "an exact opening with a next item advances");
}
{
  const recorded: string[] = [];
  const done = opening(1, null, null, recorded);
  const outcome = applyDecisionDone(done, "x", selection(null));
  assert(outcome.kind === "ignored", "no hosted decision is ignored");
}

console.log("work-item decision-fence checks passed");
