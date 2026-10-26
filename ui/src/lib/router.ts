import { parseIssueTab, type IssueTab } from "../features/issues/model";
import { parseAgentTab, type AgentTab } from "../features/agents/agentTabModel";
import { readFilters, writeFilters, NO_FILTERS, type BoardFilters } from "./filters";
import { readAppUrlState, type AppTab, type ProjectView } from "./urlState";
import { MY_WORK_URL_KEYS, readMyWorkView, writeMyWorkView, type MyWorkView } from "./myWorkQuery";

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
 *   /projects/:slug/issues/:id one issue (CAD-607). `?tab=` picks a section.
 *   /apps[/<project>/<app>]    installed apps, optionally one app's detail (CAD-557)
 *   /apps/<app-key>            one installed app resolved by package key
 *   /app-installations/<id>    retired (CAD-1438): compatibility redirect
 *                              to the same install's `/apps/<key>`, or a
 *                              clear retired page when no unique mapping
 *   /agents[/:alias]           agents, optionally one agent's page (CAD-1752); `?tab=` picks a section, `?recruit=1` the catalogue
 *   /threads                   Threads: agent, operator and group conversations (CAD-818); `?c=<id>` opens one
 *   /wiki[/<path>]             the wiki: folder tree + page (CAD-581)
 *   /wiki/edit|history|upload/<path>   its modes for one path
 *   /wiki/search[/<query>]     full-text search
 *   /setup                     first-run setup
 *   /settings[/memory|update|account|permissions|connections]  model defaults, memory, update, account, master permissions, connections
 *   /settings/connections/new[/<provider>]   the add-connection chooser and one pinned service's setup
 *   /settings/connections/account/<id>       one connection's detail page
 *   /login                     a `cadence ui login` link lands here
 *
 * Query parameters carry the rest: `project` (the in-page filter on
 * screens that have one — Agents, Team overview and Settings → Memory),
 * `issue` (the open issue drawer, on any screen), `view` and the filter
 * keys (Projects only). Home, Wiki, Outbox, the Apps home and the rest
 * of Settings carry no project scope. Parameters the app does not own
 * are kept. Pure functions — the React side is lib/useLocation.ts — so
 * routing is unit-tested in plain node (tests/router.test.ts).
 */

export type ProjectSection = "overview" | "issues" | "epics" | "milestones" | "context" | "workflows";
export type SettingsSection = "models" | "memory" | "update" | "account" | "permissions" | "connections" | "email" | "templates";
/** The wiki's modes; `browse` opens a path by its kind (CAD-581). */
export type WikiMode = "browse" | "edit" | "history" | "search" | "upload";

/**
 * CAD-1319: the Connections section's sub-pages. Absent = the index;
 * `add` = the service chooser; `addProvider` = one service's pinned
 * setup (the canonical provider name); `detail` = one connection's
 * page (its id). Never carries a credential — only identifiers.
 */
export type ConnectionPage =
  | { kind: "add" }
  | { kind: "addProvider"; provider: string }
  | { kind: "detail"; id: string };

export type Route =
  | { screen: "home" }
  | { screen: "overview" }
  | { screen: "projects"; slug: string | null; section: ProjectSection }
  | { screen: "issue"; project: string; id: string; tab: IssueTab }
  | { screen: "apps"; project: string | null; name: string | null }
  | { screen: "appsExplore" }
  | { screen: "appsCatalog"; id: string }
  | { screen: "appsManage"; installId: string }
  | { screen: "workspaceApp"; installId: string }
  | { screen: "workspaceAppKey"; appKey: string }
  | { screen: "agents"; alias: string | null; tab?: AgentTab; recruit?: boolean; ask?: boolean }
  | { screen: "threads" }
  /** CAD-1703: the tab, search and filters, from the query (`?tab=done&search=…`). */
  | { screen: "myWork"; view?: MyWorkView }
  | { screen: "wiki"; mode: WikiMode; path: string | null; query: string | null; at?: string; section?: string; page?: number }
  | { screen: "outbox" }
  | { screen: "setup" }
  | { screen: "settings"; section: SettingsSection; page?: ConnectionPage }
  | { screen: "login" }
  | { screen: "notFound"; path: string };

export type Screen = Route["screen"];

