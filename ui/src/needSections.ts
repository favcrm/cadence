import type { NeedAudience, NeedsMe } from "./types";
import { NEED_SECTION_LABEL, NEEDS_DECISION_EMPTY } from "./uxCopy";

const ORDER: readonly NeedAudience[] = ["operator", "team", "dependency", "info"];

export interface NeedSection {
  key: NeedAudience;
  label: string;
  rows: NeedsMe[];
  /** Copy for a section shown with no rows; null when it is hidden. */
  empty: string | null;
}

/**
 * Group needs-me rows by their server-resolved `audience` (CAD-253) —
 * never by kind. A row without a known audience (an older server) is
 * team work. With `decision`, "Needs your decision" always shows, and
 * says so when it is empty; other sections show only with rows.
 */
export function needSections(rows: NeedsMe[], decision: boolean): NeedSection[] {
  const keyOf = (row: NeedsMe): NeedAudience =>
    row.audience && ORDER.includes(row.audience) ? row.audience : "team";
  return ORDER.map((key) => ({
    key,
    label: NEED_SECTION_LABEL[key],
    rows: rows.filter((row) => keyOf(row) === key),
    empty: key === "operator" && decision ? NEEDS_DECISION_EMPTY : null,
  })).filter((section) => section.rows.length > 0 || section.empty !== null);
}
