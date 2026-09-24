import {
  AWAY_SECS,
  awayLabel,
  awaySince,
  LAST_SEEN_KEY,
  readLastSeen,
  summaryView,
  writeLastSeen,
  type StorageLike,
} from "../src/features/home/sinceLeft";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// Last seen per browser; throwing storage never throws out.
{
  const data = new Map<string, string>();
  const mem: StorageLike = { getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v) };
  equal(readLastSeen(mem), null, "first visit");
  writeLastSeen(1000.7, mem);
  equal([data.get(LAST_SEEN_KEY), readLastSeen(mem)], ["1000", 1000], "round trip");
  data.set(LAST_SEEN_KEY, "garbage");
  equal(readLastSeen(mem), null, "garbage is no value");
  const broken: StorageLike = {
    getItem: () => {
      throw new Error("SecurityError");
    },
    setItem: () => {
      throw new Error("QuotaExceeded");
    },
  };
  equal(readLastSeen(broken), null, "throwing read");
  writeLastSeen(1, broken);
  equal(readLastSeen(null), null, "no storage");
}

// The card shows only after at least an hour away.
{
  const now = 100_000;
  equal(awaySince(null, now), null, "never seen");
  equal(awaySince(now - AWAY_SECS + 1, now), null, "back within the hour");
  equal(awaySince(now - AWAY_SECS, now), now - AWAY_SECS, "an hour");
  equal(awaySince(now + 50, now), null, "clock skew");
  equal([awayLabel(3600), awayLabel(5400), awayLabel(86400), awayLabel(93600)], ["1 h", "1 h", "1 d", "1 d 2 h"], "labels");
}

// master_summary → sections; a missing section is unknown, not empty.
{
  const view = summaryView({
    since: "2026-09-23T00:00:00Z",
    plans_proposed: [{ epic: "D-1", title: "Onboarding", state: "proposed" }],
    plans_decided: [{ epic: "D-0", title: "Old", state: "approved" }],
    tickets_moved: null,
    reports: [{ issue: "D-3", kind: "done", agent: "w1", report: "r.md" }],
    open_questions: [{ issue: "D-4", agent: "w2", escalated: true }],
    routing_backlog: 2,
  });
  equal(view.sections.map((s) => s.label), ["Plans", "Tickets moved", "Reports", "Open questions"], "sections");
  equal(view.sections[0].rows!.map((r) => r.text), ["D-1 Onboarding — proposed", "D-0 Old — approved"], "plans");
  equal(view.sections[1].rows, null, "tickets moved unknown");
  equal(view.sections[2].rows![0], { key: "D-3:0", issue: "D-3", text: "D-3 done by w1" }, "report row");
  equal(view.sections[3].rows![0].text, "D-4 from w2 · with you", "escalated question");
  equal([view.quiet, view.backlog], [false, 2], "not quiet, backlog");
  const quiet = summaryView({ plans_proposed: [], plans_decided: [], tickets_moved: [], reports: [], open_questions: [] });
  equal(quiet.quiet, true, "quiet");
  const old = summaryView({});
  equal([old.quiet, old.sections.every((s) => s.rows === null)], [false, true], "an unknown shape is unknown");
}

console.log("since you left checks passed");
