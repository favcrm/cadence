/**
 * My work's view (CAD-1703): the tab, search and filters, mirrored in the
 * URL (`/my-work?tab=done&search=smtp&project=cadence`) and sent to
 * `GET /api/my-work` as its closed parameter set. Defaults are omitted, so
 * `/my-work` is the Active tab sorted by last update.
 */

export type MyWorkTab = "active" | "done" | "dropped" | "all";
export type MyWorkSort = "updated" | "priority" | "created";
export type MyWorkPriority = "P0" | "P1" | "P2" | "P3";

export interface MyWorkView {
  tab?: Exclude<MyWorkTab, "active">;
  search?: string;
  project?: string;
  priority?: MyWorkPriority;
  blocked?: true;
  sort?: Exclude<MyWorkSort, "updated">;
}

export const MY_WORK_TABS: MyWorkTab[] = ["active", "done", "dropped", "all"];
export const MY_WORK_PRIORITIES: MyWorkPriority[] = ["P0", "P1", "P2", "P3"];
export const MY_WORK_SORTS: MyWorkSort[] = ["updated", "priority", "created"];
/** The server's limit on a search (characters). */
export const MY_WORK_SEARCH_MAX = 200;

/** URL keys this view owns; navigation elsewhere drops them. */
export const MY_WORK_URL_KEYS = ["tab", "search", "project", "priority", "blocked", "sort"] as const;

const PROJECT_KEY = /^[a-z0-9][a-z0-9-]{0,31}$/;

/** Read the view from a URL query. Unknown or malformed values fall back to the default. */
export function readMyWorkView(q: URLSearchParams): MyWorkView {
  const view: MyWorkView = {};
  const tab = q.get("tab");
  if (tab === "done" || tab === "dropped" || tab === "all") view.tab = tab;
  const search = q.get("search")?.trim().slice(0, MY_WORK_SEARCH_MAX);
  if (search) view.search = search;
  const project = q.get("project");
  if (project && PROJECT_KEY.test(project)) view.project = project;
  const priority = q.get("priority");
  if (MY_WORK_PRIORITIES.includes(priority as MyWorkPriority)) view.priority = priority as MyWorkPriority;
  if (q.get("blocked") === "1") view.blocked = true;
  const sort = q.get("sort");
  if (sort === "priority" || sort === "created") view.sort = sort;
  return view;
}

/** Write the view into a URL query, replacing whatever it held before. */
export function writeMyWorkView(q: URLSearchParams, view: MyWorkView): void {
  for (const key of MY_WORK_URL_KEYS) q.delete(key);
  if (view.tab) q.set("tab", view.tab);
  if (view.search) q.set("search", view.search);
  if (view.project) q.set("project", view.project);
  if (view.priority) q.set("priority", view.priority);
  if (view.blocked) q.set("blocked", "1");
  if (view.sort) q.set("sort", view.sort);
}

/** Whether anything narrows the results (the tab and sort do not). */
export function myWorkFiltered(view: MyWorkView): boolean {
  return Boolean(view.search || view.project || view.priority || view.blocked);
}

/** The API query for one page. The server derives "my" from the session alone. */
export function myWorkApiQuery(view: MyWorkView, page: { cursor?: string | null; limit: number }): string {
  const q = new URLSearchParams();
  if (view.tab) q.set("tab", view.tab);
  if (view.search) q.set("q", view.search);
  if (view.project) q.set("project", view.project);
  if (view.priority) q.set("priority", view.priority);
  if (view.blocked) q.set("blocked", "1");
  if (view.sort) q.set("sort", view.sort);
  if (page.cursor) q.set("cursor", page.cursor);
  q.set("limit", String(page.limit));
  return q.toString();
}

/** A stable identity for a view: equal views, equal keys. */
export function myWorkViewKey(view: MyWorkView): string {
  const q = new URLSearchParams();
  writeMyWorkView(q, view);
  return q.toString();
}
