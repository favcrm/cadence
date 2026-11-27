import { parseIssueTab, type IssueTab } from "../features/issues/model";
import { parseAgentTab, type AgentTab } from "../features/agents/agentTabModel";
import { readFilters, writeFilters, NO_FILTERS, type BoardFilters } from "./filters";
import { readAppUrlState, type AppTab, type ProjectView } from "./urlState";
import { MY_WORK_URL_KEYS, readMyWorkView, writeMyWorkView, type MyWorkView } from "./myWorkQuery";
import type { DocumentStage } from "../features/documents/documentsClient";

/**
 * The app's routes — real paths with browser history, so every screen has
 * a link that survives a refresh or a paste:
 *
 *   /                          Home — the master's thread (CAD-328)
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
 *   /documents                 hosted only: documents by stage (CAD-2286); `?archived=1` the Archived tab, `?stage=` the Active tab's filter
 *   /setup                     first-run setup
 *   /settings[/memory|update|account|permissions|connections]  model defaults, memory, update, account, master permissions, connections
 *   /settings/members                       hosted only: the workspace's people (CAD-2115)
 *   /settings/documents                     hosted only: document type preferences (CAD-2286)
 *   /settings/connections/new[/<provider>]   the add-connection chooser and one pinned service's setup
 *   /settings/connections/account/<id>       one connection's detail page
 *   /login                     a `cadence ui login` link lands here
 *
 * Query parameters carry the rest: `project` (the in-page filter on
 * screens that have one — Settings → Memory),
 * `issue` (the open issue drawer, on any screen), `view` and the filter
 * keys (Projects only). Home, Wiki, Outbox, the Apps home and the rest
 * of Settings carry no project scope. Parameters the app does not own
 * are kept. Pure functions — the React side is lib/useLocation.ts — so
 * routing is unit-tested in plain node (tests/router.test.ts).
 */

export type ProjectSection = "overview" | "timeline" | "issues" | "plan" | "epics" | "milestones" | "context" | "workflows" | "settings";
/** `members` is hosted-only (CAD-2115): a native board serves no tenant member
 *  list, so nothing links there. `matchRoute` still parses the URL — dropping
 *  it would turn a stale link into "nothing lives here"; `settingsRedirect`
 *  moves it off a board whose `/api/meta` does not name it. */
export type SettingsSection = "models" | "memory" | "update" | "account" | "permissions" | "connections" | "email" | "templates" | "preferences" | "security" | "general" | "custody" | "members" | "documents" | "daemons";
/** The wiki's modes; `browse` opens a path by its kind (CAD-581). `trash`
 *  is the operator's deleted-landing browser (CAD-2018); its selector is a
 *  server-issued `.trash/…` value, never a logical wiki path. */
export type WikiMode = "browse" | "edit" | "history" | "search" | "upload" | "trash";

/**
 * CAD-1319: the Connections section's sub-pages. Absent = the index;
 * `add` = the service chooser; `addProvider` = one service's pinned
 * setup (the canonical provider name); `detail` = one connection's
 * page (its id). Never carries a credential — only identifiers.
 */
/**
 * CAD-2286: the Documents screen's Active-tab filter. `archived` is the
 * Archived tab, not a stage the Active tab filters by.
 */
export type DocumentStageFilter = Exclude<DocumentStage, "archived">;
const DOCUMENT_STAGE_FILTERS: readonly DocumentStageFilter[] = ["draft", "in_review", "approved", "delivered"];

export const EMAIL_CONNECTIONS_HREF = "/settings/connections/email";
export const EMAIL_SENDING_HREF = "/settings/connections/email/senders";

export type ConnectionPage =
  | { kind: "add" }
  | { kind: "addProvider"; provider: string }
  | { kind: "email"; focusSenders: boolean }
  | { kind: "emailAccount"; id: string }
  | { kind: "detail"; id: string };

