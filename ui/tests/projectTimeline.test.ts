import { FILTER_KINDS, entryChange, entryLabel, groupByDay, localDay } from "../src/features/projects/timeline";
import type { TimelineEvent } from "../src/lib/types";

// Shaped like agenticos-v2's `project-timeline.contract.json` (hosted-cadence-project-timeline.v1).
const ev = (id: string, at: string, kind: TimelineEvent["kind"], extra: Partial<TimelineEvent> = {}): TimelineEvent => ({
  id, at, kind, ticket: "ATLAS-1", title: "Add the export button", actor: { name: "Sam Sample", kind: "member" },
  from: null, to: null, landed: false, text: null, to_role: null, decision: null, open: false, open_days: null, ...extra,
});
const events = [
  ev("a", "2026-11-11T17:00:00.000Z", "owner", { to: "users/dana" }),
  ev("b", "2026-11-11T16:00:00.000Z", "status", { from: "review", to: "done", landed: true }),
  ev("c", "2026-11-11T14:00:00.000Z", "status", { from: "doing", to: "review" }),
  ev("d", "2026-11-10T09:00:00.000Z", "message", { to_role: "designer", open: true, open_days: 2 }),
  ev("e", "2026-11-09T09:00:00.000Z", "comment", { text: "Started." }),
];

const fail = (message: string) => { throw new Error(message); };

if (FILTER_KINDS.some((k) => k.kind === "claim") || FILTER_KINDS.length !== 6) fail("the chips are the six recorded kinds, no Claim");

if (entryLabel(events[1]) !== "Landed" || entryLabel(events[2]) !== "Status") fail("only a landed status event reads Landed");
const change = entryChange(events[2]);
if (change?.from !== "doing" || change.to !== "review" || entryChange(events[1])?.to !== "done") fail("a status change shows from and to, landed or not");

const days = groupByDay(events, null);
const keys = events.map((e) => localDay(Date.parse(e.at)));
if (days.length !== new Set(keys).size || days.reduce((n, d) => n + d.entries.length, 0) !== events.length) fail("entries group by local day, newest first");
if (days.some((d) => d.entries.some((x) => x.hereBefore))) fail("no previous visit, no marker");

const visit = Date.parse("2026-11-10T12:00:00.000Z");
const marked = groupByDay(events, visit).flatMap((d) => d.entries).filter((x) => x.hereBefore).map((x) => x.event.id);
if (marked.join() !== "d") fail("the marker sits before the first entry at or before the last visit");
if (groupByDay(events, Date.parse("2026-12-01T00:00:00.000Z")).some((d) => d.entries.some((x) => x.hereBefore))) fail("everything older than the visit: nothing newer, no marker");
if (groupByDay(events, Date.parse("2026-01-01T00:00:00.000Z")).some((d) => d.entries.some((x) => x.hereBefore))) fail("everything newer than the visit: nothing older, no marker");

console.log("project timeline checks passed");
