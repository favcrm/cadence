import { streamFixture } from "./streamFixture";
export {};
/**
 * CAD-2286: the Documents screen is hosted only. Mounts the real App.
 *
 * - A native board never asks `/api/document-types`, never shows the nav
 *   item, and a direct `/documents` link shows the unavailable state, not a
 *   blank body or an error.
 * - A hosted board probes once when it mounts (App's effect and its routine
 *   refresh share the one answer), shows the nav item once the probe answers
 *   `available`, and mounts the screen on `/documents` and the type
 *   preferences on `/settings/documents` from that same answer.
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
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
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
  const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
  const { DOCUMENT_CAPABILITY } = require("../src/features/documents/documentsClient") as typeof import("../src/features/documents/documentsClient");
  const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");

  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const session = { id: "documents-session", origin: "loopback", created: 1700000000, last_used: 1700000000, idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test", user: { sub: "u-owner" } };
  let hosted = false;
  let requests: string[] = [];
  globalThis.fetch = (async (input: unknown) => {
    const path = String(input);
    requests.push(path);
    const url = new URL(path, "http://localhost");
    if (url.pathname.endsWith("/stream")) return streamFixture(path);
    if (path.startsWith("/api/meta"))
      return json({
        read_only: false, operator: true, signed_in: true, actor: "operator (ui)", session,
        ...(hosted ? { hosted: true, settings_sections: ["preferences", "members", "documents", "general", "status"] } : {}),
      });
    if (path === "/api/document-types")
      return json({ capability: DOCUMENT_CAPABILITY, baseline: { stages: [], requireApproval: true, approver: null, archiveAfterDays: 30, recipientNote: "" }, types: [] });
    if (path.startsWith("/api/documents?")) return json({ documents: [] });
    if (path === "/api/team/members") return json({ members: [] });
    if (path === "/api/issues" || path.startsWith("/api/issues?")) return json({ issues: [] });
    if (path === "/api/projects") return json({ projects: [] });
    if (path === "/api/agents") return json({ daemon: "unreachable", agents: [], totals: null });
    if (path === "/api/health") return json({ ok: true, pm_present: false, projects: 0, issues: 0, daemon: "stub", embedded: true });
    if (path === "/api/update/banner") return json(null);
    if (path.startsWith("/api/threads/master")) return json({ thread: null, entries: [], more_before: false });
    if (path === "/api/app-installations") return json([]);
    if (url.pathname.startsWith("/api/settings") || url.pathname.startsWith("/api/connections")) return json({});
    // Anything else is recorded and refused, never thrown: a throw here is swallowed.
    return json({ error: { code: "not_found", message: "not in this fixture" } }, 404);
  }) as typeof fetch;

  const host = document.createElement("div");
  document.body.append(host);
  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  async function settle(check: () => void, what: string) {
    let last: unknown;
    for (let i = 0; i < 80; i++) {
      await flush();
      try { check(); return; } catch (error) { last = error; }
    }
    throw new Error(`${what}: ${last instanceof Error ? last.message : String(last)}`);
  }
  const go = (path: string) => React.act(async () => { navigate(path); });
  const navItem = () => host.querySelector('aside nav[aria-label="Primary"] a[href="/documents"]');
  const projectTab = () => host.querySelector('nav.section-nav a[href="/projects/acme/documents"]');
  const probes = () => requests.filter((path) => path === "/api/document-types").length;

  // ---- a native board ----
  win.localStorage.clear();
  history.pushState(null, "", "/");
  let root = createRoot(host);
  await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(App))); });
  await settle(() => assert(host.querySelector('aside nav[aria-label="Primary"]'), "the nav renders"), "native nav");
  await settle(() => assert(requests.includes("/api/health"), "the routine refresh ran"), "native refresh");
  assert(!navItem(), "a native board shows no Documents nav item");
  await go("/projects/acme");
  await settle(() => assert(host.querySelector('nav.section-nav'), "the project section tabs render"), "native project tabs");
  assert(!projectTab(), "a native board shows no Documents project tab");
  await go("/documents");
  await settle(() => assert(host.textContent?.includes("Documents aren’t available here"), "a direct link shows the unavailable state"), "native direct link");
  await flush();
  equal(probes(), 0, "a native board never asks /api/document-types");
  assert(!requests.some((path) => path.startsWith("/api/documents")), "a native board never lists documents");
  assert(!navItem(), "still no Documents nav item");
  await React.act(async () => { root.unmount(); });

  // ---- a hosted board with the lifecycle ----
  resetPrivateSession();
  allowPrivateReads();
  hosted = true;
  requests = [];
  history.pushState(null, "", "/");
  root = createRoot(host);
  await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(App))); });
  await settle(() => assert(navItem(), "an available probe shows the Documents nav item"), "hosted nav");
  await settle(() => assert(requests.includes("/api/health"), "the routine refresh ran"), "hosted refresh");
  await flush();
  await flush();
  equal(probes(), 1, "the mount probes once");
  await go("/documents");
  await settle(() => assert(host.textContent?.includes("No documents to show"), "the screen mounts on /documents"), "hosted screen");
  assert(requests.includes("/api/documents?archived=0"), "the screen lists active documents");
  equal(probes(), 1, "opening the screen does not probe again");
  await go("/projects/acme");
  await settle(() => assert(projectTab(), "an available probe shows the Documents project section tab"), "hosted project tab");
  await go("/projects/acme/documents");
  await settle(() => assert(requests.includes("/api/documents?project=acme&archived=0"), "the project Documents tab lists project-scoped documents"), "hosted project documents");
  equal(probes(), 1, "opening project Documents reads the shared probe, not a second one");
  // Settings → Documents mounts where the other sections are, from the same probe.
  await go("/settings/documents");
  await settle(() => assert(host.textContent?.includes("Default for new types"), "Settings → Documents mounts the type preferences"), "settings documents");
  assert(host.querySelector('a[href="/settings/documents"]'), "the settings menu lists Documents");
  assert(requests.includes("/api/team/members"), "the page reads the roster for approver names");
  equal(probes(), 1, "the settings page reads the shared probe, not a second one");
  await React.act(async () => { root.unmount(); });

  console.log("documents app nav checks passed");
}

void main().catch((err: unknown) => {
  setTimeout(() => {
    throw err;
  }, 0);
});