export type Route =
  | { screen: "home"; privateChat?: true; actionId?: string }
  | { screen: "projects"; slug: string | null; section: ProjectSection; archived?: boolean }
  | { screen: "issue"; project: string; id: string; tab: IssueTab }
  | { screen: "apps"; project: string | null; name: string | null }
  | { screen: "appsExplore" }
  | { screen: "appsCatalog"; id: string }
  | { screen: "appsManage"; installId: string }
  | { screen: "workspaceApp"; installId: string }
  | { screen: "workspaceAppKey"; appKey: string }
  | { screen: "agents"; alias: string | null; profileId?: string; tab?: AgentTab; routineAgent?: "assistant"; recruit?: boolean; ask?: boolean }
  | { screen: "threads" }
  /** CAD-1703: the tab, search and filters, from the query (`?tab=done&search=…`). */
  | { screen: "myWork"; view?: MyWorkView }
  | { screen: "wiki"; mode: WikiMode; path: string | null; query: string | null; at?: string; section?: string; page?: number; file?: string; version?: number; archived?: boolean; protectedFiles?: boolean }
  | { screen: "outbox" }
  /** CAD-2286: hosted only. The tab and the Active tab's stage filter, from
   *  the query (`?archived=1`, `?stage=in_review`). */
  | { screen: "documents"; archived: boolean; stage?: DocumentStageFilter }
  | { screen: "setup" }
  | { screen: "settings"; section: SettingsSection; page?: ConnectionPage }
  /** CAD-1085: the trusted credential entry page for one metadata request.
   *  The id is a server-issued selector; no credential rides the URL. */
  | { screen: "credentialEntry"; requestId: string }
  /** CAD-1085: the ordinary Board page that authorizes one handoff to the
   *  pinned credential-entry origin. Selector only; no nonce in the URL. */
  | { screen: "credentialAuthorize"; requestId: string }
  | { screen: "login" }
  | { screen: "notFound"; path: string };

export type Screen = Route["screen"];

/** The main navigation — MVP screens only (Setup is reached from Home). */
export const NAV: { screen: Screen; label: string; route: Route }[] = [
  { screen: "home", label: "Home", route: { screen: "home" } },
  { screen: "myWork", label: "My work", route: { screen: "myWork" } },
  { screen: "threads", label: "Channels", route: { screen: "threads" } },
  { screen: "projects", label: "Projects", route: { screen: "projects", slug: null, section: "overview" } },
  { screen: "wiki", label: "Wiki", route: { screen: "wiki", mode: "browse", path: null, query: null } },
  // CAD-2286: hosted only; App hides it unless the capability probe answers available.
  { screen: "documents", label: "Documents", route: { screen: "documents", archived: false } },
  { screen: "apps", label: "Apps", route: { screen: "apps", project: null, name: null } },
  { screen: "agents", label: "Agents", route: { screen: "agents", alias: null } },
  // CAD-1312: Outbox leaves the nav (its route, API and page remain).
  { screen: "settings", label: "Settings", route: { screen: "settings", section: "models" } },
];

/**
 * The nav a board carries. A board that does not serve the wiki (a hosted
 * tenant without `wiki: true`; the shared UI's requests answer 501) does not
 * advertise it; a local board keeps it. The route still explains itself if
 * reached by a deep link.
 */
export function navItems(noWiki: boolean, hiddenScreens?: readonly Screen[]): typeof NAV {
  const hidden = new Set<Screen>(hiddenScreens);
  if (noWiki) hidden.add("wiki");
  return hidden.size === 0 ? NAV : NAV.filter((item) => !hidden.has(item.screen));
}

/**
 * Where a project lives on a screen. The in-page chip row and the memory
 * picker read this. The sidebar and the phone menu do not: their highlight
 * is `route.screen === "projects"` (`navProject` in App.tsx).
 *
 * - `slug` — the project page (`/projects/:slug`). A sidebar project opens it.
 * - `picker` — an in-page project picker, stored as `?project=`
 *   (Settings → Memory).
 * - `none` — no project scope in the URL or in state.
 */
export type ProjectScope = "slug" | "picker" | "none";

