import type { IssueCard } from "./types";

/** The board's slice — chips in the filter bar, mirrored in the URL
 *  (`?tag=ui,api&epic=CAD-38&owner=ann&component=core&group=epic`) so a
 *  filtered view is a link. Tags narrow (all of them); epic, owner and
 *  component widen within themselves (any of them). */
export interface BoardFilters {
  tags: string[];
  epics: string[];
  owners: string[];
  components: string[];
  /** Render one swimlane per epic. */
  groupByEpic: boolean;
}

export type Facet = "tags" | "epics" | "owners" | "components";

export const NO_FILTERS: BoardFilters = {
  tags: [],
  epics: [],
  owners: [],
  components: [],
  groupByEpic: false,
};

const PARAMS: [Facet, string][] = [
  ["tags", "tag"],
  ["epics", "epic"],
  ["owners", "owner"],
  ["components", "component"],
];

export function readFilters(q: URLSearchParams): BoardFilters {
  const list = (key: string) =>
    q
      .getAll(key)
      .flatMap((v) => v.split(","))
      .filter(Boolean);
  const out = { ...NO_FILTERS, groupByEpic: q.get("group") === "epic" };
  for (const [facet, key] of PARAMS) out[facet] = list(key);
  return out;
}

export function writeFilters(q: URLSearchParams, f: BoardFilters) {
  for (const [facet, key] of PARAMS) {
    if (f[facet].length) q.set(key, f[facet].join(","));
  }
  if (f.groupByEpic) q.set("group", "epic");
}

export function toggle(f: BoardFilters, facet: Facet, value: string): BoardFilters {
  const cur = f[facet];
  return {
    ...f,
    [facet]: cur.includes(value) ? cur.filter((v) => v !== value) : [...cur, value],
  };
}

export const activeCount = (f: BoardFilters) =>
  PARAMS.reduce((n, [facet]) => n + f[facet].length, 0);

export function matches(f: BoardFilters, t: IssueCard): boolean {
  const any = (want: string[], have?: string) =>
    want.length === 0 || (have !== undefined && want.includes(have));
  return (
    f.tags.every((tag) => (t.tags ?? []).includes(tag)) &&
    any(f.epics, t.parent) &&
    any(f.owners, t.owner) &&
    any(f.components, t.component)
  );
}

/** An epic's progress over all of its children — not only the ones the
 *  current filter leaves visible. Dropped children leave the base, the
 *  same ratio `issue epic ls` reports. */
export function epicProgress(issues: IssueCard[], epic: string) {
  const kids = issues.filter((i) => i.parent === epic);
  const live = kids.filter((k) => k.status !== "dropped");
  const done = live.filter((k) => k.status === "done").length;
  return { done, total: live.length, ratio: live.length ? done / live.length : 0 };
}
