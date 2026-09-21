import { readFilters, writeFilters, type BoardFilters } from "./filters";

export const APP_TABS = ["overview", "board", "plan", "agents", "memory"] as const;
export type AppTab = (typeof APP_TABS)[number];
export type ProjectView = "kanban" | "list";

const PROJECT_VIEW_KEY = "cadence-project-view";

export interface AppUrlState {
  tab: AppTab;
  view: ProjectView;
  project: string;
  openId: string | null;
  filters: BoardFilters;
}

function parseTab(value: string | null): AppTab | undefined {
  return APP_TABS.includes(value as AppTab) ? (value as AppTab) : undefined;
}

function parseView(value: string | null): ProjectView | undefined {
  return value === "kanban" || value === "list" ? value : undefined;
}

export function readStoredProjectView(storage: Pick<Storage, "getItem"> | undefined): ProjectView | undefined {
  try {
    return parseView(storage?.getItem(PROJECT_VIEW_KEY) ?? null);
  } catch {
    return undefined;
  }
}

export function writeStoredProjectView(
  view: ProjectView,
  storage: Pick<Storage, "setItem"> | undefined,
): void {
  try {
    storage?.setItem(PROJECT_VIEW_KEY, view);
  } catch {
    // Private browsing and locked-down web views can reject localStorage.
  }
}

export function browserStoredProjectView(): ProjectView | undefined {
  if (typeof window === "undefined") return undefined;
  try {
    return readStoredProjectView(window.localStorage);
  } catch {
    return undefined;
  }
}

export function persistBrowserProjectView(view: ProjectView): void {
  if (typeof window === "undefined") return;
  try {
    writeStoredProjectView(view, window.localStorage);
  } catch {
    // A disabled localStorage must not prevent Projects from rendering.
  }
}

/**
 * Read the shareable app state. A view parameter is a legacy Projects link,
 * so it selects the Projects tab even when no tab parameter is present.
 * A project-only URL deliberately remains the Overview workspace.
 */
export function readAppUrlState(search: string, storedView?: ProjectView): AppUrlState {
  const q = new URLSearchParams(search);
  const explicitView = parseView(q.get("view"));
  const explicitTab = parseTab(q.get("tab"));
  return {
    tab: explicitTab ?? (explicitView ? "board" : "overview"),
    view: explicitView ?? storedView ?? "kanban",
    project: q.get("project") ?? "all",
    openId: q.get("issue"),
    filters: readFilters(q),
  };
}

/** Serialize app-owned state while retaining unknown query parameters. */
export function serializeAppUrlState(
  search: string,
  state: Pick<AppUrlState, "tab" | "view" | "project" | "openId" | "filters">,
): string {
  const q = new URLSearchParams(search);
  q.delete("tab");
  q.delete("view");
  q.delete("project");
  q.delete("issue");
  if (state.tab !== "overview") q.set("tab", state.tab);
  if (state.tab === "board") q.set("view", state.view);
  if (state.project !== "all") q.set("project", state.project);
  if (state.openId) q.set("issue", state.openId);
  writeFilters(q, state.filters);
  return q.toString();
}
