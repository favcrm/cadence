import { streamFixture } from "./streamFixture";
export {};
/**
 * CAD-1189: a failed or busy refresh of the installed-app list keeps the
 * last good list in the sidebar, says so, and a retry (manual, or automatic
 * for a busy daemon) restores a clean state. Mounts the real App.
 */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
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
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const AppComponent = (require("../src/App") as typeof import("../src/App")).default;
const App = () => React.createElement(LocaleProvider, null, React.createElement(AppComponent));

const mk = (id: string, name: string, title: string) => ({
  install_id: id, title, name, version: "0.1.0",
  summary: "", digest: "install-digest", catalog_generation: "gen-1", approved: true,
  storage_kind: "workspace", project_link: null, executable: true,
  approval: { state: "approved" }, guide: "", files: [], capabilities: null, connection_slots: [],
});
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
let mode: "ok" | "fail" | "busy-once" = "ok";
let operator: boolean | null = true;
let metaUnavailable = false;
let listCalls = 0;
globalThis.fetch = (async (input: unknown) => {
  const path = String(input);
  const url = new URL(path, "http://localhost");
  if (url.pathname.endsWith("/stream")) return streamFixture(path);
  if (path.startsWith("/api/meta") && metaUnavailable) return json({ error: "Verification unavailable" }, 503);
  if (path.startsWith("/api/meta")) return json({ read_only: false, operator, signed_in: true, actor: "operator (ui)", session: { id: "sidebar-session", origin: "loopback", created: 1700000000, last_used: 1700000000, idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test" } });
  if (path === "/api/issues" || path.startsWith("/api/issues?")) return json({ issues: [] });
  if (path === "/api/projects") return json({ projects: [] });
  if (path === "/api/agents") return json({ daemon: "unreachable", agents: [], totals: null });
  if (path === "/api/health") return json({ ok: true, pm_present: false, projects: 0, issues: 0, daemon: "stub", embedded: true });
  if (path === "/api/update/banner") return json(null);
  if (path.startsWith("/api/threads/master")) return json({ thread: null, entries: [], more_before: false });
  if (path === "/api/app-installations") {
    listCalls += 1;
    if (!operator) return json({ error: "operator only" }, 403);
    if (mode === "fail") return json({ error: "list exploded" }, 500);
    if (mode === "busy-once") { mode = "ok"; return json({ error: "another live writer holds the lock", code: "resource_busy" }, 503); }
    return json([mk("install-crm", "crm", "CRM"), mk("install-soc", "social-content", "Social Content")]);
  }
  if (path === "/api/app-installations/install-crm") return json(mk("install-crm", "crm", "CRM"));
  if (path === "/api/app-installations/install-crm/contexts") return json({ contexts: [] });
  if (path.startsWith("/api/app-installations/install-forged")) return json({ error: "unknown installation" }, 404);
  if (url.pathname.endsWith("/records")) return json({ records: [], truncated: false, next_cursor: null });
  if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
  if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
  if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
  if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: [] });
  if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: [] });
  if (url.pathname === "/api/wiki/ls") return json({ path: url.searchParams.get("path") ?? "", kind: "dir", entries: [] });
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

win.localStorage.clear();
history.pushState(null, "", "/");
await React.act(async () => { root.render(React.createElement(App)); });
const notice = () => host.querySelector('aside nav[aria-label="Primary"] [role="status"]');
const retryButton = () => Array.from(host.querySelectorAll('aside nav[aria-label="Primary"] [role="status"] button')).find((b) => b.textContent?.trim() === "Retry") as HTMLButtonElement | undefined;

await settle(() => assert(appLink("CRM") && appLink("Social Content"), "apps list on Home"));
assert(!notice(), "no notice while healthy");

// A failed refresh keeps the last good list and shows a retry notice.
mode = "fail";
await go("/settings/email-sending");
await settle(() => assert(notice() && retryButton(), "failed refresh shows a notice with Retry"));
assert(notice()!.textContent!.includes("list exploded"), "the notice carries the error");
assert(appLink("CRM") && appLink("Social Content"), "the last good list stays");

// Manual retry restores a clean state.
mode = "ok";
await React.act(async () => { retryButton()!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
await settle(() => assert(!notice(), "notice clears after a good retry"));
assert(appLink("CRM") && appLink("Social Content"), "list intact after retry");

// A busy daemon is retried automatically, with the notice while waiting.
mode = "busy-once";
await go("/");
await settle(() => assert(notice()?.textContent?.includes("busy") && !retryButton(), "busy shows a retrying notice without a manual button"));
assert(appLink("CRM"), "the last good list stays while retrying");
for (let i = 0; i < 40 && notice(); i++) await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 100)); });
assert(!notice(), "the automatic retry recovered");
assert(appLink("CRM") && appLink("Social Content"), "list intact after the automatic retry");
await React.act(async () => { root.unmount(); });

