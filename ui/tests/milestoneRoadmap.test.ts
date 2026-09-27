import { checkpointDate, checkpointLabel, checkpointWork, currentCheckpoints, scheduleLabel } from "../src/features/projects/roadmap";
import type { MilestoneRow } from "../src/lib/types";
const row = (id: string, extra: Partial<MilestoneRow> = {}): MilestoneRow => ({ project: "demo", id, title: null, exit: null, configured: true, progress: { done_weight: 3, total_weight: 3, ratio: 1, counts: { total: 1, done: 1, dropped: 0, open: 0, doing: 0, review: 0, blocked: 0 } }, health: { state: "on_track", reasons: [] }, epics: [], issues: [], ...extra });
function check(ok: boolean, message: string) { if (!ok) throw new Error(message); }
check(checkpointLabel(row("beta", { status: "active" })) === "Active", "100% task completion must not achieve an active checkpoint");
check(checkpointLabel(row("m1", { configured: false })) === "Needs definition", "inferred milestone has no declared checkpoint status");
check(checkpointLabel(row("old")) === "Status not set", "older API keeps missing metadata unknown");
check(currentCheckpoints([row("inferred", { configured: false }), row("closed", { status: "achieved" }), row("next", { status: "planned" })])[0]?.id === "next", "upcoming uses declared order and skips inferred/closed rows");
check(currentCheckpoints([row("next", { status: "planned" }), row("active", { status: "active" }), row("active2", { status: "active" })]).map((r) => r.id).join() === "active,active2", "all explicitly active milestones lead Overview");
check(currentCheckpoints([row("legacy")]).length === 0, "Overview never fabricates an active milestone");
check(checkpointDate("2024-02-29") === "Feb 29, 2024", "calendar dates format in UTC across browser timezones");
check(checkpointDate("2026-02-30") === null && checkpointDate(undefined) === null && checkpointDate("2026-09-01T00:00:00Z") === null, "invalid and missing dates do not masquerade as deadlines");
check(checkpointWork(row("done")) === "1 of 1 tasks complete", "task scope is labelled separately from checkpoint status");
check(scheduleLabel(row("late", { schedule: { state: "overdue", days_remaining: -4 } })) === "4 days overdue", "overdue calendar status is clear");
check(scheduleLabel(row("done", { schedule: { state: "achieved", days_remaining: -4 } })) === null, "achieved checkpoint is not presented as overdue");
console.log("milestone roadmap checks passed");
