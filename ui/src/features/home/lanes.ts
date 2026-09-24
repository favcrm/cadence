import type { OverviewLane } from "../../lib/types";

/** The PR as `#<n>` when it is a GitHub pull URL, else as recorded. */
function prLabel(pr: string): string {
  const m = /\/pull\/(\d+)/.exec(pr);
  return m ? `#${m[1]}` : pr;
}

/** CAD-378: one open lane on one line — who, which PR, the code areas
 *  it plans or changes, and the lanes it overlaps. */
export function laneSummary(l: OverviewLane): string {
  const parts = [l.issue, l.worker ?? "no worker", l.pr ? `PR ${prLabel(l.pr)}` : "no PR"];
  if (l.areas.length > 0) parts.push(`areas ${l.areas.join(", ")}`);
  if (l.overlaps.length > 0) parts.push(`overlaps ${l.overlaps.join(", ")}`);
  return parts.join(" · ");
}

/** Lanes worth showing first: overlapping ones, then those in an area. */
export function sortLanes(lanes: OverviewLane[]): OverviewLane[] {
  const weight = (l: OverviewLane) => (l.overlaps.length > 0 ? 0 : l.areas.length > 0 ? 1 : 2);
  return [...lanes].sort((a, b) => weight(a) - weight(b) || a.issue.localeCompare(b.issue));
}
