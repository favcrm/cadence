import { laneSummary, sortLanes } from "../src/features/home/lanes";
import type { OverviewLane } from "../src/lib/types";

function equal(actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
  }
}

function lane(issue: string, areas: string[], overlaps: string[], pr: string | null): OverviewLane {
  return { issue, worker: `w-${issue}`, pm: "pm", pr, planned: [], changed_count: 0, areas, overlaps };
}

// CAD-378: one line per open lane, naming its PR, areas and overlaps.
equal(
  laneSummary(lane("CAD-1", ["caller"], ["CAD-2"], "https://github.com/o/r/pull/9")),
  "CAD-1 · w-CAD-1 · PR #9 · areas caller · overlaps CAD-2",
);
equal(laneSummary(lane("CAD-3", [], [], null)), "CAD-3 · w-CAD-3 · no PR");
equal(
  sortLanes([lane("CAD-5", [], [], null), lane("CAD-4", ["a"], [], null), lane("CAD-6", [], ["CAD-5"], null)])
    .map((l) => l.issue)
    .join(","),
  "CAD-6,CAD-4,CAD-5",
);

console.log("lanes checks passed");
