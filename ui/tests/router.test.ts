import { NO_FILTERS } from "../src/lib/filters";
import {
  goTo,
  legacyRedirect,
  locationHref,
  matchRoute,
  NAV,
  openProject,
  projectScope,
  readLocation,
  routePath,
  scopeRedirect,
  showProjectChoices,
  withProject,
  type Route,
} from "../src/lib/router";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// Every MVP route matches and prints back to the same path.
const paths: [string, Route][] = [
  ["/", { screen: "home" }],
  ["/projects", { screen: "projects", slug: null, section: "overview" }],
  ["/projects/cadence", { screen: "projects", slug: "cadence", section: "overview" }],
  ["/projects/cadence/issues", { screen: "projects", slug: "cadence", section: "issues" }],
  ["/projects/cadence/context", { screen: "projects", slug: "cadence", section: "context" }],
  ["/projects/cadence/epics", { screen: "projects", slug: "cadence", section: "epics" }],
  ["/projects/cadence/milestones", { screen: "projects", slug: "cadence", section: "milestones" }],
  ["/projects/cadence/workflows", { screen: "projects", slug: "cadence", section: "workflows" }],
  ["/apps", { screen: "apps", project: null, name: null }],
  ["/apps/cadence/studio", { screen: "apps", project: "cadence", name: "studio" }],
  ["/agents", { screen: "agents", alias: null }],
  ["/agents/cc-1", { screen: "agents", alias: "cc-1" }],
  ["/setup", { screen: "setup" }],
  ["/overview", { screen: "overview" }],
  ["/settings", { screen: "settings", section: "models" }],
  ["/settings/memory", { screen: "settings", section: "memory" }],
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
for (const dead of ["/overview/x", "/nope", "/projects/x/y", "/agents/a/b", "/apps/p", "/apps/p/n/x", "/settings/nope", "/setup/x", "/login/x", "/projects/%E0"]) {
  equal(matchRoute(dead).screen, "notFound", `not found ${dead}`);
}

// Main nav: MVP screens only (Wiki joined in CAD-581).
equal(
  NAV.map((n) => n.label),
  ["Home", "Projects", "Wiki", "Apps", "Agents", "Outbox", "Settings"],
  "nav",
);

// Which screens keep a project. One table.
equal(projectScope({ screen: "projects", slug: "cadence", section: "issues" }), "slug", "projects are the slug");
equal(projectScope({ screen: "agents", alias: null }), "chips", "agents");
equal(projectScope({ screen: "overview" }), "chips", "overview");
equal(projectScope({ screen: "apps", project: null, name: null }), "chips", "apps list");
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
// in-page filter on Agents, Overview, Apps and Memory. Elsewhere it is ignored.
{
  const loc = readLocation("/projects/cadence", "?view=kanban&tag=ui&issue=CAD-1", "list");
  equal(loc.project, "cadence", "slug scope");
  equal(loc.view, "kanban", "explicit view");
  equal(loc.filters.tags, ["ui"], "filters on projects");
  equal(loc.openId, "CAD-1", "drawer");
  const agents = readLocation("/agents", "?project=cadence&tag=ui");
  equal(agents.project, "cadence", "agents filter");
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
  equal(readLocation("/apps", "?project=cadence").project, "cadence", "apps query scope");
}

// An app detail href does not repeat its path's project in the query.
{
  const loc = readLocation("/apps/cadence/studio", "");
  equal(locationHref(loc), "/apps/cadence/studio", "app detail href");
  const list = readLocation("/apps", "?project=cadence");
  equal(locationHref(list), "/apps?project=cadence", "apps href keeps scope");
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

// Moving between screens carries the scope and the open drawer.
{
  const here = readLocation("/projects/cadence", "?view=list&issue=CAD-3");
  equal(
    locationHref(goTo(here, { screen: "agents", alias: null })),
    "/agents?project=cadence&issue=CAD-3",
    "projects → agents",
  );
  const agents = readLocation("/agents", "?project=cadence");
  equal(
    locationHref(goTo(agents, { screen: "projects", slug: null, section: "issues" })),
    "/projects/cadence/issues?view=list",
    "agents → projects keeps the scope as the slug",
  );
  equal(locationHref(openProject(here, "all")), "/projects?view=list&issue=CAD-3", "sidebar all projects opens the board");
  equal(
    locationHref(openProject(readLocation("/agents", "?project=kidult"), "cadence")),
    "/projects/cadence",
    "sidebar on agents opens the project overview",
  );
  equal(
    locationHref(openProject(readLocation("/projects/cadence/epics", ""), "kidult")),
    "/projects/kidult/epics",
    "sidebar keeps the project section",
  );
  equal(
    locationHref(withProject(readLocation("/agents", ""), "cadence")),
    "/agents?project=cadence",
    "agents chip sets the filter",
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
  ["?tab=agents&project=cadence&issue=CAD-7", "/agents?project=cadence&issue=CAD-7"],
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
equal(scopeRedirect("/agents", "?project=cadence"), null, "agents keeps the filter");
equal(scopeRedirect("/apps", "?project=cadence"), null, "apps keeps the filter");
equal(scopeRedirect("/overview", "?project=cadence"), null, "overview keeps the filter");
equal(scopeRedirect("/settings/memory", "?project=cadence"), null, "memory keeps the filter");
equal(legacyRedirect("/projects/cadence", "?view=list"), null, "a routed URL is not legacy");
equal(legacyRedirect("/index.html", "?tab=agents"), "/agents", "index.html with a tab");
equal(legacyRedirect("/index.html", ""), "/", "bare index.html");

console.log("router checks passed");

// A plain Projects entry is overview; existing board bookmarks remain Issues.
equal(readLocation("/projects", "").route, { screen: "projects", slug: null, section: "overview" }, "portfolio landing");
equal(readLocation("/projects/cadence", "?view=list").route, { screen: "projects", slug: "cadence", section: "issues" }, "legacy issue bookmark");
