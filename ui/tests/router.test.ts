import { NO_FILTERS } from "../src/lib/filters";
import { readThreadsUrl } from "../src/features/threads/threadsView";
import {
  appNavigationSearch,
  goTo,
  legacyRedirect,
  locationHref,
  matchRoute,
  openProject,
  projectScope,
  readLocation,
  routePath,
  scopeRedirect,
  showProjectChoices,
  stripAppRouteParams,
  withProject,
  type Route,
} from "../src/lib/router";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

{
  const search = "?chat=assistant";
  const home = readLocation("/", search);
  equal(locationHref({ ...home, openId: "CAD-1" }, search, home.route), "/?chat=assistant&issue=CAD-1", "a Home detail preserves the private chat mode");
  const agents = { screen: "agents", alias: null } as const;
  equal(locationHref(goTo(home, agents), search, home.route), "/agents", "the private Home selector doesn't leave Home");
  equal(locationHref(goTo(home, { screen: "home" }), search, home.route), "/", "normal Home navigation restores the default pane");
  const query = "?tab=routines&routineAgent=assistant";
  const review = readLocation("/agents/master", query);
  equal(locationHref(review, query), "/agents/master?tab=routines&routineAgent=assistant", "the owner routine filter round-trips");
  equal(locationHref(readLocation("/agents/assistant", query), query), "/agents/assistant?tab=routines", "the filter cannot override the personal assistant route");
}

// Every MVP route matches and prints back to the same path.
const paths: [string, Route][] = [
  ["/", { screen: "home" }],
  ["/my-work", { screen: "myWork" }],
  ["/projects", { screen: "projects", slug: null, section: "overview" }],
  ["/projects/cadence", { screen: "projects", slug: "cadence", section: "overview" }],
  ["/projects/cadence/issues", { screen: "projects", slug: "cadence", section: "issues" }],
  ["/projects/cadence/context", { screen: "projects", slug: "cadence", section: "context" }],
  ["/projects/cadence/epics", { screen: "projects", slug: "cadence", section: "epics" }],
  ["/projects/cadence/milestones", { screen: "projects", slug: "cadence", section: "milestones" }],
  ["/projects/cadence/workflows", { screen: "projects", slug: "cadence", section: "workflows" }],
  ["/projects/cadence/settings", { screen: "projects", slug: "cadence", section: "settings" }],
  ["/projects/cadence/issues/CAD-607", { screen: "issue", project: "cadence", id: "CAD-607", tab: "overview" }],
  ["/apps", { screen: "apps", project: null, name: null }],
  ["/apps/social-content", { screen: "workspaceAppKey", appKey: "social-content" }],
  ["/apps/crm", { screen: "workspaceAppKey", appKey: "crm" }],
  ["/app-installations/install-social", { screen: "workspaceApp", installId: "install-social" }],
  ["/apps/cadence/studio", { screen: "apps", project: "cadence", name: "studio" }],
  ["/agents", { screen: "agents", alias: null }],
  ["/agents/cc-1", { screen: "agents", alias: "cc-1" }],
  ["/threads", { screen: "threads" }],
  ["/setup", { screen: "setup" }],
  ["/overview", { screen: "overview" }],
  ["/settings", { screen: "settings", section: "models" }],
  ["/settings/memory", { screen: "settings", section: "memory" }],
  ["/settings/permissions", { screen: "settings", section: "permissions" }],
  ["/settings/connections", { screen: "settings", section: "connections" }],
  ["/settings/connections/new", { screen: "settings", section: "connections", page: { kind: "add" } }],
  ["/settings/connections/new/smtp", { screen: "settings", section: "connections", page: { kind: "addProvider", provider: "smtp" } }],
  ["/settings/connections/account/c-1", { screen: "settings", section: "connections", page: { kind: "detail", id: "c-1" } }],
  ["/settings/email", { screen: "settings", section: "email" }],
  ["/outbox", { screen: "outbox" }],
  ["/login", { screen: "login" }],
];
for (const [path, route] of paths) {
  equal(matchRoute(path), route, `match ${path}`);
  equal(routePath(route), path, `print ${path}`);
}
equal(matchRoute("/projects/cadence/"), matchRoute("/projects/cadence"), "trailing slash");
equal(matchRoute("/agents/a%20b"), { screen: "agents", alias: "a b" }, "decoded alias");
equal(routePath({ screen: "agents", alias: "a b" }), "/agents/a%20b", "encoded alias");
equal(matchRoute("/index.html"), { screen: "home" }, "index.html is home");
equal(projectScope({ screen: "workspaceApp", installId: "install-social" }), "none", "workspace app has no project scope");
equal(projectScope({ screen: "workspaceAppKey", appKey: "crm" }), "none", "friendly app route has no project scope");
for (const path of ["/app-installations", "/app-installations/..", "/app-installations/a%2Fb", "/app-installations/a/extra"]) {
  equal(matchRoute(path).screen, "notFound", `invalid installation route ${path}`);
}
for (const dead of ["/overview/x", "/nope", "/projects/x/y", "/projects/cadence/secrets", "/projects/cadence/issues/CAD-1/extra", "/agents/a/b", "/apps/%2F", "/apps/%E0", "/apps/catalog", "/apps/manage", "/apps/p/n/x", "/settings/nope", "/setup/x", "/login/x", "/projects/%E0", "/settings/connections/new/smtp/extra", "/settings/connections/account", "/settings/connections/bogus", "/settings/connections/new/%E0", "/settings/account/unwanted", "/settings/memory/extra", "/settings/update/extra", "/settings/permissions/extra", "/settings/email/extra", "/settings/connections/account/x/y", "/settings/connections/new//smtp", "/settings/connections/account//c-1", "/settings/connections//new"]) {
  equal(matchRoute(dead).screen, "notFound", `not found ${dead}`);
}
equal(matchRoute("/my-work/extra").screen, "notFound", "My work has no nested routes");

