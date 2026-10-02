export {};
/**
 * CAD-784 host App menu: Apps → CRM → Customers/Segments/Campaigns
 * nested in the actual shared surfaces — the desktop `aside`
 * sidebar and the mobile `#mobile-navigation` — for a verified CRM
 * installation, and absent otherwise. Mounts the real App.
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
const outlet = require("../src/features/app-shell/CrmOutlet") as typeof import("../src/features/app-shell/CrmOutlet");
// Pure menu derivation: titles, hrefs and current flags from the
// route href alone.
const menu = outlet.crmAppMenu("/app-installations/install-crm?ctx=ctx-a", "CRM", "segments");
equal(menu.title, "CRM", "menu carries the verified installation title");
equal(
  menu.sections.map((s) => [s.label, s.href, s.current]),
  [
    ["Customers", "/app-installations/install-crm?ctx=ctx-a", false],
    ["Segments", "/app-installations/install-crm?ctx=ctx-a&crm=segments", true],
    ["Campaigns", "/app-installations/install-crm?ctx=ctx-a&crm=campaigns", false],
  ],
  "menu sections link with context and mark current",
);
equal(
  outlet.crmAppMenu("/app-installations/install-crm?ctx=ctx-a&crm=campaigns&record=r1", "CRM", "campaigns").sections[2],
  { label: "Campaigns", href: "/app-installations/install-crm?ctx=ctx-a&crm=campaigns", current: true },
  "menu links drop the record view",
);

const crmInstall = {
  install_id: "install-crm", title: "CRM", name: "crm", version: "0.1.0",
  summary: "", digest: "install-digest", catalog_generation: "gen-1", approved: true,
  storage_kind: "workspace", project_link: null, executable: true,
  approval: { state: "approved" }, guide: "", files: [], capabilities: null, connection_slots: [],
};
const notesInstall = {
  install_id: "install-notes", title: "Notes", name: "notes", version: "0.1.0",
  summary: "", digest: "install-digest", catalog_generation: "gen-1", approved: true,
  storage_kind: "workspace", project_link: null, executable: true,
  approval: { state: "approved" }, guide: "", files: [], capabilities: null, connection_slots: [],
};
const seed = {
  id: "customer-s1", install_id: "install-crm", context_id: "ctx-a", kind: "customer",
  revision: 1, digest: "sha256:seed",
  profile: { schema: 1, display_name: "Seed Alpha", email: "seed-alpha@example.com", tags: ["vip"], consent: { email: "granted", sms: "unknown" } },
  history: [{ revision: 1, digest: "sha256:seed", actor: "operator", at: 1759113600 }],
  consent_history: [{ revision: 1, channel: "email", state: "granted", actor: "operator", at: 1759113600 }],
};
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown) => {
  const path = String(input);
  const url = new URL(path, "http://localhost");
  if (path.startsWith("/api/meta")) {
    return json({ read_only: false, operator: true, signed_in: true, actor: "operator (ui)", session: null });
  }
  if (path === "/api/issues" || path.startsWith("/api/issues?")) return json({ issues: [] });
  if (path === "/api/projects") return json({ projects: [] });
  if (path === "/api/agents") return json({ daemon: "unreachable", agents: [], totals: null });
  if (path === "/api/health") return json({ ok: true, pm_present: false, projects: 0, issues: 0, daemon: "stub", embedded: true });
  if (path === "/api/update/banner") return json(null);
  if (path.startsWith("/api/threads/master")) return json({ thread: null, entries: [], more_before: false });
  if (path === "/api/app-installations/install-crm") return json(crmInstall);
  if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [
    { id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
  ] });
  if (path === "/api/app-installations/install-notes") return json(notesInstall);
  if (path === "/api/app-installations/install-notes/contexts") return json({ contexts: [] });
  if (path === "/api/app-installations/install-nope" || path === "/api/app-installations/install-nope/contexts") {
    return json({ error: "unknown installation" }, 404);
  }
  if (url.pathname.endsWith("/records")) return json({ records: [seed], truncated: false, next_cursor: null });
  if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
  if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
  if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
  if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: [] });
  if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: [] });
  throw new Error(`Unexpected read ${path}`);
}) as typeof fetch;

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
const text = () => host.textContent ?? "";
async function settle(check: () => void) {
  for (let i = 0; i < 60; i++) {
    await flush();
    try { check(); return; } catch { /* keep polling */ }
  }
  await flush();
  check();
}
async function click(element: Element | undefined | null) {
  assert(element, "click target exists");
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await flush();
}
const sidebarMenu = () => host.querySelector('aside nav[aria-label="CRM sections"]');
const sidebarLinks = () => Array.from(sidebarMenu()?.querySelectorAll("a") ?? []);

