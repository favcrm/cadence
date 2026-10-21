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
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { NavList: NavListView, ProjectList: ProjectListView } = require("../src/ui/NavList") as typeof import("../src/ui/NavList");
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
const NavList = (props: React.ComponentProps<typeof NavListView>) =>
  React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(NavListView, props) });
const ProjectList = (props: React.ComponentProps<typeof ProjectListView>) =>
  React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(ProjectListView, props) });
const { NAV } = require("../src/lib/router") as typeof import("../src/lib/router");
const fs = require("fs");

const hrefs = (html: string) => Array.from(html.matchAll(/<a [^>]*href="([^"]*)"/g)).map((m) => m[1]);
const navHref = (route: { screen: string }) => `/${route.screen}`;
const appMenu = {
  apps: [
    {
      installId: "install-crm", title: "CRM", href: "/crm?s", current: true,
      sections: [
        { label: "Customers", href: "/crm?c", current: false },
        { label: "Segments", href: "/crm?s", current: true },
      ],
    },
    { installId: "install-soc", title: "Social Content", href: "/soc", current: false, sections: null },
  ],
};
const nav = (screen: string, menu: typeof appMenu | null, label?: string) =>
  renderToStaticMarkup(React.createElement(LocaleProvider, null, React.createElement(NavList, { screen, navHref, appMenu: menu, label } as any)));

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
assert(/<a [^>]*href="\/crm\?s"[^>]*class="navlink navlink-sub"|<a [^>]*class="navlink navlink-sub"[^>]*href="\/crm\?s"/.test(withMenu), "each installed app is a link row");
assert(hrefs(withMenu).includes("/soc"), "an app without sections still lists");
assert(!/aria-label="Social Content sections"/.test(withMenu), "no sections list for an app that declares none");
assert(!withMenu.includes("slabel"), "no separate app heading");
// An open app without sections takes the accent alone; Apps is the quiet parent.
const social = nav("apps", { apps: [{ ...appMenu.apps[1], current: true }] });
assert(!/<a [^>]*aria-current="page"[^>]*>(?:(?!<\/a>).)*Apps<\/a>/.test(social), "Apps is not page-current when an app row is");
assert(/aria-current="page"[^>]*>(?:(?!<\/a>).)*Social Content/.test(social), "the open app row is page-current");
// Off the Apps screen the parent marker is absent even when a menu is passed.
assert(!nav("home", appMenu).includes("data-parent-current") && !nav("home", appMenu).includes('aria-current="true"'), "parent marker only when Apps is current");

// Projects: empty list shows a plain line instead of the All projects row.
const state = (data: unknown[] | null, status = "ready") => ({ data, status, error: null }) as any;
const projectList = (projects: unknown[], extra: object = {}) =>
  renderToStaticMarkup(React.createElement(LocaleProvider, null, React.createElement(ProjectList, { project: null, projectHref: (k: string) => `/p/${k}`, projects, issues: state([]), ...extra } as any)));
const empty = projectList([]);
assert(empty.includes("No projects yet"), "empty list says so");
assert(!empty.includes("All projects") && hrefs(empty).length === 0, "empty list has no All projects row or link");
const error = projectList([], { projectsError: "boom" });
assert(error.includes("All projects") && error.includes("could not load projects"), "a load error keeps the row and the alert");
const several = projectList([{ key: "alpha", prefix: "ALP" }, { key: "beta", prefix: "BET" }]);
assert(several.includes("All projects") && !several.includes("No projects yet"), "projects keep the All projects row");
equal(hrefs(several), ["/p/all", "/p/alpha", "/p/beta"], "one row per project");

// CAD-1692: per-browser fold and pin state. The store is shared by the
// desktop sidebar and the phone menu; static markup reads its snapshot.
const prefsMod = require("../src/features/app-shell/sidebarPrefs") as typeof import("../src/features/app-shell/sidebarPrefs");
const { readSidebarPrefs, updateSidebarPrefs, noteAppScreen, DEFAULT_SIDEBAR_PREFS, SIDEBAR_PREFS_KEY } = prefsMod;
const mem = (raw: string | null) => ({ getItem: (k: string) => (k === SIDEBAR_PREFS_KEY ? raw : null) });
equal(readSidebarPrefs(null), DEFAULT_SIDEBAR_PREFS, "no storage means the defaults");
equal(readSidebarPrefs({ getItem: () => { throw new Error("blocked"); } }), DEFAULT_SIDEBAR_PREFS, "throwing storage means the defaults");
equal(readSidebarPrefs(mem("{not json")), DEFAULT_SIDEBAR_PREFS, "malformed storage means the defaults");
equal(readSidebarPrefs(mem('{"appsOpen":"no","pinned":["a",3,"a",""],"closedApps":"x"}')),
  { ...DEFAULT_SIDEBAR_PREFS, pinned: ["a"] }, "wrong-typed fields fall back; pins keep only unique non-empty strings");

