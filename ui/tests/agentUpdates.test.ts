import { agentThreadHref, detailTasksNotOnRoster, partitionActiveTasks } from "../src/features/agents/AgentBlocks";
import { activeAssignedGroups, agentWorkHref, recentAgentUpdates, recentWorkingAgents, reportedAgentUpdates, updatesNotices } from "../src/features/home/agentUpdateModel";
import { activityTimeMs } from "../src/lib/fmt";
import type { Agent, AgentDetail, AgentTask } from "../src/lib/types";
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
const agents = Array.from({ length: 20 }, (_, i) => ({ provider: "test", endpoint_kind: "test", state: "idle", group: "test", group_root: true, tasks: [], unknown: 0, parked: 0, fenced: false, alias: `a${i}`, on: [], running: 0, queued: 0, last_activity: new Date(1000 * i).toISOString() } as Agent));
const selected = recentWorkingAgents([...agents, { ...agents[0], alias: "master" }]);
check(selected.length === 12 && selected[0].alias === "a19", "bounded reads select recently active agents");
check(!selected.some((a) => a.alias === "master"), "Master has its own conversation");
check(activityTimeMs(1790562108.64) === 1790562108640, "daemon epoch seconds resolve to milliseconds");
check(activityTimeMs("2026-09-28T02:21:48.640Z") === 1790562108640, "ISO activity remains supported");
check(activityTimeMs("not-a-time") === 0, "invalid activity stays unavailable");
check(activityTimeMs("2026-02-31T04:00:00Z") === 0, "nonexistent calendar dates stay unavailable");
check(recentWorkingAgents([
  { ...agents[0], alias: "recent", last_activity: 1790562108.64 },
  { ...agents[0], alias: "older", last_activity: "2026-09-27T00:00:00Z" },
])[0].alias === "recent", "numeric activity sorts newer workers first");
console.log("agentUpdates: passed");

const reports = reportedAgentUpdates([
  { seq: 9, role: "system", kind: "message", text: "[report] CAD-607 done by cur-607 at 2026-09-26T17:57:25Z\n\n## Expected\nThe layout stays consistent.\n\n## Evidence\nchecks pass" },
  { seq: 10, role: "operator", kind: "message", text: "[report] CAD-608 done by worker at 2026-09-26T18:00:00Z" },
]);
check(reports.get("cur-607")?.[0].text === "CAD-607 — The layout stays consistent.", "delivered worker report supplies a readable summary");
check(reports.get("cur-607")?.[0].label === "Work reported", "a done report is not a verified task completion");
check(!reports.has("worker"), "operator text never impersonates a delivered report");

function task(id: string, state: string, title: string): AgentTask {
  return { task: id, task_state: state, title, issue: "", job: "job-1", job_state: "running" };
}
function worker(alias: string, tasks: AgentTask[]): Agent {
  return { ...agents[0], alias, tasks, running: tasks.some((item) => item.task_state === "running") ? 1 : 0 };
}
const grouped = activeAssignedGroups([
  worker("ada", [task("run-1", "running", "Draw the states"), task("done-1", "done", "Write the spec")]),
  worker("master", [task("run-2", "running", "Hidden")]),
]);
check(grouped.length === 1 && grouped[0].alias === "ada", "active work is grouped by the assigned agent");
check(grouped[0].tasks.length === 1 && grouped[0].tasks[0].task === "run-1", "a done task leaves the active group");
check(agentWorkHref("ada") === "/agents/ada?tab=work", "the agent opens the existing Work tab");
check(agentThreadHref("ada") === "/threads?dm=ada", "the task opens the existing conversation");
const failed = updatesNotices({ hasData: true, daemon: "unreachable", refreshError: false, recentCount: 0 });
check(failed.unavailable && !failed.empty, "a failed read is not an empty success");
const partial = updatesNotices({ hasData: true, daemon: "ok", refreshError: true, recentCount: 2 });
check(partial.partial && !partial.empty && !partial.unavailable, "a partial roster stays partial");
check(JSON.stringify(detailTasksNotOnRoster(null, ["a", "b"])) === JSON.stringify(["a", "b"]), "a missing roster keeps every detail task id");
check(JSON.stringify(detailTasksNotOnRoster(partitionActiveTasks(worker("ada", [task("run-1", "running", "Draw the states")])), ["run-1", "detail-only"])) === JSON.stringify(["detail-only"]), "a roster task is not listed twice");
check(JSON.stringify(detailTasksNotOnRoster(partitionActiveTasks(worker("ada", [])), ["detail-only"])) === JSON.stringify(["detail-only"]), "an empty roster row still shows detail task ids");
check(JSON.stringify(detailTasksNotOnRoster(partitionActiveTasks(worker("ada", [])), undefined)) === JSON.stringify([]), "missing detail ids stay empty");
