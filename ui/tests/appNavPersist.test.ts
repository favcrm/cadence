export {};
/**
 * CAD-1116: the sidebar Apps item lists the installed apps from the
 * verified installation list on every screen, and the last-used app keeps
 * its sections (and last section) off the app screens. Mounts the real App.
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

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
Object.defineProperty(globalThis, "EventSource", {
  configurable: true,
  value: class {
    onopen: null = null;
    onerror: null = null;
    constructor() {}
    addEventListener() {}
    removeEventListener() {}
    close() {}
  },
});
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  if (id === "react-markdown") return { __esModule: true, default: () => null };
  if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const App = (require("../src/App") as typeof import("../src/App")).default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");

const mk = (id: string, name: string, title: string) => ({
  install_id: id, title, name, version: "0.1.0",
  summary: "", digest: "install-digest", catalog_generation: "gen-1", approved: true,
  storage_kind: "workspace", project_link: null, executable: true,
  approval: { state: "approved" }, guide: "", files: [], capabilities: null, connection_slots: [],
});
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
let listCalls = 0;
globalThis.fetch = (async (input: unknown) => {
  const path = String(input);
  const url = new URL(path, "http://localhost");
  if (path.startsWith("/api/meta")) return json({ read_only: false, operator: true, signed_in: true, actor: "operator (ui)", session: null });
  if (path === "/api/issues" || path.startsWith("/api/issues?")) return json({ issues: [] });
  if (path === "/api/projects") return json({ projects: [] });
  if (path === "/api/agents") return json({ daemon: "unreachable", agents: [], totals: null });
  if (path === "/api/health") return json({ ok: true, pm_present: false, projects: 0, issues: 0, daemon: "stub", embedded: true });
  if (path === "/api/update/banner") return json(null);
  if (path.startsWith("/api/threads/master")) return json({ thread: null, entries: [], more_before: false });
  if (path === "/api/app-installations") { listCalls++; return json([mk("install-crm", "crm", "CRM"), mk("install-soc", "social-content", "Social Content")]); }
  if (path === "/api/app-installations/install-crm") return json(mk("install-crm", "crm", "CRM"));
  if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [] });
  if (path.startsWith("/api/app-installations/install-forged")) return json({ error: "unknown installation" }, 404);
  if (url.pathname.endsWith("/records")) return json({ records: [], truncated: false, next_cursor: null });
  if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
  if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
  if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
  if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: [] });
  if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: [] });
  if (url.pathname.startsWith("/api/settings") || url.pathname.startsWith("/api/connections")) return json({});
  throw new Error(`Unexpected read ${path}`);
}) as typeof fetch;

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
async function settle(check: () => void) {
  for (let i = 0; i < 60; i++) {
    await flush();
    try { check(); return; } catch { /* keep polling */ }
  }
  await flush();
  check();
}
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
const go = (path: string) => React.act(async () => { navigate(path); });
const appLinks = () => Array.from(host.querySelectorAll('aside nav[aria-label="Primary"] > div a.navlink-sub')) as HTMLAnchorElement[];
const appLink = (title: string) => appLinks().find((a) => a.textContent?.trim() === title);
const sections = () => Array.from(host.querySelectorAll('aside nav[aria-label="CRM sections"] a')) as HTMLAnchorElement[];

win.localStorage.clear();
history.pushState(null, "", "/");
await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(App))); });

// (b) The Apps item lists the installed apps from the API on a non-app
// screen, each linking to its readable app-key route when unique.
await settle(() => assert(appLink("CRM") && appLink("Social Content"), "installed apps list from the API on Home"));
assert(listCalls > 0, "the list came from GET /api/app-installations");
equal(appLink("Social Content")!.getAttribute("href"), "/apps/social-content", "app links to its readable key");
assert(sections().length === 0, "no sections before any app was used");

// (a) Visit CRM on its Segments section; the sections stay on Settings.
await go("/apps/crm?crm=segments&ctx=crm-context&conversation=crm-conversation&keep=1");
await settle(() => assert(sections().length === 3, "CRM sections show on the CRM screen"));
assert(appLink("CRM")!.getAttribute("aria-current") === "true", "the open app is highlighted");
equal(appLink("Social Content")!.getAttribute("href"), "/apps/social-content?keep=1", "switching apps drops installation-scoped query but preserves unrelated state");
await go("/settings/email-sending");
await settle(() => assert(location.pathname.startsWith("/settings"), "on Settings"));
await settle(() => assert(sections().length === 3, "CRM sections stay visible on Settings"));

// (c) The last section is restored: the app link and the Segments row
// return to Segments, and a fresh mount (reload) remembers it.
equal(appLink("CRM")!.getAttribute("href"), "/apps/crm?crm=segments", "app link returns to the last section");
await React.act(async () => { root.unmount(); });
const root2 = createRoot(host);
history.pushState(null, "", "/");
await React.act(async () => { root2.render(React.createElement(LocaleProvider, null, React.createElement(App))); });
await settle(() => assert(sections().length === 3, "sections restored after reload"));
equal(appLink("CRM")!.getAttribute("href"), "/apps/crm?crm=segments", "last section survives a reload");
const clicked = appLink("CRM")!;
await React.act(async () => { clicked.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
await settle(() => assert(location.search.includes("crm=segments"), "clicking returns to CRM at Segments"));

// (b) A forged installation in the URL yields no entry and no sections.
await go("/app-installations/install-forged");
await settle(() => assert(host.textContent?.includes("unknown installation"), "forged installation refuses"));
assert(!appLink("install-forged") && appLinks().length === 2, "forged id adds no app entry");
assert(sections().length === 0, "forged id shows no sections");
await React.act(async () => { root2.unmount(); });

console.log("app nav persistence checks passed");
}
void main();