const pick = (html: string, re: RegExp) => (re.exec(html) ?? [""])[0];
const nameTexts = (html: string) => Array.from(html.matchAll(/<span class="truncate">([^<]*)<\/span>/g)).map((m) => m[1]);
const set = (p: Partial<typeof DEFAULT_SIDEBAR_PREFS>) => updateSidebarPrefs(() => ({ ...DEFAULT_SIDEBAR_PREFS, ...p }));

// Apps fold: the Apps link still navigates; a separate button folds the tree.
set({});
const open = nav("apps", appMenu);
assert(/<button[^>]*class="navdisc"[^>]*aria-expanded="true"[^>]*aria-label="Collapse apps"/.test(open), "Apps has an expanded disclosure button");
assert(/<a [^>]*href="\/apps"[^>]*>(?:(?!<\/a>).)*Apps<\/a>/.test(open), "the Apps label stays a link to the Apps page");
assert(/<button[^>]*aria-expanded="true"[^>]*aria-label="Collapse CRM sections"/.test(open), "an app with sections has its own disclosure");
assert(!/aria-label="[^"]*Social Content sections"/.test(open), "an app without sections has none");
set({ appsOpen: false });
const folded = nav("apps", appMenu);
assert(/<button[^>]*aria-expanded="false"[^>]*aria-label="Expand apps"/.test(folded), "folded Apps says so");
const foldedTree = pick(folded, /<div [^>]*class="navfold"[^>]*>/);
assert(foldedTree.includes('aria-hidden="true"') && /inert=""|inert=/.test(foldedTree), "the folded tree is hidden and inert");
const peek = pick(folded, /<div class="navtree"><a [^>]*>(?:(?!<\/a>).)*<\/a><\/div>/);
assert(peek.includes('aria-current="page"') && peek.includes(">CRM<"), "folded on an app screen, the open app keeps one current row");
set({ appsOpen: false });
assert(!/<div class="navtree"><a /.test(nav("home", appMenu)), "folded off an app screen, no peek row");
set({ closedApps: ["install-crm"] });
const crmClosed = nav("apps", appMenu);
assert(/<button[^>]*aria-expanded="false"[^>]*aria-label="Expand CRM sections"/.test(crmClosed), "a closed app's sections say so");
assert(/<div [^>]*class="navfold"[^>]*aria-hidden="true"[^>]*><div class="navfold-in"><nav aria-label="CRM sections"/.test(crmClosed), "a closed app's sections are folded");

// Entering an app reopens Apps and that app. Loading straight onto an app
// page (even before its installation resolves), staying, and leaving never do.
const { resetPrivateSession: resetSession, allowPrivateReads: allowReads } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
const expanded = () => /aria-label="Collapse apps"/.test(nav("apps", appMenu));
const crmOpen = () => /aria-label="Collapse CRM sections"/.test(nav("apps", appMenu));
// (a) Reload onto an app page: unresolved first, then resolved — a load, not an entry.
resetSession(); allowReads();
set({ appsOpen: false, closedApps: ["install-crm"] });
noteAppScreen(true, null);
noteAppScreen(true, "install-crm");
assert(!expanded() && !crmOpen(), "loading onto an app page keeps the folds");
noteAppScreen(true, "install-crm");
assert(!expanded(), "staying on one app keeps the fold");
noteAppScreen(true, "install-soc");
assert(expanded(), "switching to another app unfolds Apps");
set({ appsOpen: false, closedApps: ["install-crm"] });
noteAppScreen(false, null);
assert(!expanded(), "leaving an app keeps a fold");
noteAppScreen(true, "install-crm");
assert(expanded() && crmOpen(), "entering an app unfolds Apps and its sections");
// (b) Reload onto Home, then enter an app before the installed-app list answers.
resetSession(); allowReads();
set({ appsOpen: false, closedApps: ["install-crm"] });
noteAppScreen(false, null);
noteAppScreen(true, null);
assert(!expanded(), "an unresolved app screen decides nothing yet");
noteAppScreen(true, "install-crm");
assert(expanded() && crmOpen(), "entry from Home counts once the app resolves, even if the list was still loading");