// CAD-1703: My work's tab, search and filters live in its URL and round-trip;
// they never follow a navigation to another screen.
{
  const search = "?tab=done&search=smtp+relay&project=agenticos-v2&priority=P1&blocked=1&sort=priority";
  const loc = readLocation("/my-work", search);
  equal(loc.route, {
    screen: "myWork",
    view: { tab: "done", search: "smtp relay", project: "agenticos-v2", priority: "P1", blocked: true, sort: "priority" },
  }, "My work reads its view");
  equal(locationHref(loc, search), "/my-work?tab=done&search=smtp+relay&project=agenticos-v2&priority=P1&blocked=1&sort=priority", "My work view round-trips");
  equal(scopeRedirect("/my-work", search), null, "My work keeps its project filter");
  equal(readLocation("/my-work", "?tab=bogus&priority=P9&project=..%2Fx&sort=owner").route, { screen: "myWork" }, "malformed view values fall back to defaults");
  equal(locationHref(goTo(loc, { screen: "threads" }), search), "/threads", "My work view stays on My work");
  equal(locationHref({ ...loc, route: { screen: "myWork", view: {} } }, search), "/my-work", "the default view has a bare URL");
}


// CAD-1811: the shared issue detail opens over Threads through `?issue=`.
// Threads owns its list scope in `?project=`, so opening and closing the
// detail must keep the conversation, agent, kind, system flag and project.
{
  const search = "?c=conv-1&project=cadence&agent=cc-1&kind=agents&system=1";
  const loc = readLocation("/threads", search);
  const params = (href: string) => {
    const q = new URLSearchParams(href.slice(href.indexOf("?") + 1));
    return [...q].sort(([a], [b]) => a.localeCompare(b));
  };
  const openedHref = locationHref({ ...loc, openId: "CAD-77" }, search, { screen: "threads" });
  equal(params(openedHref), params("/threads?c=conv-1&project=cadence&agent=cc-1&kind=agents&system=1&issue=CAD-77"), "the detail URL keeps Threads scope");
  const openedSearch = openedHref.slice(openedHref.indexOf("?"));
  equal(readThreadsUrl(openedSearch), { conversation: "conv-1", kind: "agents", project: "cadence", agent: "cc-1", includeSystem: true }, "Threads reads unchanged list scope with the detail open");
  const reopened = readLocation("/threads", openedSearch);
  equal(params(locationHref({ ...reopened, openId: null }, openedSearch, { screen: "threads" })), params("/threads?c=conv-1&project=cadence&agent=cc-1&kind=agents&system=1"), "closing the detail keeps Threads scope");
}