/** The main navigation — MVP screens only (Setup is reached from Home). */
export const NAV: { screen: Screen; label: string; route: Route }[] = [
  { screen: "home", label: "Home", route: { screen: "home" } },
  { screen: "myWork", label: "My work", route: { screen: "myWork" } },
  { screen: "threads", label: "Threads", route: { screen: "threads" } },
  { screen: "projects", label: "Projects", route: { screen: "projects", slug: null, section: "overview" } },
  { screen: "wiki", label: "Wiki", route: { screen: "wiki", mode: "browse", path: null, query: null } },
  { screen: "apps", label: "Apps", route: { screen: "apps", project: null, name: null } },
  { screen: "agents", label: "Agents", route: { screen: "agents", alias: null } },
  // CAD-1312: Outbox leaves the nav (its route, API and page remain).
  { screen: "settings", label: "Settings", route: { screen: "settings", section: "models" } },
];

/**
 * The nav a board carries. A hosted tenant does not serve the wiki (the shared
 * UI's requests answer 501), so it is not advertised there; a local board keeps
 * it. The route still explains itself if reached by a deep link.
 */
export function navItems(hosted: boolean): typeof NAV {
  return hosted ? NAV.filter((item) => item.screen !== "wiki") : NAV;
}

/**
 * Where a project lives on a screen. The in-page chip row and the memory
 * picker read this. The sidebar and the phone menu do not: their highlight
 * is `route.screen === "projects"` (`navProject` in App.tsx).
 *
 * - `slug` — the project page (`/projects/:slug`). A sidebar project opens it.
 * - `chips` — an in-page chip row, stored as `?project=` (Team overview).
 *   Agents belong to the team, not a project (CAD-1752), so `/agents` has none.
 * - `picker` — an in-page project picker, stored as `?project=`
 *   (Settings → Memory).
 * - `none` — no project scope in the URL or in state.
 */
export type ProjectScope = "slug" | "chips" | "picker" | "none";

export function projectScope(route: Route): ProjectScope {
  switch (route.screen) {
    case "projects":
      return "slug";
    case "overview":
      return "chips";
    case "settings":
      return route.section === "memory" ? "picker" : "none";
    default:
      // `/apps` is a workspace home (CAD-1607): it has no project scope,
      // so it neither shows a chip row nor keeps `?project=` in its URL.
      return "none";
  }
}

/** Screens that keep `?project=` as a visible in-page filter. */
export function projectFilter(route: Route): boolean {
  const scope = projectScope(route);
  return scope === "chips" || scope === "picker";
}

/**
 * The chip row and the memory picker are visible when there is a real
 * choice: more than one project, or a selected project that would
 * otherwise filter the page with nothing on screen to say so.
 */
export function showProjectChoices(projectCount: number, selected: string): boolean {
  return projectCount > 1 || (selected !== "all" && selected !== "");
}

