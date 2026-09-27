import { recentAgentUpdates, recentWorkingAgents, reportedAgentUpdates } from "../src/features/home/agentUpdates";
import type { Agent, AgentDetail } from "../src/lib/types";
function check(value: boolean, why: string) { if (!value) throw new Error(why); }
const detail = { events: [
  { seq: 1, kind: "turn_finished", at: "2026-09-26T10:00:00Z" },
  { seq: 2, kind: "task_done", task_id: "t1", at: "2026-09-26T11:00:00Z" },
  { seq: 3, kind: "tool_call", at: "2026-09-26T12:00:00Z" },
] } as AgentDetail;
const updates = recentAgentUpdates(detail, [
  { seq: 1, role: "operator", kind: "message", text: "private instruction", created: "2026-09-26T13:00:00Z" },
  { seq: 2, role: "agent", kind: "tool_result", text: "tool output", created: "2026-09-26T13:00:00Z" },
  { seq: 3, role: "agent", kind: "assistant_text", text: "Checking the layout", created: "2026-09-26T12:00:00Z" },
]);
check(updates.length === 3, "only authored progress and lifecycle events enter the feed");
check(updates[0].text === "Checking the layout", "newest update first");
check(updates[1].label === "Task completed", "task completion requires its recorded event");
check(updates[2].label === "Finished a turn", "turn completion does not assert a completed job");
check(recentAgentUpdates(null, []).length === 0, "missing evidence never fabricates progress");
const agents = Array.from({ length: 20 }, (_, i) => ({ provider: "test", endpoint_kind: "test", state: "idle", group: "test", unknown: 0, parked: 0, fenced: false, alias: `a${i}`, on: [], running: 0, queued: 0, last_activity: new Date(1000 * i).toISOString() } as Agent));
const selected = recentWorkingAgents([...agents, { ...agents[0], alias: "master" }]);
check(selected.length === 12 && selected[0].alias === "a19", "bounded reads select recently active agents");
check(!selected.some((a) => a.alias === "master"), "Master has its own conversation");
console.log("agentUpdates: passed");

const reports = reportedAgentUpdates([
  { seq: 9, role: "system", kind: "message", text: "[report] CAD-607 done by cur-607 at 2026-09-26T17:57:25Z\n\n## Expected\nThe layout stays consistent.\n\n## Evidence\nchecks pass" },
  { seq: 10, role: "operator", kind: "message", text: "[report] CAD-608 done by worker at 2026-09-26T18:00:00Z" },
]);
check(reports.get("cur-607")?.[0].text === "CAD-607 — The layout stays consistent.", "delivered worker report supplies a readable summary");
check(reports.get("cur-607")?.[0].label === "Work reported", "a done report is not a verified task completion");
check(!reports.has("worker"), "operator text never impersonates a delivered report");