// Which screens keep a project. One table.
equal(projectScope({ screen: "projects", slug: "cadence", section: "issues" }), "slug", "projects are the slug");
equal(projectScope({ screen: "agents", alias: null }), "none", "agents (CAD-1772: the team, not a project)");
equal(projectScope({ screen: "overview" }), "chips", "overview");
equal(projectScope({ screen: "apps", project: null, name: null }), "none", "apps list is a workspace home");
equal(projectScope({ screen: "apps", project: "cadence", name: "studio" }), "none", "app detail is not a filter");
equal(projectScope({ screen: "settings", section: "memory" }), "picker", "memory");
equal(projectScope({ screen: "settings", section: "models" }), "none", "models");
equal(projectScope({ screen: "settings", section: "update" }), "none", "update");
equal(projectScope({ screen: "home" }), "none", "home");
equal(projectScope({ screen: "wiki", mode: "browse", path: null, query: null }), "none", "wiki");
equal(projectScope({ screen: "outbox" }), "none", "outbox");
equal(showProjectChoices(2, "all"), true, "two projects show the row");
equal(showProjectChoices(1, "all"), false, "one project hides the row");
equal(showProjectChoices(0, "all"), false, "no projects hides the row");
equal(showProjectChoices(1, "cadence"), true, "a selected project stays visible");

// Reading a location: the slug is the scope on Projects. ?project= is the
// in-page filter on Agents, Team overview and Settings → Memory. Elsewhere
// it is ignored.
{
  const loc = readLocation("/projects/cadence", "?view=kanban&tag=ui&issue=CAD-1", "list");
  equal(loc.project, "cadence", "slug scope");
  equal(loc.view, "kanban", "explicit view");
  equal(loc.filters.tags, ["ui"], "filters on projects");
  equal(loc.openId, "CAD-1", "drawer");
  const agents = readLocation("/agents", "?project=cadence&tag=ui");
  equal(agents.project, "all", "agents carry no project filter");
  equal(agents.filters, NO_FILTERS, "filters only on projects");
  equal(readLocation("/projects", "", "kanban").view, "kanban", "stored view");
  equal(readLocation("/", "").project, "all", "no scope");
  equal(readLocation("/", "?project=cadence").project, "all", "home ignores project");
  equal(readLocation("/wiki", "?project=cadence").project, "all", "wiki ignores project");
  equal(readLocation("/outbox", "?project=cadence").project, "all", "outbox ignores project");
  equal(readLocation("/settings", "?project=cadence").project, "all", "settings ignores project");
  equal(readLocation("/settings/update", "?project=cadence").project, "all", "update ignores project");
  equal(readLocation("/overview", "?project=cadence").project, "cadence", "overview filter");
  equal(readLocation("/settings/memory", "?project=cadence").project, "cadence", "memory filter");
  // The app's project lives on the route. It is not the board's scope.
  const detail = readLocation("/apps/cadence/studio", "?project=other");
  equal(detail.project, "all", "app detail ignores query project");
  equal(detail.route, { screen: "apps", project: "cadence", name: "studio" }, "app detail route");
  equal(readLocation("/apps", "?project=cadence").project, "all", "apps ignores query project");
}

// An app detail href does not repeat its path's project in the query.
{
  const loc = readLocation("/apps/cadence/studio", "");
  equal(locationHref(loc), "/apps/cadence/studio", "app detail href");
  const list = readLocation("/apps", "?project=cadence");
  equal(locationHref(list), "/apps", "apps href drops the query project");
}