export interface AppLocation {
  route: Route;
  /** The project page's slug, or the in-page filter; "all" when the screen has neither. */
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
  if (head === "projects" && b === "issues" && rest.length === 1) {
    const slug = a ? segment(a) : null;
    const issueId = segment(rest[0]);
    if (slug && issueId) return { screen: "issue", project: slug, id: issueId, tab: "overview" };
  }
  if (rest.length === 0 || (head === "settings" && a === "connections" && rest.length === 1)) {
    if (!head && !a && rest.length === 0) return { screen: "home" };
    if (head === "index.html" && !a && rest.length === 0) return { screen: "home" };
    if (head === "projects") {
      if (!a) return { screen: "projects", slug: null, section: "overview" };
      const slug = segment(a);
      if (slug && !b) return { screen: "projects", slug, section: "overview" };
      if (slug && (b === "overview" || b === "issues" || b === "context" || b === "epics" || b === "milestones" || b === "workflows")) {
        return { screen: "projects", slug, section: b };
      }
    }
    if (head === "apps") {
      // CAD-1129: the Apps home's siblings — the Explorer
      // (`/apps/explore`), one catalog app (`/apps/catalog/<id>`) and
      // the operator's manage tab (`/apps/manage/<install_id>`). The
      // bare `/apps/<a>` shape is reserved for these reserved words so
      // a project named "explore"/"catalog"/"manage" never collides.
      // The remaining bare `/apps/<key>` shape opens an installed package key;
      // project-qualified `/apps/<project>/<name>` links remain legacy details.
      if (!a) return { screen: "apps", project: null, name: null };
      if (a === "explore" && !b) return { screen: "appsExplore" };
      if (a === "catalog" && b) {
        const id = segment(b);
        if (id && !rest.length) return { screen: "appsCatalog", id };
      }
      if (a === "manage" && b) {
        const installId = segment(b);
        if (installId && /^[A-Za-z0-9_-]{1,128}$/.test(installId) && !rest.length) {
          return { screen: "appsManage", installId };
        }
      }
      const project = segment(a);
      const name = b ? segment(b) : null;
      if (project && name) return { screen: "apps", project, name };
      if (
        !b && project &&
        !["explore", "catalog", "manage"].includes(project) &&
        /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(project) &&
        project !== "." && project !== ".."
      ) {
        return { screen: "workspaceAppKey", appKey: project };
      }
    }
    if (head === "app-installations" && a && !b) {
      const installId = segment(a);
      if (installId && /^[A-Za-z0-9_-]{1,128}$/.test(installId)) return { screen: "workspaceApp", installId };
    }
    if (head === "agents" && !b) {
      if (!a) return { screen: "agents", alias: null };
      const alias = segment(a);
      if (alias) return { screen: "agents", alias };
    }
    if (head === "threads" && !a) return { screen: "threads" };
    if (head === "setup" && !a) return { screen: "setup" };
    if (head === "login" && !a) return { screen: "login" };
    if (head === "overview" && !a) return { screen: "overview" };
    // `?item=<effect_id>` deep-links one published item (CAD-546).
    if (head === "outbox" && !a) return { screen: "outbox" };
    if (head === "my-work" && !a) return { screen: "myWork" };
    // `!b` keeps the original one-segment shape; the connections
    // section alone may carry its /new, /new/<provider> and
    // /account/<id> sub-pages (bounded to one extra segment above).
    if (head === "settings" && (!b || a === "connections")) {
      if (!a) return { screen: "settings", section: "models" };
      if (a === "memory") return { screen: "settings", section: "memory" };
      if (a === "update") return { screen: "settings", section: "update" };
      if (a === "account") return { screen: "settings", section: "account" };
      if (a === "permissions") return { screen: "settings", section: "permissions" };
      if (a === "email") return { screen: "settings", section: "email" };
      if (a === "templates") return { screen: "settings", section: "templates" };
      if (a === "connections") {
        if (!b) return { screen: "settings", section: "connections" };
        // CAD-1319: the dedicated pages — the add chooser, one pinned
        // service's setup and one connection's detail. `parts` dropped
        // empty segments, so the RAW path is re-checked here: an
        // internal `//` (a dropped empty segment) must not normalize
        // into a valid sub-page. A trailing slash still normalizes,
        // like every other route; the index above is untouched.
        const rawSegs = pathname.split("/").slice(1);
        const emptyInternal = rawSegs.some((s, i) => s === "" && i < rawSegs.length - 1);
        if (!emptyInternal) {
          if (b === "new" && rest.length === 0) {
            return { screen: "settings", section: "connections", page: { kind: "add" } };
          }
          if (b === "new" && rest.length === 1) {
            const provider = segment(rest[0]);
            if (provider) {
              return { screen: "settings", section: "connections", page: { kind: "addProvider", provider } };
            }
          }
          if (b === "account" && rest.length === 1) {
            const id = segment(rest[0]);
            if (id) {
              return { screen: "settings", section: "connections", page: { kind: "detail", id } };
            }
          }
        }
      }
    }
  }
  return { screen: "notFound", path: pathname };
}

export function routePath(route: Route): string {
  switch (route.screen) {
    case "workspaceApp":
      // CAD-1438: kept only so the compatibility route round-trips; no
      // producer emits it anymore.
      return `/app-installations/${encodeURIComponent(route.installId)}`;
    case "workspaceAppKey":
      return `/apps/${encodeURIComponent(route.appKey)}`;
    case "home":
      return "/";
    case "projects": {
      if (!route.slug) return "/projects";
      const base = `/projects/${encodeURIComponent(route.slug)}`;
      return route.section === "overview" ? base : `${base}/${route.section}`;
    }
    case "issue":
      return `/projects/${encodeURIComponent(route.project)}/issues/${encodeURIComponent(route.id)}`;
    case "apps":
      if (!route.project) return "/apps";
      return route.name
        ? `/apps/${encodeURIComponent(route.project)}/${encodeURIComponent(route.name)}`
        : "/apps";
    case "appsExplore":
      return "/apps/explore";
    case "appsCatalog":
      return `/apps/catalog/${encodeURIComponent(route.id)}`;
    case "appsManage":
      return `/apps/manage/${encodeURIComponent(route.installId)}`;
    case "agents":
      return route.alias ? `/agents/${encodeURIComponent(route.alias)}` : "/agents";
    case "threads":
      return "/threads";
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
    case "myWork":
      return "/my-work";
    case "settings":
      if (route.section === "account") return "/settings/account";
      if (route.section === "memory") return "/settings/memory";
      if (route.section === "update") return "/settings/update";
      if (route.section === "permissions") return "/settings/permissions";
      if (route.section === "connections") {
        if (route.page?.kind === "add") return "/settings/connections/new";
        if (route.page?.kind === "addProvider") {
          return `/settings/connections/new/${encodeURIComponent(route.page.provider)}`;
        }
        if (route.page?.kind === "detail") {
          return `/settings/connections/account/${encodeURIComponent(route.page.id)}`;
        }
        return "/settings/connections";
      }
      if (route.section === "email") return "/settings/email";
      if (route.section === "templates") return "/settings/templates";
      return "/settings";
    case "notFound":
      return route.path;
  }
}

