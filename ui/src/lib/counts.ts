import { activeCount, matches, type BoardFilters } from "./filters";
import type { IssueCard } from "./types";

/**
 * The one issue-count source. The sidebar, the board header and the
 * overview project summary all read `issueCounts` over the `/api/issues`
 * cards, so they agree by construction; every issue left out is counted
 * under a named exclusion instead of silently disappearing.
 *
 * `open` is the board's default view: leaf issues whose derived status is
 * neither `done` nor `dropped`. Containers (epics — issues with children)
 * are excluded because their status is a roll-up of the leaves already
 * counted.
 */
export interface IssueCounts {
  open: number;
  /** Open issues per derived status (never `done` or `dropped`). */
  byStatus: Record<string, number>;
  done: number;
  dropped: number;
  /** Epics, whatever their status. */
  containers: number;
}

export function issueCounts(issues: IssueCard[], project = "all"): IssueCounts {
  const out: IssueCounts = { open: 0, byStatus: {}, done: 0, dropped: 0, containers: 0 };
  for (const issue of issues) {
    if (project !== "all" && issue.project !== project) continue;
    if (issue.container) out.containers += 1;
    else if (issue.status === "done") out.done += 1;
    else if (issue.status === "dropped") out.dropped += 1;
    else {
      out.open += 1;
      out.byStatus[issue.status] = (out.byStatus[issue.status] ?? 0) + 1;
    }
  }
  return out;
}

/** Every exclusion, labelled: "3 done hidden · 1 dropped · 2 epics". */
export function exclusionLabel(c: IssueCounts, doneShown = false): string {
  const parts: string[] = [];
  if (c.done > 0) parts.push(`${c.done} done ${doneShown ? "shown" : "hidden"}`);
  if (c.dropped > 0) parts.push(`${c.dropped} dropped`);
  if (c.containers > 0) parts.push(`${c.containers} epic${c.containers === 1 ? "" : "s"}`);
  return parts.join(" · ");
}

/** "12 open · 3 done hidden · 2 epics" — the tooltip and summary line. */
export function countLabel(c: IssueCounts): string {
  return [`${c.open} open`, exclusionLabel(c)].filter(Boolean).join(" · ");
}

/** "doing:5  ready:3" in status order, for the overview summary. */
export function statusBreakdown(c: IssueCounts): string {
  return Object.entries(c.byStatus)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([k, v]) => `${k}:${v}`)
    .join("  ");
}

/**
 * The cards the board works over: the project and search slice, minus
 * epics and dropped issues — the same exclusions `issueCounts` names.
 */
export function boardScope(issues: IssueCard[], project: string, query: string): IssueCard[] {
  const q = query.toLowerCase();
  return issues.filter(
    (t) =>
      !t.container &&
      t.status !== "dropped" &&
      (project === "all" || t.project === project) &&
      (!q ||
        (t.id + t.title + (t.owner ?? "") + (t.tags ?? []).join(" "))
          .toLowerCase()
          .includes(q)),
  );
}

/**
 * What the board renders. Finished work stays out of the default view; a
 * typed search looks through it anyway since a query means "find this",
 * not "browse".
 */
export function boardVisible(scope: IssueCard[], filters: BoardFilters, query: string): IssueCard[] {
  return scope.filter(
    (t) => matches(filters, t) && (filters.showDone || t.status !== "done" || query !== ""),
  );
}

/**
 * The board header's count. Un-narrowed it is exactly the sidebar's
 * number ("12 open · 3 done hidden"); a search or facet filter says how
 * much of it is on screen ("4 of 12 open · …").
 */
export function boardHeadline(
  counts: IssueCounts,
  visible: IssueCard[],
  filters: BoardFilters,
  query: string,
): string {
  const narrowed = query !== "" || activeCount(filters) > 0;
  const shownOpen = visible.filter((t) => t.status !== "done").length;
  const head = narrowed ? `${shownOpen} of ${counts.open} open` : `${counts.open} open`;
  return [head, exclusionLabel(counts, filters.showDone)].filter(Boolean).join(" · ");
}
