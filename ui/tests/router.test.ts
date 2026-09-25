import { NO_FILTERS } from "../src/lib/filters";
import {
  goTo,
  legacyRedirect,
  locationHref,
  matchRoute,
  NAV,
  readLocation,
  routePath,
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
  ["/projects", { screen: "projects", slug: null, section: "issues" }],
  ["/projects/cadence", { screen: "projects", slug: "cadence", section: "issues" }],
  ["/projects/cadence/context", { screen: "projects", slug: "cadence", section: "context" }],
  ["/projects/cadence/epics", { screen: "projects", slug: "cadence", section: "epics" }],
  ["/projects/cadence/milestones", { screen: "projects", slug: "cadence", section: "milestones" }],
  ["/projects/cadence/workflows", { screen: "projects", slug: "cadence", section: "workflows" }],
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
for (const dead of ["/overview/x", "/nope", "/projects/x/y", "/agents/a/b", "/settings/nope", "/setup/x", "/login/x", "/projects/%E0"]) {
  equal(matchRoute(dead).screen, "notFound", `not found ${dead}`);
}

// Main nav: MVP screens only.
equal(NAV.map((n) => n.label), ["Home", "Projects", "Agents", "Outbox", "Settings"], "nav");

// Reading a location: the slug is the scope on Projects, ?project= elsewhere.
{
  const loc = readLocation("/projects/cadence", "?view=kanban&tag=ui&issue=CAD-1", "list");
  equal(loc.project, "cadence", "slug scope");
  equal(loc.view, "kanban", "explicit view");
  equal(loc.filters.tags, ["ui"], "filters on projects");
  equal(loc.openId, "CAD-1", "drawer");
  const agents = readLocation("/agents", "?project=cadence&tag=ui");
  equal(agents.project, "cadence", "query scope");
  equal(agents.filters, NO_FILTERS, "filters only on projects");
  equal(readLocation("/projects", "", "kanban").view, "kanban", "stored view");
  equal(readLocation("/", "").project, "all", "no scope");
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
  equal(href, "/projects/cadence?unknown=keep&view=kanban", "projects href");
  const home = locationHref(
    { route: { screen: "home" }, project: "cadence", view: "list", openId: "CAD-9", filters: NO_FILTERS },
    "?view=list&tab=board",
  );
  equal(home, "/?project=cadence&issue=CAD-9", "home keeps scope and drawer, drops view");
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
    "/projects/cadence?view=list",
    "agents → projects keeps the scope as the slug",
  );
  equal(locationHref(withProject(here, "all")), "/projects?view=list&issue=CAD-3", "all projects");
  equal(
    locationHref(withProject(readLocation("/settings/memory", ""), "cadence")),
    "/settings/memory?project=cadence",
    "memory scoped",
  );
}

// Pre-router URLs redirect to their new home, keeping scope, drawer and filters.
const legacy: [string, string][] = [
  ["?tab=overview&project=cadence", "/?project=cadence"],
  ["?tab=board&view=kanban&project=cadence&tag=ui", "/projects/cadence?view=kanban&tag=ui"],
  ["?project=cadence&view=list", "/projects/cadence?view=list"],
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
equal(legacyRedirect("/", "?project=cadence"), null, "a project-only link stays on Home");
equal(legacyRedirect("/projects/cadence", "?view=list"), null, "a routed URL is not legacy");
equal(legacyRedirect("/index.html", "?tab=agents"), "/agents", "index.html with a tab");
equal(legacyRedirect("/index.html", ""), "/", "bare index.html");

console.log("router checks passed");
