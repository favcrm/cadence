import { matchRoute, NAV, routePath, type Route } from "../src/lib/router";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// CAD-581: the wiki's routes — a real path per store path, modes as
// reserved first segments, the search query as a path segment.
equal(matchRoute("/wiki"), { screen: "wiki", mode: "browse", path: null, query: null }, "the root");
equal(
  matchRoute("/wiki/projects/cadence/notes.md"),
  { screen: "wiki", mode: "browse", path: "projects/cadence/notes.md", query: null },
  "a page deep link",
);
equal(
  matchRoute("/wiki/agents/swe-1/knowledge/"),
  { screen: "wiki", mode: "browse", path: "agents/swe-1/knowledge", query: null },
  "a folder, trailing slash and all",
);
equal(
  matchRoute("/wiki/edit/projects/cadence/notes.md"),
  { screen: "wiki", mode: "edit", path: "projects/cadence/notes.md", query: null },
  "the editor",
);
equal(
  matchRoute("/wiki/history/global/glossary.md"),
  { screen: "wiki", mode: "history", path: "global/glossary.md", query: null },
  "history",
);
equal(
  matchRoute("/wiki/upload/projects/cadence/design"),
  { screen: "wiki", mode: "upload", path: "projects/cadence/design", query: null },
  "upload into a folder",
);
equal(matchRoute("/wiki/search"), { screen: "wiki", mode: "search", path: null, query: null }, "search, empty");
equal(
  matchRoute("/wiki/search/board%20shell"),
  { screen: "wiki", mode: "search", path: null, query: "board shell" },
  "search with a query",
);
equal(
  matchRoute("/wiki/search/a/b"),
  { screen: "wiki", mode: "search", path: null, query: "a/b" },
  "a query containing a slash",
);
// A page named like a mode is still reachable — only exact segments reserve.
equal(
  matchRoute("/wiki/search.md"),
  { screen: "wiki", mode: "browse", path: "search.md", query: null },
  "search.md is a page, not the search screen",
);
const escaped = matchRoute("/wiki/%E2%82%AC/notes.md");
equal(escaped.screen === "wiki" ? escaped.path : null, "€/notes.md", "escaped segments decode");
equal(matchRoute("/wikis").screen, "notFound", "no prefix matching");

// And back: every route has a stable href that parses to itself.
const routes: Route[] = [
  { screen: "wiki", mode: "browse", path: null, query: null },
  { screen: "wiki", mode: "browse", path: "projects/cadence/notes.md", query: null },
  { screen: "wiki", mode: "browse", path: "agents/swe-1/knowledge", query: null },
  { screen: "wiki", mode: "edit", path: "projects/cadence/notes.md", query: null },
  { screen: "wiki", mode: "history", path: "global/glossary.md", query: null },
  { screen: "wiki", mode: "upload", path: "projects/cadence/design", query: null },
  { screen: "wiki", mode: "search", path: null, query: null },
  { screen: "wiki", mode: "search", path: null, query: "board shell" },
  { screen: "wiki", mode: "search", path: null, query: "a/b c" },
];
for (const route of routes) {
  equal(matchRoute(routePath(route)), route, `round trip ${routePath(route)}`);
}
equal(routePath({ screen: "wiki", mode: "browse", path: "my notes/é.md", query: null }), "/wiki/my%20notes/%C3%A9.md", "escaped href");

// The nav entry exists and points at the root.
const nav = NAV.find((item) => item.screen === "wiki");
equal(nav?.label, "Wiki", "the Wiki nav entry");
equal(nav?.route, { screen: "wiki", mode: "browse", path: null, query: null }, "the nav's route");
equal(NAV.map((item) => item.label).indexOf("Wiki") > NAV.map((item) => item.label).indexOf("Projects"), true, "Wiki sits after Projects");

console.log("wiki route checks passed");
