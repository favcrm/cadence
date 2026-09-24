import { needSections } from "../src/features/home/needSections";
import type { NeedsMe } from "../src/lib/types";

function equal(actual: unknown, expected: unknown): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function row(kind: string, audience?: NeedsMe["audience"]): NeedsMe {
  return { kind, title: kind, age: 0, project: "cadence", command: "c", audience };
}

// Nothing operator-bound: the decision section still renders, with its
// explicit empty state.
const quiet = needSections([row("merge", "team"), row("inbox_unread", "info")], true);
equal(
  quiet.map((s) => [s.key, s.label, s.rows.length, s.empty]),
  [
    ["operator", "Needs your decision", 0, "Nothing needs your decision"],
    ["team", "Team handling", 1, null],
    ["info", "Information", 1, null],
  ],
);
equal(needSections([], true).map((s) => [s.key, s.empty]), [
  ["operator", "Nothing needs your decision"],
]);

// Grouping reads `audience`, not kind: a fenced row (once a static
// team kind) escalated by the server lands under the decision, and an
// approval the server kept as team stays team.
const escalated = needSections(
  [row("fenced", "operator"), row("approval", "team"), row("drift", "dependency")],
  true,
);
equal(
  escalated.map((s) => [s.key, s.rows.map((r) => r.kind)]),
  [
    ["operator", ["fenced"]],
    ["team", ["approval"]],
    ["dependency", ["drift"]],
  ],
);
equal(escalated[0].empty, "Nothing needs your decision");

// Without the decision section (the global list), empty sections hide;
// a row with no audience is team work.
equal(
  needSections([row("stalled")], false).map((s) => [s.key, s.rows.length]),
  [["team", 1]],
);
equal(needSections([], false), []);

console.log("need sections checks passed");