// CAD-1026: an app page's native-controls choice never follows to another installation.
{
  const friendly = readLocation("/apps/crm", "?crm=segments&ctx=scope-a");
  equal(locationHref(friendly, "?crm=segments&ctx=scope-a"), "/apps/crm?crm=segments&ctx=scope-a", "friendly app href keeps app query context");
  const here = readLocation("/app-installations/install-a", "?screen=native");
  equal(
    locationHref(goTo(here, { screen: "workspaceApp", installId: "install-b" }), "?screen=native&foo=1"),
    "/app-installations/install-b?foo=1",
    "switching installation drops screen=native",
  );
}

// Printing a location keeps unknown params and drops stale app ones.
{
  const href = locationHref(
    {
      route: { screen: "projects", slug: null, section: "issues" },
      project: "cadence",
      view: "kanban",
      openId: null,
      filters: NO_FILTERS,
    },
    "?unknown=keep&tag=stale&view=list&project=old",
  );
  equal(href, "/projects/cadence/issues?unknown=keep&view=kanban", "projects href");
  const home = locationHref(
    { route: { screen: "home" }, project: "cadence", view: "list", openId: "CAD-9", filters: NO_FILTERS },
    "?view=list&tab=board",
  );
  equal(home, "/?issue=CAD-9", "home keeps the drawer and drops project scope");
  const context = locationHref({
    route: { screen: "projects", slug: null, section: "context" },
    project: "all",
    view: "list",
    openId: null,
    filters: NO_FILTERS,
  });
  equal(context, "/projects?view=list", "context without a project falls back to issues");
}

// An issue page is /projects/<key>/issues/<id>, with the section in ?tab=.
{
  const loc = readLocation("/projects/cadence/issues/CAD-1", "?tab=activity&issue=CAD-9&view=list");
  equal(loc.route, { screen: "issue", project: "cadence", id: "CAD-1", tab: "activity" }, "issue tab");
  equal(loc.project, "cadence", "issue project");
  equal(loc.openId, null, "the page does not also open the peek");
  equal(locationHref(loc, "?tab=activity&issue=CAD-9&foo=1"), "/projects/cadence/issues/CAD-1?foo=1&tab=activity", "issue href");
  equal(readLocation("/projects/cadence/issues/CAD-1", "?tab=nope").route, { screen: "issue", project: "cadence", id: "CAD-1", tab: "overview" }, "unknown tab");
  const page = readLocation("/projects/cadence/issues/CAD-1", "");
  equal(locationHref(withProject(page, "other")), "/projects/other/issues?view=list", "leaving an issue for another project's issue list");
  equal(
    locationHref(goTo(page, { screen: "agents", alias: null })),
    "/agents",
    "issue → agents carries no project and drops the peek",
  );
}

// Moving between screens carries the scope and the open drawer.
{
  const here = readLocation("/projects/cadence", "?view=list&issue=CAD-3");
  equal(
    locationHref(goTo(here, { screen: "agents", alias: null })),
    "/agents?issue=CAD-3",
    "projects → agents carries no project",
  );
  const agents = readLocation("/overview", "?project=cadence");
  equal(
    locationHref(goTo(agents, { screen: "projects", slug: null, section: "issues" })),
    "/projects/cadence/issues?view=list",
    "overview → projects keeps the scope as the slug",
  );
  equal(locationHref(openProject(here, "all")), "/projects?view=list&issue=CAD-3", "sidebar all projects opens the board");
  equal(
    locationHref(openProject(readLocation("/overview", "?project=kidult"), "cadence")),
    "/projects/cadence",
    "sidebar on overview opens the project overview",
  );
  equal(
    locationHref(openProject(readLocation("/projects/cadence/epics", ""), "kidult")),
    "/projects/kidult/epics",
    "sidebar keeps the project section",
  );
  equal(
    locationHref(withProject(readLocation("/overview", ""), "cadence")),
    "/overview?project=cadence",
    "an overview chip sets the filter",
  );
  equal(
    locationHref(withProject(readLocation("/", "?issue=CAD-1"), "cadence")),
    "/?issue=CAD-1",
    "home cannot take a project filter",
  );
  equal(
    locationHref(withProject(readLocation("/settings/memory", ""), "cadence")),
    "/settings/memory?project=cadence",
    "memory picker sets the filter",
  );
  equal(
    locationHref(goTo(here, { screen: "home" })),
    "/?issue=CAD-3",
    "projects → home drops the project",
  );
  equal(
    locationHref(goTo(readLocation("/agents", "?project=cadence&issue=CAD-3"), { screen: "wiki", mode: "browse", path: null, query: null })),
    "/wiki?issue=CAD-3",
    "agents → wiki drops the project",
  );
  equal(
    locationHref(goTo(readLocation("/", "?project=cadence"), { screen: "agents", alias: null })),
    "/agents",
    "home → agents carries no project",
  );
}

