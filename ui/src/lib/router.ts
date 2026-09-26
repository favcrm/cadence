import { readFilters, writeFilters, NO_FILTERS, type BoardFilters } from "./filters";
import { readAppUrlState, type AppTab, type ProjectView } from "./urlState";

/**
 * The app's routes — real paths with browser history, so every screen has
 * a link that survives a refresh or a paste:
 *
 *   /                          Home — the master's thread (CAD-328)
 *   /overview                  the team overview (needs, drift, monitors)
 *   /projects[/:slug]          a project's issues (board or list)
 *   /projects/:slug/epics      its epics: stage, progress, health (CAD-432)
 *   /projects/:slug/milestones its milestones: progress, worst health
 *   /projects/:slug/context    its context
 *   /projects/:slug/workflows  its stored workflows and new runs (CAD-496)
 *   /apps[/<project>/<app>]    installed apps, optionally one app's detail (CAD-557)
 *   /agents[/:alias]           agents, optionally one agent's drawer
 *   /wiki[/<path>]             the wiki: folder tree + page (CAD-581)
 *   /wiki/edit|history|upload/<path>   its modes for one path
 *   /wiki/search[/<query>]     full-text search
 *   /setup                     first-run setup
 *   /settings[/memory]         model defaults, memory
 *   /login                     a `cadence ui login` link lands here
 *
 * Query parameters carry the rest: `project` (the scope on screens whose
 * path has no slug), `issue` (the open issue drawer, on any screen),
 * `view` and the filter keys (Projects only). Parameters the app does not
 * own are kept. Pure functions — the React side is lib/useLocation.ts —
 * so routing is unit-tested in plain node (tests/router.test.ts).
 */

export type ProjectSection = "issues" | "epics" | "milestones" | "context" | "workflows";
export type SettingsSection = "models" | "memory" | "update";
/** The wiki's modes; `browse` opens a path by its kind (CAD-581). */
export type WikiMode = "browse" | "edit" | "history" | "search" | "upload";

export type Route =
  | { screen: "home" }
  | { screen: "overview" }
  | { screen: "projects"; slug: string | null; section: ProjectSection }
  | { screen: "apps"; project: string | null; name: string | null }
  | { screen: "agents"; alias: string | null }
  | { screen: "wiki"; mode: WikiMode; path: string | null; query: string | null }
  | { screen: "outbox" }
  | { screen: "setup" }
  | { screen: "settings"; section: SettingsSection }
  | { screen: "login" }
  | { screen: "notFound"; path: string };

export type Screen = Route["screen"];

/** The main navigation — MVP screens only (Setup is reached from Home). */
export const NAV: { screen: Screen; label: string; route: Route }[] = [
  { screen: "home", label: "Home", route: { screen: "home" } },
  { screen: "projects", label: "Projects", route: { screen: "projects", slug: null, section: "issues" } },
  { screen: "wiki", label: "Wiki", route: { screen: "wiki", mode: "browse", path: null, query: null } },
  { screen: "apps", label: "Apps", route: { screen: "apps", project: null, name: null } },
  { screen: "agents", label: "Agents", route: { screen: "agents", alias: null } },
  { screen: "outbox", label: "Outbox", route: { screen: "outbox" } },
  { screen: "settings", label: "Settings", route: { screen: "settings", section: "models" } },
];

export interface AppLocation {
  route: Route;
  /** The project scope: the slug on Projects, else `?project=`; "all" when none. */
  project: string;
  view: ProjectView;
  openId: string | null;
  filters: BoardFilters;
}

function segment(value: string): string | null {
  try {
    const decoded = decodeURIComponent(value);
    return decoded === "" ? null : decoded;
  } catch {
    return null;
  }
}

/** Decode the segments from `from` on; empty and malformed ones drop out. */
function pathFrom(parts: string[], from: number): string {
  return parts
    .slice(from)
    .map(segment)
    .filter((s): s is string => s !== null)
    .join("/");
}

