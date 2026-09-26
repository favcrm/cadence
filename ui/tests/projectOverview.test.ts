import { projectSummary } from "../src/features/projects/overview";
import type { IssueCard } from "../src/lib/types";
const card = (id: string, status: string, extra: Partial<IssueCard> = {}): IssueCard => ({ id, project: "a", title: id, status, status_source: "file", priority: "P2", container: false, blocked: false, ready: false, blocked_by: [], relates: [], refs: [], created: "2026-01-01", rev: "r", counts: { comments: 0, artifacts: 0, refs: 0 }, checks: { done: 0, total: 0 }, ...extra });
const summary = projectSummary([card("A-1", "doing"), card("A-2", "review"), card("A-3", "backlog", { blocked: true }), card("A-4", "done"), card("A-5", "doing", { container: true }), card("B-1", "doing", { project: "b" })], "a");
if (summary.open !== 3 || summary.doing !== 1 || summary.review !== 1 || summary.blocked !== 1 || summary.done !== 1 || summary.epics.length !== 1) throw new Error("project counts isolate scope, exclude epics and closed work");
if (summary.focus.map((item) => item.id).join() !== "A-2,A-3,A-1") throw new Error("reviews and blockers come before routine work");
if (projectSummary([], "empty").open !== 0) throw new Error("empty projects remain empty");
console.log("project overview checks passed");