// Pre-router URLs redirect to their new home, keeping scope, drawer and filters.
const legacy: [string, string][] = [
  ["?tab=overview&project=cadence", "/"],
  ["?tab=board&view=kanban&project=cadence&tag=ui", "/projects/cadence/issues?view=kanban&tag=ui"],
  ["?project=cadence&view=list", "/projects/cadence/issues?view=list"],
  ["?view=list", "/projects?view=list"],
  ["?tab=plan&project=cadence", "/projects/cadence/context"],
  ["?tab=plan", "/projects?view=list"],
  ["?tab=agents&project=cadence&issue=CAD-7", "/agents?issue=CAD-7"],
  ["?tab=memory&project=cadence", "/settings/memory?project=cadence"],
  ["?tab=settings", "/settings"],
  ["?tab=loops&x=1", "/?x=1"],
];
for (const [search, target] of legacy) {
  const next = legacyRedirect("/", search);
  equal(next, target, `redirect ${search}`);
  // A redirect target is itself final — no loops.
  const [path, query = ""] = next!.split("?");
  equal(legacyRedirect(path, query ? `?${query}` : ""), null, `final ${target}`);
  equal(matchRoute(path).screen === "notFound", false, `live ${target}`);
}
equal(legacyRedirect("/", "?tab=board", "kanban"), "/projects?view=kanban", "stored view on a bare board link");
equal(legacyRedirect("/", "?project=cadence"), null, "a project-only link is not a legacy tab");
equal(scopeRedirect("/", "?project=cadence"), "/", "home drops ?project=");
equal(scopeRedirect("/wiki", "?project=cadence"), "/wiki", "wiki drops ?project=");
equal(scopeRedirect("/outbox", "?project=cadence&item=ef-1"), "/outbox?item=ef-1", "outbox keeps its own params");
equal(scopeRedirect("/settings", "?project=cadence"), "/settings", "settings drops ?project=");
equal(scopeRedirect("/settings/update", "?project=cadence"), "/settings/update", "update drops ?project=");
equal(scopeRedirect("/apps/cadence/studio", "?project=other"), "/apps/cadence/studio", "app detail drops query project");
equal(scopeRedirect("/agents", "?project=cadence"), "/agents", "agents drops ?project= (the team, not a project)");
equal(scopeRedirect("/overview", "?project=cadence"), null, "overview keeps the filter");
equal(scopeRedirect("/apps", "?project=cadence"), "/apps", "apps drops the query project");
equal(scopeRedirect("/overview", "?project=cadence"), null, "overview keeps the filter");
equal(scopeRedirect("/settings/memory", "?project=cadence"), null, "memory keeps the filter");
equal(legacyRedirect("/projects/cadence", "?view=list"), null, "a routed URL is not legacy");
equal(legacyRedirect("/index.html", "?tab=agents"), "/agents", "index.html with a tab");
equal(legacyRedirect("/index.html", ""), "/", "bare index.html");

console.log("router checks passed");

// A plain Projects entry is overview; existing board bookmarks remain Issues.
equal(readLocation("/projects", "").route, { screen: "projects", slug: null, section: "overview" }, "portfolio landing");
equal(readLocation("/projects/cadence", "?view=list").route, { screen: "projects", slug: "cadence", section: "issues" }, "legacy issue bookmark");