export function projectScope(route: Route): ProjectScope {
  switch (route.screen) {
    case "projects":
      return "slug";
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
  return scope === "picker";
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
 * The wiki's paths (CAD-581). `edit`, `history`, `upload`, `search` and
 * `trash` are reserved first segments — the modes' homes; anything else is
 * a path into the store, which `browse` opens by its kind. No fixed wiki
 * root is named `trash`, so the reservation cannot shadow a real folder.
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
  // CAD-2018: the Trash screen is one operator screen. A landing's identity
  // is the board's row, not a URL: `from` may be null, so a selector in the
  // path would be a second, weaker identity.
  if (mode === "trash") {
    return { screen: "wiki", mode: "trash", path: null, query: null };
  }
  return { screen: "wiki", mode: "browse", path: pathFrom(parts, 1) || null, query: null };
}

const REQUEST_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function matchRoute(pathname: string): Route {
  const parts = pathname.split("/").filter(Boolean);
  const [head, a, b, ...rest] = parts;
  if (head === "wiki") return matchWiki(parts);
  // CAD-1085: the trusted credential-entry page. Only a canonical lowercase
  // request UUID is a route; the page itself proves the session.
  if (head === "connections" && a === "enroll" && b && rest.length === 0 && REQUEST_ID.test(b)) {
    return { screen: "credentialEntry", requestId: b };
  }
  if (head === "connections" && a === "authorize" && b && rest.length === 0 && REQUEST_ID.test(b)) {
    return { screen: "credentialAuthorize", requestId: b };
  }
  if (head === "projects" && b === "issues" && rest.length === 1) {
    const slug = a ? segment(a) : null;
    const issueId = segment(rest[0]);
    if (slug && issueId) return { screen: "issue", project: slug, id: issueId, tab: "overview" };
  }
  if (rest.length === 0 || (head === "settings" && a === "connections" && rest.length <= 2)) {
    if (!head && !a && rest.length === 0) return { screen: "home" };
    if (head === "index.html" && !a && rest.length === 0) return { screen: "home" };
    if (head === "projects") {
      if (!a) return { screen: "projects", slug: null, section: "overview" };
      const slug = segment(a);
      if (slug && !b) return { screen: "projects", slug, section: "overview" };
      if (slug && (b === "overview" || b === "timeline" || b === "issues" || b === "plan" || b === "context" || b === "epics" || b === "milestones" || b === "workflows" || b === "settings")) {
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
    if (head === "agents" && a === "profile" && b && !rest.length) {
      const profileId = segment(b);
      if (profileId && /^ap_[A-Za-z0-9_-]+$/.test(profileId)) return { screen: "agents", alias: null, profileId };
    }
    if ((head === "threads" || head === "channels") && !a) return { screen: "threads" };
    if (head === "setup" && !a) return { screen: "setup" };
    if (head === "login" && !a) return { screen: "login" };
    // `?item=<effect_id>` deep-links one published item (CAD-546).
    if (head === "outbox" && !a) return { screen: "outbox" };
    if (head === "my-work" && !a) return { screen: "myWork" };
    if (head === "documents" && !a) return { screen: "documents", archived: false };
    // `!b` keeps the original one-segment shape; the connections
    // section alone may carry its /new, /new/<provider> and
    // /account/<id> sub-pages (bounded to one extra segment above).
    if (head === "settings" && (!b || a === "connections")) {
      if (!a) return { screen: "settings", section: "models" };
      if (a === "memory") return { screen: "settings", section: "memory" };
      if (a === "update") return { screen: "settings", section: "update" };
      if (a === "account") return { screen: "settings", section: "account" };
      if (a === "preferences") return { screen: "settings", section: "preferences" };
      if (a === "security") return { screen: "settings", section: "security" };
      if (a === "permissions") return { screen: "settings", section: "permissions" };
      if (a === "email") return { screen: "settings", section: "connections", page: { kind: "email", focusSenders: true } };
      if (a === "general") return { screen: "settings", section: "general" };
      if (a === "members") return { screen: "settings", section: "members" };
      if (a === "documents") return { screen: "settings", section: "documents" };
      if (a === "daemons") return { screen: "settings", section: "daemons" };
      if (a === "custody") return { screen: "settings", section: "custody" };
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
          if (b === "email" && rest.length === 2 && rest[0] === "account") {
            const id = segment(rest[1]);
            if (id) return { screen: "settings", section: "connections", page: { kind: "emailAccount", id } };
          }
          if (b === "email" && (rest.length === 0 || (rest.length === 1 && rest[0] === "senders"))) {
            return { screen: "settings", section: "connections", page: { kind: "email", focusSenders: rest.length === 1 } };
          }
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
      return route.profileId
        ? `/agents/profile/${encodeURIComponent(route.profileId)}`
        : route.alias ? `/agents/${encodeURIComponent(route.alias)}` : "/agents";
    case "threads":
      return "/threads";
    case "wiki": {
      const path = route.path ? route.path.split("/").map(encodeURIComponent).join("/") : "";
      if (route.mode === "search") {
        return route.query ? `/wiki/search/${encodeURIComponent(route.query)}` : "/wiki/search";
      }
      // CAD-2018: one Trash screen; no landing selector rides the URL.
      if (route.mode === "trash") return "/wiki/trash";
      if (route.mode === "browse") return path ? `/wiki/${path}` : "/wiki";
      return path ? `/wiki/${route.mode}/${path}` : `/wiki/${route.mode}`;
    }
    case "setup":
      return "/setup";
    case "credentialEntry":
      return `/connections/enroll/${encodeURIComponent(route.requestId)}`;
    case "credentialAuthorize":
      return `/connections/authorize/${encodeURIComponent(route.requestId)}`;
    case "login":
      return "/login";
    case "outbox":
      return "/outbox";
    case "myWork":
      return "/my-work";
    case "documents":
      return "/documents";
    case "settings":
      if (route.section === "account") return "/settings/account";
      if (route.section === "preferences") return "/settings/preferences";
      if (route.section === "security") return "/settings/security";
      if (route.section === "memory") return "/settings/memory";
      if (route.section === "update") return "/settings/update";
      if (route.section === "permissions") return "/settings/permissions";
      if (route.section === "connections") {
        if (route.page?.kind === "email") return route.page.focusSenders ? EMAIL_SENDING_HREF : EMAIL_CONNECTIONS_HREF;
        if (route.page?.kind === "emailAccount") return `/settings/connections/email/account/${encodeURIComponent(route.page.id)}`;
        if (route.page?.kind === "add") return "/settings/connections/new";
        if (route.page?.kind === "addProvider") {
          return `/settings/connections/new/${encodeURIComponent(route.page.provider)}`;
        }
        if (route.page?.kind === "detail") {
          return `/settings/connections/account/${encodeURIComponent(route.page.id)}`;
        }
        return "/settings/connections";
      }
      if (route.section === "email") return EMAIL_CONNECTIONS_HREF;
      if (route.section === "general") return "/settings/general";
      if (route.section === "members") return "/settings/members";
      if (route.section === "documents") return "/settings/documents";
      if (route.section === "daemons") return "/settings/daemons";
      if (route.section === "custody") return "/settings/custody";
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
    const file = q.get("file");
    const rawVersion = q.get("version");
    const version = rawVersion && /^[1-9][0-9]*$/.test(rawVersion) ? Number(rawVersion) : undefined;
    const archived = q.get("archived") === "1";
    const protectedFiles = q.get("wiki_scope") === "protected";
    if (at !== null || section || parsedPage !== undefined || file !== null || archived || protectedFiles) {
      route = {
        ...route,
        ...(at !== null ? { at } : {}),
        ...(section ? { section } : {}),
        ...(parsedPage !== undefined ? { page: parsedPage } : {}),
        ...(file !== null ? { file, ...(version && Number.isSafeInteger(version) ? { version } : {}) } : {}),
        ...(archived ? { archived: true } : {}),
        ...(protectedFiles ? { protectedFiles: true } : {}),
      };
    }
  }
  if (route.screen === "projects" && route.section === "overview" && q.has("view")) {
    route = { ...route, section: "issues" };
  }
  if (route.screen === "projects" && route.slug === null && route.section === "overview" && q.get("archived") === "1") {
    route = { ...route, archived: true };
  }
  if (route.screen === "home") {
    const privateChat = q.get("chat") === "assistant";
    const rawAction = q.get("action")?.trim();
    const actionId = rawAction ? rawAction : undefined;
    if (privateChat || actionId) {
      route = {
        ...route,
        ...(privateChat ? { privateChat: true } : {}),
        ...(actionId ? { actionId } : {}),
      };
    }
  }
  if (route.screen === "agents") {
    const tab = parseAgentTab(q.get("tab"));
    const detail = route.alias !== null || route.profileId !== undefined;
    route = {
      ...route,
      ...(detail && tab !== "overview" ? { tab } : {}),
      ...(route.alias === "master" && tab === "routines" && q.get("routineAgent") === "assistant" ? { routineAgent: "assistant" } : {}),
      ...(!detail && q.get("recruit") === "1" ? { recruit: true } : {}),
      ...(!detail && q.get("ask") === "1" ? { ask: true } : {}),
    };
  }
  if (route.screen === "myWork") {
    const view = readMyWorkView(q);
    route = Object.keys(view).length ? { screen: "myWork", view } : { screen: "myWork" };
  }
  if (route.screen === "documents") {
    // CAD-2286: unknown or malformed values fall back to the default, as My
    // work's do. The Archived tab has no stage filter.
    const archived = q.get("archived") === "1";
    const stage = DOCUMENT_STAGE_FILTERS.find((value) => value === q.get("stage"));
    route = { screen: "documents", archived, ...(!archived && stage ? { stage } : {}) };
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
  const section = slug ? route.section : route.section === "overview" ? "overview" : "issues";
  return {
    screen: "projects",
    slug,
    section,
    ...(slug === null && section === "overview" && route.archived ? { archived: true } : {}),
  };
}

/** Query parameters only a mounted app route owns. `install` is the
 *  CAD-1438 pin of a generated contextual link — the immutable install id
 *  the ctx/conversation/record selectors were minted for. `drawer` and
 *  `drawer_size` are the host's return hint for the drawer surface an app
 *  asked to open (a connect that leaves the board and comes back); they are
 *  the app page's alone and never follow the operator elsewhere. */
const APP_ROUTE_PARAMS = ["crm", "record", "ctx", "appview", "segment", "conversation", "install", "drawer", "drawer_size", "wa_thread"] as const;

/** CAD-2555: the page's declared-view params (`p.<name>`) are app-owned too. */
function deleteAppRouteParams(q: URLSearchParams): void {
  for (const key of [...q.keys()]) if (key.startsWith("p.")) q.delete(key);
  for (const key of APP_ROUTE_PARAMS) q.delete(key);
}

/** Query keys owned by host screens so an app key matching one is never stripped as app state. */
const HOST_QUERY_KEYS = new Set<string>([
  "tab", "view", "project", "issue", "run", "new", "screen", "at", "section", "page",
  "recruit", "ask", "h", "dm", "chat", "action", "file", "mode", "q",
  "c", "kind", "agent", "system",
  "item", "outbox_q", "outbox_project",
  ...MY_WORK_URL_KEYS,
]);

/** Global cross-route host query keys preserved even if explicitly passed as a section query. */
const GLOBAL_HOST_QUERY_KEYS = new Set<string>(["project", "issue"]);

export type SectionQueryInput = string | readonly (string | null | undefined)[] | null | undefined;

function deleteSectionQueries(q: URLSearchParams, queries: SectionQueryInput): void {
  if (!queries) return;
  const list = typeof queries === "string" ? [queries] : queries;
  for (const key of list) {
    if (key && !GLOBAL_HOST_QUERY_KEYS.has(key)) q.delete(key);
  }
}

export function isAppRoute(route: Route): boolean {
  return route.screen === "workspaceApp" || route.screen === "workspaceAppKey";
}

export function sameAppRoute(a: Route | null, b: Route): boolean {
  if (a === null) return false;
  if (a.screen === "workspaceApp" && b.screen === "workspaceApp") return a.installId === b.installId;
  if (a.screen === "workspaceAppKey" && b.screen === "workspaceAppKey") return a.appKey === b.appKey;
  return false;
}

/** Strip parameters owned by a mounted app when leaving it or switching to another app. */
export function appNavigationSearch(
  from: Route,
  to: Route,
  search: string,
  activeSectionQuery?: SectionQueryInput,
): string {
  if (!isAppRoute(from) || sameAppRoute(from, to)) return search;
  const q = new URLSearchParams(search);
  deleteAppRouteParams(q);
  if (from.screen === "workspaceAppKey" && !HOST_QUERY_KEYS.has(from.appKey)) {
    q.delete(from.appKey);
  }
  deleteSectionQueries(q, activeSectionQuery);
  return q.toString();
}

/** Strip app-owned query parameters unconditionally before entering another app. */
export function stripAppRouteParams(search: string, extraSectionQuery?: SectionQueryInput): string {
  const q = new URLSearchParams(search);
  deleteAppRouteParams(q);
  deleteSectionQueries(q, extraSectionQuery);
  return q.toString();
}

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
  for (const key of ["tab", "view", "project", "issue", "run", "new", "screen", "at", "section", "page", "recruit", "ask", "action", "file", "version", "routineAgent"]) q.delete(key);
  // CAD-1703: My work's view belongs to its own route.
  for (const key of MY_WORK_URL_KEYS) q.delete(key);
  writeFilters(q, NO_FILTERS);
  const route = scopedRoute(loc.route, loc.project);
  if (!isAppRoute(route) || from?.screen === "home") q.delete("chat");
  // CAD-2286: Documents owns `archived` and `stage` and drops them elsewhere,
  // except on a mounted app route, whose own query may use either name. Leaving
  // Documents for an app route drops them too: there they are still Documents'.
  if (!isAppRoute(route) || from?.screen === "documents" || from?.screen === "projects") {
    q.delete("archived");
    if (!isAppRoute(route) || from?.screen === "documents") q.delete("stage");
  }
  if (!isAppRoute(route) || from?.screen === "wiki") q.delete("wiki_scope");
  // CAD-1068: the mounted app routes (`workspaceAppKey`, and the retired
  // `workspaceApp` compat route) own these; they stay on the app's own
  // route and never follow the user to /settings, /threads, or another app.
  const leavingApp = !isAppRoute(route) || (from !== null && isAppRoute(from) && !sameAppRoute(from, route));
  if (leavingApp) {
    deleteAppRouteParams(q);
    if (from?.screen === "workspaceAppKey" && !HOST_QUERY_KEYS.has(from.appKey)) {
      q.delete(from.appKey);
    }
  }
  if (route.screen === "issue") {
    if (route.tab !== "overview") q.set("tab", route.tab);
    const issueSearch = q.toString();
    return routePath(route) + (issueSearch ? `?${issueSearch}` : "");
  }
  if (route.screen === "home") {
    if (route.privateChat) q.set("chat", "assistant");
    if (route.actionId) q.set("action", route.actionId);
  }
  if (route.screen === "agents") {
    const detail = route.alias !== null || route.profileId !== undefined;
    if (detail && route.tab) q.set("tab", route.tab);
    if (route.alias === "master" && route.tab === "routines" && route.routineAgent) q.set("routineAgent", route.routineAgent);
    if (!detail && route.recruit) q.set("recruit", "1");
    if (!detail && route.ask) q.set("ask", "1");
  }
  if (route.screen === "projects") {
    if (route.section === "issues") {
      q.set("view", loc.view);
      writeFilters(q, loc.filters);
    } else if (route.slug === null && route.section === "overview" && route.archived) {
      q.set("archived", "1");
    }
  } else if (projectFilter(route) && loc.project !== "all") {
    q.set("project", loc.project);
  }
  if (threadsProject !== null) q.set("project", threadsProject);
  if (loc.openId) q.set("issue", loc.openId);
  if (route.screen === "myWork" && route.view) writeMyWorkView(q, route.view);
  // CAD-2286: Documents owns `archived` and `stage`; the defaults are a bare URL.
  if (route.screen === "documents") {
    if (route.archived) q.set("archived", "1");
    else if (route.stage) q.set("stage", route.stage);
  }
  if (route.screen === "wiki") {
    if (route.at !== undefined) q.set("at", route.at);
    if (route.section) q.set("section", route.section);
    if (route.page !== undefined) q.set("page", String(route.page));
    if (route.file && route.version) {
      q.set("file", route.file);
      q.set("version", String(route.version));
    }
    if (route.archived) q.set("archived", "1");
    if (route.protectedFiles) q.set("wiki_scope", "protected");
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
  if (pathname === "/settings/email" || pathname === "/settings/email/") {
    const loc = readLocation(pathname, search, storedView);
    return locationHref(loc, search);
  }
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
    const section = slug ? route.section : route.section === "overview" ? "overview" : "issues";
    return {
      ...current,
      route: {
        screen: "projects",
        slug,
        section,
        ...(slug === null && section === "overview" && route.archived ? { archived: true } : {}),
      },
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