function parseView(value: string | null): ProjectView | undefined {
  return value === "kanban" || value === "list" ? value : undefined;
}

export function readLocation(pathname: string, search: string, storedView?: ProjectView): AppLocation {
  const matched = matchRoute(pathname);
  const q = new URLSearchParams(search);
  let route = matched.screen === "issue" ? { ...matched, tab: parseIssueTab(q.get("tab")) } : matched;
  if (route.screen === "wiki") {
    const at = q.get("at");
    const section = q.get("section");
    const page = q.get("page");
    const parsedPage = page !== null && /^[1-9][0-9]{0,5}$/.test(page) ? Number(page) : undefined;
    if (at !== null || section || parsedPage !== undefined) {
      route = {
        ...route,
        ...(at !== null ? { at } : {}),
        ...(section ? { section } : {}),
        ...(parsedPage !== undefined ? { page: parsedPage } : {}),
      };
    }
  }
  if (route.screen === "projects" && route.section === "overview" && q.has("view")) {
    route = { ...route, section: "issues" };
  }
  if (route.screen === "agents") {
    const tab = parseAgentTab(q.get("tab"));
    route = {
      screen: "agents",
      alias: route.alias,
      ...(route.alias && tab !== "overview" ? { tab } : {}),
      ...(!route.alias && q.get("recruit") === "1" ? { recruit: true } : {}),
      ...(!route.alias && q.get("ask") === "1" ? { ask: true } : {}),
    };
  }
  if (route.screen === "myWork") {
    const view = readMyWorkView(q);
    route = Object.keys(view).length ? { screen: "myWork", view } : { screen: "myWork" };
  }
  const onProjects = route.screen === "projects";
  const queried = q.get("project");
  return {
    route,
    // The slug is the project on a project page. An issue page carries its
    // project in the path. A filter screen reads `?project=`. Every other
    // screen ignores it — an app detail's project lives on the route.
    project:
      route.screen === "issue"
        ? route.project
        : route.screen === "projects"
          ? (route.slug ?? "all")
          : projectFilter(route)
            ? queried || "all"
            : "all",
    view: parseView(q.get("view")) ?? storedView ?? "list",
    openId: route.screen === "issue" ? null : q.get("issue"),
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
  return { screen: "projects", slug, section: slug ? route.section : route.section === "overview" ? "overview" : "issues" };
}

/** Query parameters only the mounted app route owns. `install` is the
 *  CAD-1438 pin of a generated contextual link — the immutable install id
 *  the ctx/conversation/record selectors were minted for. */
const CRM_APP_PARAMS = ["crm", "record", "ctx", "appview", "segment", "install"] as const;

/** Path plus query for a location, keeping query parameters the app does not own. */
export function locationHref(loc: AppLocation, search = "", from: Route | null = null): string {
  const q = new URLSearchParams(search);
  // `run` is owned by the Workflows section — it opens one row's form
  // there — and `new` by an app page (it opens the New post drawer on
  // arrival); neither follows a navigation elsewhere. `screen=native` is
  // an app page's too (CAD-1040: the operator's back door to native
  // controls instead of the mounted app screen); it never carries over to
  // another installation.
  // CAD-1811: Threads owns its list scope in `?project=` (threadsView). It is
  // not a board filter, so opening or closing the shared issue detail there
  // must carry it across the delete below. Only a Threads → Threads move
  // keeps it: another screen's `?project=` is that screen's filter.
  const threadsProject = from?.screen === "threads" && loc.route.screen === "threads" ? q.get("project") : null;
  for (const key of ["tab", "view", "project", "issue", "run", "new", "screen", "at", "section", "page", "recruit", "ask"]) q.delete(key);
  // CAD-1703: My work's view belongs to its own route.
  for (const key of MY_WORK_URL_KEYS) q.delete(key);
  writeFilters(q, NO_FILTERS);
  const route = scopedRoute(loc.route, loc.project);
  // CAD-1068: the mounted app routes (`workspaceAppKey`, and the retired
  // `workspaceApp` compat route) own these; they stay on the app's own
  // route and never follow the user to /settings and the like.
  if (route.screen !== "workspaceApp" && route.screen !== "workspaceAppKey") for (const key of CRM_APP_PARAMS) q.delete(key);
  if (route.screen === "issue") {
    if (route.tab !== "overview") q.set("tab", route.tab);
    const issueSearch = q.toString();
    return routePath(route) + (issueSearch ? `?${issueSearch}` : "");
  }
  if (route.screen === "agents") {
    if (route.alias && route.tab) q.set("tab", route.tab);
    if (!route.alias && route.recruit) q.set("recruit", "1");
    if (!route.alias && route.ask) q.set("ask", "1");
  }
  if (route.screen === "projects") {
    if (route.section === "issues") {
      q.set("view", loc.view);
      writeFilters(q, loc.filters);
    }
  } else if (projectFilter(route) && loc.project !== "all") {
    q.set("project", loc.project);
  }
  if (threadsProject !== null) q.set("project", threadsProject);
  if (loc.openId) q.set("issue", loc.openId);
  if (route.screen === "myWork" && route.view) writeMyWorkView(q, route.view);
  if (route.screen === "wiki") {
    if (route.at !== undefined) q.set("at", route.at);
    if (route.section) q.set("section", route.section);
    if (route.page !== undefined) q.set("page", String(route.page));
  }
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

/** A project worth carrying onto another screen that has a filter or a slug. */
function carriedProject(current: AppLocation): string {
  if (current.route.screen === "issue") return current.route.project;
  return projectFilter(current.route) || current.route.screen === "projects" ? current.project : "all";
}

/** The location after moving to another screen. A filter or a project page
 *  keeps its project; Home, Wiki, Outbox and the rest of Settings drop it.
 *  The open issue drawer still comes along. */
export function goTo(current: AppLocation, route: Route): AppLocation {
  if (route.screen === "issue") {
    return { ...current, route, project: route.project, openId: null };
  }
  if (route.screen === "projects") {
    const slug = route.slug ?? (carriedProject(current) === "all" ? null : carriedProject(current));
    return {
      ...current,
      route: { screen: "projects", slug, section: slug ? route.section : route.section === "overview" ? "overview" : "issues" },
      project: slug ?? "all",
    };
  }
  if (projectFilter(route)) return { ...current, route, project: carriedProject(current) };
  return { ...current, route, project: "all" };
}

/**
 * Sidebar and the phone menu. Opens that project's page (keeping the
 * section when already on one). Never filters the screen you are on.
 */
export function openProject(current: AppLocation, project: string): AppLocation {
  const section = current.route.screen === "projects" ? current.route.section : "overview";
  const slug = project === "all" ? null : project;
  return {
    ...current,
    project: slug ?? "all",
    route: { screen: "projects", slug, section: slug ? section : section === "overview" ? "overview" : "issues" },
  };
}

/** The in-page filter. No-op on a screen that has none. */
export function withProject(current: AppLocation, project: string): AppLocation {
  if (current.route.screen === "issue") {
    return {
      ...current,
      project,
      openId: null,
      route: { screen: "projects", slug: project === "all" ? null : project, section: "issues" },
    };
  }
  if (!projectFilter(current.route)) return current;
  return { ...current, project: project || "all" };
}

/**
 * A legacy `?tab=` redirect, or a `?project=` dropped from a screen that
 * has no project scope. Null when the URL is already canonical.
 */
export function scopeRedirect(
  pathname: string,
  search: string,
  storedView?: ProjectView,
): string | null {
  const legacy = legacyRedirect(pathname, search, storedView);
  if (legacy) return legacy;
  if (!new URLSearchParams(search).has("project")) return null;
  const loc = readLocation(pathname, search, storedView);
  // My work's `?project=` is its own filter (CAD-1703), not a screen scope.
  if (projectFilter(loc.route) || loc.route.screen === "projects" || loc.route.screen === "myWork") return null;
  const next = locationHref(loc, search);
  const current = pathname + (search ? (search.startsWith("?") ? search : `?${search}`) : "");
  return next === current ? null : next;
}