win.sessionStorage.clear();
history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a");
await React.act(async () => {
  root.render(React.createElement(App));
});
// The verified CRM installation nests its sections in the desktop
// sidebar: real links, context kept, current section marked.
await settle(() => assert(sidebarMenu(), "desktop sidebar nests the CRM submenu"));
await settle(() => assert(text().includes("Seed Alpha"), "outlet still paints server rows"));
assert(sidebarLinks().length === 3, "sidebar submenu lists three sections");
assert(!host.querySelector(".app-outlet .section-nav"), "CRM pages have no duplicate outlet tabs");
assert(!host.querySelector(".crm-crumb"), "CRM pages have no redundant breadcrumb row");
assert(
  (sidebarLinks()[1] as HTMLAnchorElement).getAttribute("href") === "/app-installations/install-crm?ctx=ctx-a&crm=segments",
  "sidebar segment link keeps the selected context",
);
assert(
  sidebarMenu()!.querySelector('a[aria-current="page"]')?.textContent?.trim() === "Customers",
  "sidebar marks the current section",
);
// Apps > CRM > children: the verified group nests immediately under
// the Apps item, ahead of the next top-level item (Agents).
const primaryNav = () => host.querySelector('aside > nav[aria-label="Primary"]');
assert(primaryNav(), "desktop primary nav renders");
const primaryKids = () => Array.from(primaryNav()?.children ?? []);
const appsIdx = primaryKids().findIndex(
  (el) => el.tagName === "A" && (el.textContent ?? "").trim() === "Apps",
);
assert(appsIdx >= 0, "Apps item renders in the primary nav");
const nestedGroup = primaryKids()[appsIdx + 1];
assert(
  nestedGroup?.tagName === "DIV" && nestedGroup.querySelector('nav[aria-label="CRM sections"]'),
  "CRM group nests immediately under Apps",
);
assert(
  (primaryKids()[appsIdx + 2]?.textContent ?? "").trim() === "Agents",
  "Agents follows the CRM group",
);
// A sidebar move routes, keeps the single chat node, and follows state.
const chatBefore = host.querySelector("[data-chat-pane]");
assert(chatBefore, "chat pane mounts with the shell");
await click(sidebarLinks()[1]);
await settle(() => assert(location.search.includes("crm=segments"), "sidebar link routes"));
assert(host.querySelector("[data-chat-pane]") === chatBefore, "sidebar move keeps the single chat node mounted");
assert(
  sidebarMenu()!.querySelector('a[aria-current="page"]')?.textContent?.trim() === "Segments",
  "sidebar follows the route",
);
// The phone menu nests the same submenu with the same current state.
await click(host.querySelector('button[aria-controls="mobile-navigation"]'));
await settle(() => assert(host.querySelector("#mobile-navigation"), "phone menu opens"));
const phoneLinks = () =>
  Array.from(host.querySelectorAll("#mobile-navigation a")).filter((el) =>
    ["Customers", "Segments", "Campaigns"].includes((el.textContent ?? "").trim()),
  );
assert(phoneLinks().length === 3, "phone menu nests the CRM submenu");
// Same hierarchy on phone: the CRM group follows the Apps cell
// inside the main grid, ahead of Agents.
const phoneGrid = () => host.querySelector('#mobile-navigation > nav[aria-label="Workspace"]');
assert(phoneGrid(), "phone main list renders");
const phoneKids = () => Array.from(phoneGrid()?.children ?? []);
const phoneAppsIdx = phoneKids().findIndex(
  (el) => el.tagName === "A" && (el.textContent ?? "").trim() === "Apps",
);
assert(phoneAppsIdx >= 0, "Apps cell renders in the phone list");
const phoneGroup = phoneKids()[phoneAppsIdx + 1];
assert(
  phoneGroup?.tagName === "DIV" &&
    phoneGroup.querySelector('nav[aria-label="CRM sections"]')?.querySelectorAll("a").length === 3,
  "CRM group nests immediately under Apps on phone",
);
assert(
  (phoneKids()[phoneAppsIdx + 2]?.textContent ?? "").trim() === "Agents",
  "Agents follows the CRM group on phone",
);
assert(
  phoneLinks().find((el) => el.getAttribute("aria-current") === "page")?.textContent?.trim() === "Segments",
  "phone menu marks the current section",
);
await click(phoneLinks().find((el) => (el.textContent ?? "").trim() === "Campaigns"));
await settle(() => assert(location.search.includes("crm=campaigns"), "phone link routes"));
assert(!host.querySelector("#mobile-navigation"), "phone link closes the menu");
// Absence: a non-CRM installation shows no CRM submenu anywhere.
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
await React.act(async () => {
  navigate("/app-installations/install-notes");
});
await settle(() => assert(text().includes("No records yet"), "generic outlet renders"));
assert(!host.querySelector('aside nav[aria-label="CRM sections"]'), "no sidebar submenu for a non-CRM app");
await click(host.querySelector('button[aria-controls="mobile-navigation"]'));
await settle(() => assert(host.querySelector("#mobile-navigation"), "phone menu opens off-CRM"));
assert(
  Array.from(host.querySelectorAll("#mobile-navigation a")).filter((el) =>
    ["Customers", "Segments", "Campaigns"].includes((el.textContent ?? "").trim()),
  ).length === 0,
  "no phone submenu for a non-CRM app",
);
// Absence: an unverified installation (failed receipt) shows none either.
await React.act(async () => {
  navigate("/app-installations/install-nope?ctx=ctx-a");
});
await settle(() => assert(text().includes("unknown installation"), "unknown installation refuses"));
assert(!host.querySelector('aside nav[aria-label="CRM sections"]'), "no sidebar submenu without a verified receipt");
await React.act(async () => { root.unmount(); });

console.log("crm app menu checks passed");
}
void main();