// CAD-1068: CRM app params stay on the app route and never follow to /settings.
{
  const crm = "?crm=campaigns&record=r1&ctx=c1&appview=new&segment=s1&keep=1";
  const app = readLocation("/app-installations/install-a", crm);
  equal(locationHref(app, crm), "/app-installations/install-a?crm=campaigns&record=r1&ctx=c1&appview=new&segment=s1&keep=1", "app route keeps its own params");
  const settings = goTo(app, { screen: "settings", section: "memory" });
  equal(locationHref(settings, crm), "/settings/memory?keep=1", "settings drops CRM params, keeps unrelated ones");
  equal(locationHref(goTo(app, { screen: "home" }), crm), "/?keep=1", "home drops CRM params");
}

// CAD-2286: the Documents screen and the Settings → Documents section. The
// screen's tab (`?archived=1`) and stage filter (`?stage=`) live in its URL,
// as My work's `?tab=` does, and never follow a navigation elsewhere.
{
  equal(matchRoute("/documents"), { screen: "documents", archived: false }, "match /documents");
  equal(routePath({ screen: "documents", archived: false }), "/documents", "print /documents");
  equal(matchRoute("/settings/documents"), { screen: "settings", section: "documents" }, "match /settings/documents");
  equal(routePath({ screen: "settings", section: "documents" }), "/settings/documents", "print /settings/documents");
  for (const dead of ["/documents/x", "/documents/x/y", "/settings/documents/extra"]) {
    equal(matchRoute(dead).screen, "notFound", `not found ${dead}`);
  }

  const active = readLocation("/documents", "");
  equal(active.route, { screen: "documents", archived: false }, "/documents is the Active tab");
  equal(locationHref(active, ""), "/documents", "the Active tab has a bare URL");

  const archived = readLocation("/documents", "?archived=1");
  equal(archived.route, { screen: "documents", archived: true }, "?archived=1 is the Archived tab");
  equal(locationHref(archived, "?archived=1"), "/documents?archived=1", "the Archived tab round-trips");

  const staged = readLocation("/documents", "?stage=in_review");
  equal(staged.route, { screen: "documents", archived: false, stage: "in_review" }, "?stage= filters the Active tab");
  equal(locationHref(staged, "?stage=in_review"), "/documents?stage=in_review", "the stage filter round-trips");
  for (const stage of ["draft", "in_review", "approved", "delivered"]) {
    equal(readLocation("/documents", `?stage=${stage}`).route, { screen: "documents", archived: false, stage }, `stage ${stage} is a filter`);
  }

  equal(readLocation("/documents", "?archived=yes&stage=bogus").route, { screen: "documents", archived: false }, "malformed values fall back to the default");
  equal(readLocation("/documents", "?stage=archived").route, { screen: "documents", archived: false }, "archived is a tab, not a stage filter");
  equal(readLocation("/documents", "?archived=1&stage=approved").route, { screen: "documents", archived: true }, "the Archived tab has no stage filter");
  equal(locationHref({ ...archived, route: { screen: "documents", archived: true, stage: "approved" } }, ""), "/documents?archived=1", "the Archived tab never writes a stage");

  const both = "?stage=approved&keep=1";
  const loc = readLocation("/documents", both);
  equal(locationHref(goTo(loc, { screen: "threads" }), both), "/threads?keep=1", "the stage stays on Documents");
  equal(locationHref(goTo(archived, { screen: "home" }), "?archived=1"), "/", "the tab stays on Documents");
  equal(locationHref(goTo(loc, { screen: "documents", archived: true }), both), "/documents?keep=1&archived=1", "switching tabs drops the Active tab's stage");
  equal(projectScope(loc.route), "none", "Documents has no project scope");

  // A mounted app's own query may use the same names; its route keeps them.
  const appQuery = "?stage=lead&archived=1&keep=1";
  const app = readLocation("/apps/crm", appQuery);
  equal(locationHref(app, appQuery), "/apps/crm?stage=lead&archived=1&keep=1", "an app route keeps its own stage and archived params");
  const legacyApp = readLocation("/app-installations/install-a", appQuery);
  equal(locationHref(legacyApp, appQuery), "/app-installations/install-a?stage=lead&archived=1&keep=1", "the compat app route keeps them too");
  equal(locationHref(goTo(app, { screen: "home" }), appQuery), "/?keep=1", "leaving the app drops them");
  // Leaving Documents for an app route drops Documents' own params: they were
  // the screen's, not the app's.
  const crm: Route = { screen: "workspaceAppKey", appKey: "crm" };
  equal(locationHref(goTo(loc, crm), both, loc.route), "/apps/crm?keep=1", "documents → app drops the stage");
  equal(locationHref(goTo(archived, crm), "?archived=1&keep=1", archived.route), "/apps/crm?keep=1", "documents → app drops the tab");
  equal(locationHref(goTo(archived, { screen: "workspaceApp", installId: "install-a" }), "?archived=1", archived.route), "/app-installations/install-a", "documents → compat app route drops them too");
}