// A non-operator session never asks for the list and shows no notice.
operator = false;
listCalls = 0;
const host2 = document.createElement("div");
document.body.append(host2);
const root2 = createRoot(host2);
await React.act(async () => { root2.render(React.createElement(App)); });
for (let i = 0; i < 10; i++) await flush();
assert(listCalls === 0, "a non-operator session does not load the installation list");
assert(!host2.querySelector('aside nav[aria-label="Primary"] [role="status"]'), "no installation notice for a non-operator");
await React.act(async () => { root2.unmount(); });

// A completed workspace recheck exposes an actionable retry in the mobile
// menu too; opening that menu must not restart the shared wait.
operator = null;
let finishRecovery: (() => void) | undefined;
const nativeTimeout = win.setTimeout.bind(win);
win.setTimeout = (callback: (...args: unknown[]) => void, delay?: number, ...args: unknown[]) => {
  if (delay === 60_000) {
    finishRecovery = () => callback(...args);
    return nativeTimeout(() => undefined, 0);
  }
  return nativeTimeout(callback, delay, ...args);
};
let reloads = 0;
Object.defineProperty(win.location, "reload", { value: () => { reloads++; }, configurable: true });
const root3 = createRoot(host2);
await React.act(async () => { root3.render(React.createElement(App)); });
await settle(() => assert(finishRecovery, "workspace recheck has started"));
await React.act(async () => { finishRecovery!(); });
const menu = host2.querySelector<HTMLButtonElement>('button[aria-controls="mobile-navigation"]');
assert(menu, "mobile navigation can be opened");
await React.act(async () => { menu.click(); });
const workspaceRetry = Array.from(host2.querySelectorAll<HTMLButtonElement>("#mobile-navigation button")).find((button) => button.textContent?.trim() === "Retry workspace");
assert(workspaceRetry && !workspaceRetry.disabled, "completed recovery is actionable after opening the mobile menu");
await React.act(async () => { workspaceRetry.click(); });
assert(reloads === 1, "mobile workspace retry performs the recovery reload");
await React.act(async () => { root3.unmount(); });
win.setTimeout = nativeTimeout;

// Recovering the real Wiki route returns keyboard focus to the workspace,
// even when it has no page heading or main landmark to receive it.
operator = true;
mode = "ok";
metaUnavailable = true;
history.replaceState(null, "", "/wiki/upload/global/guides");
const root4 = createRoot(host2);
await React.act(async () => { root4.render(React.createElement(App)); });
await settle(() => assert(host2.querySelector('[data-auth-state="unavailable"]'), "initial verification is unavailable"));
const initialVerificationRetry = Array.from(host2.querySelectorAll<HTMLButtonElement>("button")).find((button) => button.textContent?.trim() === "Retry verification");
assert(initialVerificationRetry, "verification can be retried");
metaUnavailable = false;
await React.act(async () => { initialVerificationRetry.click(); });
await settle(() => assert(host2.querySelector('[data-auth-state="ready"]'), "Wiki is admitted after retry"));
const workspace = host2.querySelector<HTMLElement>("[data-app-shell]");
assert(workspace?.contains(document.activeElement), "verification recovery returns keyboard focus to the admitted workspace");
const chooser = Array.from(host2.querySelectorAll<HTMLButtonElement>("button")).find((button) => button.textContent?.trim() === "Choose files");
assert(chooser, "the admitted Wiki offers file selection");
chooser.focus();
metaUnavailable = true;
await React.act(async () => { win.dispatchEvent(new Event("focus")); });
await settle(() => assert(host2.querySelector('[data-auth-state="unavailable"]'), "a later verification failure conceals Wiki"));
const subsequentVerificationRetry = Array.from(host2.querySelectorAll<HTMLButtonElement>("button")).find((button) => button.textContent?.trim() === "Retry verification");
assert(subsequentVerificationRetry, "the same session can be retried");
metaUnavailable = false;
await React.act(async () => { subsequentVerificationRetry.click(); });
await settle(() => assert(document.activeElement === chooser, "same-session recovery restores the previously focused Wiki control"));
await React.act(async () => { root4.unmount(); });
console.log("sidebar busy checks passed");
}
void main();
