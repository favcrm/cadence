export {};
/**
 * CAD-1031: one compact nav row list for the desktop sidebar and the
 * phone menu. NavList/ProjectList render to static markup; the real-App
 * phone-menu behaviour (nesting, closing on click) stays in
 * crmAppMenu.test.ts.
 */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");
const { NavList, ProjectList } = require("../src/ui/NavList") as typeof import("../src/ui/NavList");
const { NAV } = require("../src/lib/router") as typeof import("../src/lib/router");
const fs = require("fs");

const hrefs = (html: string) => Array.from(html.matchAll(/<a [^>]*href="([^"]*)"/g)).map((m) => m[1]);
const navHref = (route: { screen: string }) => `/${route.screen}`;
const appMenu = {
  title: "CRM",
  sections: [
    { label: "Customers", href: "/crm?c", current: false },
    { label: "Segments", href: "/crm?s", current: true },
  ],
};
const nav = (screen: string, menu: typeof appMenu | null, label?: string) =>
  renderToStaticMarkup(React.createElement(NavList, { screen, navHref, appMenu: menu, label } as any));

// Same items, whichever landmark hosts the list (desktop "Primary", phone "Workspace").
equal(hrefs(nav("home", null)), hrefs(nav("home", null, "Workspace")), "desktop and phone render the same rows");
equal(hrefs(nav("home", null)).length, NAV.length, "one row per NAV item");
assert(nav("home", null, "Workspace").includes('aria-label="Workspace"'), "phone landmark is named Workspace");
assert(!nav("home", null).includes("navlink-sub"), "no sub rows without an app menu");
assert(!nav("home", null).includes("data-parent-current"), "no parent marker without an app menu");

// Without an app menu the Apps row is the plain current page.
const plain = nav("apps", null);
assert(/<a [^>]*aria-current="page"[^>]*>(?:(?!<\/a>).)*Apps/.test(plain), "Apps is aria-current=page without a menu");
assert(!plain.includes("data-parent-current"), "no data-parent-current without a menu");

// With an app menu the sub row carries the accent; Apps is a quiet parent.
const withMenu = nav("apps", appMenu);
const apps = /<a [^>]*>(?:(?!<\/a>).)*Apps<\/a>/.exec(withMenu)![0];
assert(apps.includes('aria-current="true"') && apps.includes("data-parent-current"), "Apps is the quiet parent");
assert(!apps.includes('aria-current="page"'), "parent does not take the accent");
assert(/class="navlink navlink-sub"[^>]*aria-current="page"[^>]*>Segments|aria-current="page"[^>]*class="navlink navlink-sub"[^>]*>Segments/.test(withMenu), "current sub row carries aria-current=page");
assert(withMenu.includes('aria-label="CRM sections"'), "sub list is its own landmark");
assert(withMenu.includes('<div class="text-micro text-ink-500 px-2 pt-0.5 pb-1 truncate" title="CRM">CRM</div>'), "app name is the muted first line");
assert(!withMenu.includes("slabel"), "no separate app heading");
// Off the Apps screen the parent marker is absent even when a menu is passed.
assert(!nav("home", appMenu).includes("data-parent-current"), "parent marker only when Apps is current");

// Projects: empty list shows a plain line instead of the All projects row.
const state = (data: unknown[] | null, status = "ready") => ({ data, status, error: null }) as any;
const projectList = (projects: unknown[], extra: object = {}) =>
  renderToStaticMarkup(React.createElement(ProjectList, { project: null, projectHref: (k: string) => `/p/${k}`, projects, issues: state([]), ...extra } as any));
const empty = projectList([]);
assert(empty.includes("No projects yet"), "empty list says so");
assert(!empty.includes("All projects") && hrefs(empty).length === 0, "empty list has no All projects row or link");
const error = projectList([], { projectsError: "boom" });
assert(error.includes("All projects") && error.includes("could not load projects"), "a load error keeps the row and the alert");
const several = projectList([{ key: "alpha", prefix: "ALP" }, { key: "beta", prefix: "BET" }]);
assert(several.includes("All projects") && !several.includes("No projects yet"), "projects keep the All projects row");
equal(hrefs(several), ["/p/all", "/p/alpha", "/p/beta"], "one row per project");

// One row style: the phone menu no longer carries its own grid.
const app = fs.readFileSync("src/App.tsx", "utf8");
assert(!app.includes("grid-cols-2 sm:grid-cols-4"), "App.tsx has no second nav grid");
assert(app.includes("<NavList"), "the phone menu renders NavList");

console.log("sidebar nav checks passed");