// Leaving a mounted app or switching apps strips section_query + app-owned params while keeping unrelated ones.
{
  const hrSearch = "?hr=leave&record=rec-1&conversation=conv-1&drawer=connect&drawer_size=md&foo=1";
  const hrLoc = readLocation("/apps/hr", hrSearch);
  equal(
    locationHref(goTo(hrLoc, { screen: "home" }), hrSearch, hrLoc.route),
    "/?foo=1",
    "leaving /apps/hr for home strips hr, record, conversation, drawer, drawer_size",
  );
  equal(
    locationHref(goTo(hrLoc, { screen: "threads" }), hrSearch, hrLoc.route),
    "/threads?foo=1",
    "leaving /apps/hr for threads strips app query params",
  );
  equal(
    locationHref(goTo(hrLoc, { screen: "workspaceAppKey", appKey: "accounting" }), hrSearch, hrLoc.route),
    "/apps/accounting?foo=1",
    "switching from /apps/hr to /apps/accounting strips previous app query params",
  );
  const customSearch = "?people_tab=directory&record=r9&unknown=stay";
  const hrLegacy: Route = { screen: "workspaceApp", installId: "inst-hr" };
  equal(
    appNavigationSearch(hrLegacy, hrLegacy, customSearch, "people_tab"),
    customSearch,
    "staying on the same app keeps declared section_query",
  );
  equal(
    appNavigationSearch(hrLegacy, { screen: "home" }, customSearch, "people_tab"),
    "unknown=stay",
    "leaving an app strips its declared activeSectionQuery",
  );
  equal(
    appNavigationSearch(hrLegacy, { screen: "workspaceAppKey", appKey: "accounting" }, customSearch, "people_tab"),
    "unknown=stay",
    "switching apps strips activeSectionQuery",
  );
  equal(
    stripAppRouteParams("?crm=leads&record=r2&hr_tab=leave&project=cadence&keep=1", "hr_tab"),
    "project=cadence&keep=1",
    "stripAppRouteParams drops declared target section_query and keeps unrelated params",
  );
  equal(
    stripAppRouteParams("?mode=edit&q=abc&section=pay&tab=runs&issue=CAD-1&project=cadence&keep=1", [
      "mode",
      "q",
      "section",
      "tab",
    ]),
    "issue=CAD-1&project=cadence&keep=1",
    "stripAppRouteParams drops declared section_query keys even when overlapping screen-local names and accepts arrays",
  );
  equal(
    appNavigationSearch(
      hrLegacy,
      { screen: "home" },
      "?section=pay&mode=draft&q=filter&project=cadence&issue=CAD-2&keep=1",
      ["section", "mode", "q"],
    ),
    "project=cadence&issue=CAD-2&keep=1",
    "appNavigationSearch strips declared section_query arrays while preserving global project and issue params",
  );
  equal(
    stripAppRouteParams("?project=cadence&issue=CAD-1&record=r2&keep=1", ["project", "issue"]),
    "project=cadence&issue=CAD-1&keep=1",
    "stripAppRouteParams preserves global cross-route host keys even if passed as extraSectionQuery",
  );
}