// Projects: pins lead, the rest wait behind Show N more, the current project stays.
const five = ["alpha", "beta", "gamma", "delta", "eps"].map((key) => ({ key, prefix: key.slice(0, 3).toUpperCase() }));
set({});
equal(hrefs(projectList(five)), ["/p/all", ...five.map((p) => `/p/${p.key}`)], "no pins: today's list, same order");
assert(!projectList(five).includes("projmore") && !projectList(five).includes("projsep"), "no pins: no divider or toggle");
set({ pinned: ["gamma", "alpha"] });
const pinnedList = projectList(five);
equal(nameTexts(pinnedList), ["All projects", "gamma", "alpha"], "pins lead, in pin order, rest hidden");
assert(pinnedList.includes("Show 3 more"), "the toggle counts the hidden projects");
assert(/data-pinned=""[^>]*>(?:(?!<\/div>).)*aria-pressed="true"[^>]*aria-label="Unpin gamma"/.test(pinnedList), "a pinned row offers Unpin");
const onDelta = projectList(five, { project: "delta" });
equal(nameTexts(onDelta), ["All projects", "gamma", "alpha", "delta"], "the project on screen stays visible");
assert(onDelta.includes("projsep") && onDelta.includes("Show 2 more"), "divider before it; the toggle counts the rest");
assert(/class="proj on"[^>]*aria-current="page"[^>]*>(?:(?!<\/a>).)*delta/.test(onDelta), "and it is highlighted");
set({ pinned: ["gamma", "alpha"], showAll: true });
equal(nameTexts(projectList(five)), ["All projects", "gamma", "alpha", "beta", "delta", "eps"], "show all: pins, then the rest in order");
assert(projectList(five).includes("Show pinned only"), "show all offers the way back");
set({ pinned: ["gone", "beta"] });
equal(nameTexts(projectList(five)), ["All projects", "beta"], "a pin for a removed project renders nothing");
set({ pinned: ["alpha", "beta", "gamma", "delta", "eps"] });
assert(!projectList(five).includes("projmore"), "every project pinned: no empty toggle");

// The Projects heading folds the section and shows the count while folded.
const { ProjectsSection: ProjectsSectionView } = require("../src/ui/NavList") as typeof import("../src/ui/NavList");
// Fixture projects carry only the fields the sidebar reads.
const sectionProps = { project: null, projectHref: (k: string) => `/p/${k}`, projects: five, issues: state([]) } as unknown as React.ComponentProps<typeof ProjectsSectionView>;
const section = () => renderToStaticMarkup(React.createElement(LocaleProvider, null, React.createElement(NavigationProvider, { adapter: hostNavigation, children:
  React.createElement(ProjectsSectionView, sectionProps) })));
set({});
assert(/<button[^>]*class="projhead-toggle slabel"[^>]*aria-expanded="true"/.test(section()) && section().includes("~/pm"), "open section: expanded heading, ~/pm hint");
set({ projectsOpen: false });
const closed = section();
assert(/aria-expanded="false"/.test(closed) && />5<\/span>/.test(closed), "folded section: collapsed heading shows the project count");
assert(/<div [^>]*class="navfold"[^>]*aria-hidden="true"/.test(closed), "folded section hides its list");
set({});

// A confirmed identity change drops the previous session's layout and pins.
set({ pinned: ["alpha"], appsOpen: false });
resetSession();
allowReads();
equal(nameTexts(projectList(five)), ["All projects", ...five.map((p) => p.key)], "a session reset unpins everything");
assert(/aria-label="Collapse apps"/.test(nav("apps", appMenu)), "a session reset unfolds Apps");

// One row style: the phone menu no longer carries its own grid.
const app = fs.readFileSync("src/App.tsx", "utf8");
assert(!app.includes("grid-cols-2 sm:grid-cols-4"), "App.tsx has no second nav grid");
assert(app.includes("<NavList"), "the phone menu renders NavList");

console.log("sidebar nav checks passed");
