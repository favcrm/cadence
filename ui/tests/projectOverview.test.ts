import { projectSummary } from "../src/features/projects/overview";
import type { IssueCard } from "../src/lib/types";
const card = (id: string, status: string, extra: Partial<IssueCard> = {}): IssueCard => ({ id, project: "a", title: id, status, status_source: "file", priority: "P2", container: false, blocked: false, ready: false, blocked_by: [], relates: [], refs: [], created: "2026-01-01", rev: "r", counts: { comments: 0, artifacts: 0, refs: 0 }, paths: [], tags: [], work: { type: "task", type_source: "implicit", weight: 1 }, checks: { done: 0, total: 0 }, ...extra });
const summary = projectSummary([card("A-1", "doing"), card("A-2", "review"), card("A-3", "backlog", { blocked: true }), card("A-4", "done"), card("A-5", "doing", { container: true }), card("B-1", "doing", { project: "b" })], "a");
if (summary.open !== 3 || summary.doing !== 1 || summary.review !== 1 || summary.blocked !== 1 || summary.done !== 1 || summary.epics.length !== 1) throw new Error("project counts isolate scope, exclude epics and closed work");
if (summary.focus.map((item) => item.id).join() !== "A-2,A-3,A-1") throw new Error("reviews and blockers come before routine work");
if (projectSummary([], "empty").open !== 0) throw new Error("empty projects remain empty");
const reported = card("A-6", "backlog", {
  local_assignment: {
    version: "hosted-cadence-local-assignment-read.v1",
    source: "local_assignment_coordinator",
    scope: "participant",
    available: true,
    assignments: [{ assignment_id: "assignment", state: "result" }],
  },
});
const withResult = projectSummary([card("A-1", "doing"), reported, card("A-2", "review")], "a");
if (withResult.focus.map((item) => item.id).join() !== "A-2,A-6,A-1") throw new Error("a visible reported result is surfaced without changing tracker status");
if (reported.status !== "backlog") throw new Error("a report must not accept or close the tracker issue");
console.log("project overview checks passed");