/**
 * The wiki's paths (CAD-581). `edit`, `history`, `upload` and `search` are
 * reserved first segments — the modes' homes; anything else is a path into
 * the store, which `browse` opens by its kind.
 */
function matchWiki(parts: string[]): Route {
  const mode = parts[1];
  if (mode === undefined) return { screen: "wiki", mode: "browse", path: null, query: null };
  if (mode === "search") {
    const query = pathFrom(parts, 2);
    return { screen: "wiki", mode: "search", path: null, query: query || null };
  }
  if (mode === "edit" || mode === "history" || mode === "upload") {
    return { screen: "wiki", mode, path: pathFrom(parts, 2) || null, query: null };
  }
  return { screen: "wiki", mode: "browse", path: pathFrom(parts, 1) || null, query: null };
}

export function matchRoute(pathname: string): Route {
  const parts = pathname.split("/").filter(Boolean);
  const [head, a, b, ...rest] = parts;
  if (head === "wiki") return matchWiki(parts);
  if (rest.length === 0) {
    if (!head && !a) return { screen: "home" };
    if (head === "index.html" && !a) return { screen: "home" };
    if (head === "projects") {
      if (!a) return { screen: "projects", slug: null, section: "issues" };
      const slug = segment(a);
      if (slug && !b) return { screen: "projects", slug, section: "issues" };
      if (slug && (b === "context" || b === "epics" || b === "milestones" || b === "workflows")) {
        return { screen: "projects", slug, section: b };
      }
    }
    if (head === "apps") {
      if (!a) return { screen: "apps", project: null, name: null };
      const project = segment(a);
      const name = b ? segment(b) : null;
      if (project && name) return { screen: "apps", project, name };
    }
    if (head === "agents" && !b) {
      if (!a) return { screen: "agents", alias: null };
      const alias = segment(a);
      if (alias) return { screen: "agents", alias };
    }
    if (head === "setup" && !a) return { screen: "setup" };
    if (head === "login" && !a) return { screen: "login" };
    if (head === "overview" && !a) return { screen: "overview" };
    // `?item=<effect_id>` deep-links one published item (CAD-546).
    if (head === "outbox" && !a) return { screen: "outbox" };
    if (head === "settings" && !b) {
      if (!a) return { screen: "settings", section: "models" };
      if (a === "memory") return { screen: "settings", section: "memory" };
      if (a === "update") return { screen: "settings", section: "update" };
    }
  }
  return { screen: "notFound", path: pathname };
}

export function routePath(route: Route): string {
  switch (route.screen) {
    case "home":
      return "/";
    case "projects": {
      if (!route.slug) return "/projects";
      const base = `/projects/${encodeURIComponent(route.slug)}`;
      return route.section === "issues" ? base : `${base}/${route.section}`;
    }
    case "apps":
      if (!route.project) return "/apps";
      return route.name
        ? `/apps/${encodeURIComponent(route.project)}/${encodeURIComponent(route.name)}`
        : "/apps";
    case "agents":
      return route.alias ? `/agents/${encodeURIComponent(route.alias)}` : "/agents";
    case "wiki": {
      const path = route.path ? route.path.split("/").map(encodeURIComponent).join("/") : "";
      if (route.mode === "search") {
        return route.query ? `/wiki/search/${encodeURIComponent(route.query)}` : "/wiki/search";
      }
      if (route.mode === "browse") return path ? `/wiki/${path}` : "/wiki";
      return path ? `/wiki/${route.mode}/${path}` : `/wiki/${route.mode}`;
    }
    case "setup":
      return "/setup";
    case "login":
      return "/login";
    case "overview":
      return "/overview";
    case "outbox":
      return "/outbox";
    case "settings":
      if (route.section === "memory") return "/settings/memory";
      return route.section === "update" ? "/settings/update" : "/settings";
    case "notFound":
      return route.path;
  }
}

function parseView(value: string | null): ProjectView | undefined {
  return value === "kanban" || value === "list" ? value : undefined;
}

export function readLocation(pathname: string, search: string, storedView?: ProjectView): AppLocation {
  const route = matchRoute(pathname);
  const q = new URLSearchParams(search);
  const onProjects = route.screen === "projects";
  return {
    route,
    // On an app's detail route the path's project is the scope — the
    // sidebar highlights it like the slug does on Projects.
    project: onProjects
      ? (route.slug ?? "all")
      : route.screen === "apps" && route.project
        ? route.project
        : (q.get("project") ?? "all"),
    view: parseView(q.get("view")) ?? storedView ?? "list",
    openId: q.get("issue"),
    filters: onProjects ? readFilters(q) : NO_FILTERS,
  };
}

/**
 * The route with the project scope folded in: on Projects the scope is the
 * slug (a section needs one — context without a project falls back to the
 * issues of all projects).
 */
function scopedRoute(route: Route, project: string): Route {
  if (route.screen !== "projects") return route;
  const slug = project === "all" ? null : project;
  return { screen: "projects", slug, section: slug ? route.section : "issues" };
}

/** Path plus query for a location, keeping query parameters the app does not own. */
export function locationHref(loc: AppLocation, search = ""): string {
  const q = new URLSearchParams(search);
  // `run` is owned by the Workflows section — it opens one row's form
  // there — and `new` by an app page (it opens the New post drawer on
  // arrival); neither follows a navigation elsewhere.
  for (const key of ["tab", "view", "project", "issue", "run", "new"]) q.delete(key);
  writeFilters(q, NO_FILTERS);
  const route = scopedRoute(loc.route, loc.project);
  if (route.screen === "projects") {
    if (route.section === "issues") {
      q.set("view", loc.view);
      writeFilters(q, loc.filters);
    }
  } else if (
    loc.project !== "all" &&
    !(route.screen === "apps" && route.project)
  ) {
    q.set("project", loc.project);
  }
  if (loc.openId) q.set("issue", loc.openId);
  const s = q.toString();
  return routePath(route) + (s ? `?${s}` : "");
}

/** Each pre-router tab's new home. */
const LEGACY_TABS: Record<AppTab, Route> = {
  overview: { screen: "home" },
  board: { screen: "projects", slug: null, section: "issues" },
  plan: { screen: "projects", slug: null, section: "context" },
  agents: { screen: "agents", alias: null },
  memory: { screen: "settings", section: "memory" },
  settings: { screen: "settings", section: "models" },
};

/**
 * Where a pre-router URL now lives. The board used to be one page with
 * `?tab=` (overview, board, plan, agents, memory, settings) and `?view=`
 * for the board; those links redirect to their route with the project,
 * issue and filters intact. Returns null for a URL that needs no redirect.
 */
export function legacyRedirect(
  pathname: string,
  search: string,
  storedView?: ProjectView,
): string | null {
  const q = new URLSearchParams(search);
  if (matchRoute(pathname).screen !== "home" || (!q.has("tab") && !q.has("view"))) {
    // `/index.html` is Home under its file name.
    return pathname === "/index.html" ? `/${search}` : null;
  }
  const old = readAppUrlState(search, storedView);
  const next = LEGACY_TABS[old.tab];
  return locationHref(
    {
      route: next,
      project: old.project,
      view: old.view,
      openId: old.openId,
      filters: next.screen === "projects" ? old.filters : NO_FILTERS,
    },
    search,
  );
}

/** The location after moving to another screen: scope and drawer come along. */
export function goTo(current: AppLocation, route: Route): AppLocation {
  const project = route.screen === "projects" && route.slug ? route.slug : current.project;
  return { ...current, route, project };
}

/** The location scoped to another project, staying on the same screen. */
export function withProject(current: AppLocation, project: string): AppLocation {
  return { ...current, project };
}
