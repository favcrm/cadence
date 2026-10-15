/**
 * CAD-1351/CAD-1452 acceptance: unknown workspace access never grants
 * visible private authority or clears the tab key. Exercise actual App
 * outage, bounded verification, retry and recovery behavior.
 */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/projects/cadence/issues?view=kanban" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
Object.defineProperty(globalThis, "setInterval", { value: () => 1, configurable: true });
Object.defineProperty(globalThis, "clearInterval", { value: () => undefined, configurable: true });
Object.defineProperty(globalThis, "EventSource", { value: class { readyState = 0; addEventListener() {} close() {} }, configurable: true });
// Drive the boundary's eight-second verification bound without wall-clock waits.
const realSetTimeout = win.setTimeout.bind(win);
const realClearTimeout = win.clearTimeout.bind(win);
let fakeId = 1_000_000;
const fakeIds = new Map<number, () => void>();
win.setTimeout = (fn: () => void, ms?: number, ...rest: unknown[]) => {
  if (ms === 8_000) {
    const id = ++fakeId;
    fakeIds.set(id, fn);
    return id;
  }
  return realSetTimeout(fn, ms, ...rest);
};
win.clearTimeout = (id: number) => {
  if (fakeIds.delete(id)) return;
  realClearTimeout(id);
};
let reloads = 0;
Object.defineProperty(win.location, "reload", { value: () => { reloads += 1; }, configurable: true });
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
const AppView = (require("../src/App") as typeof import("../src/App")).default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
// main.tsx renders <LocaleProvider><App /></LocaleProvider>; the test mounts the same tree (CAD-1331).
const App = () => React.createElement(LocaleProvider, null, React.createElement(AppView));
const { sessionKey, setSessionKey } = require("../src/lib/sessionKey") as typeof import("../src/lib/sessionKey");
const json = (data: unknown) => new Response(JSON.stringify(data), { headers: { "Content-Type": "application/json" } });
const calls: { path: string; method: string }[] = [];
let releaseMeta: ((response: Response) => void) | null = null;
let failMeta: ((error: Error) => void) | null = null;
globalThis.fetch = async (input, init) => {
  const path = new URL(String(input), "http://localhost").pathname;
  calls.push({ path, method: init?.method ?? "GET" });
  if (path.endsWith("/stream")) return new Response(new ReadableStream(), { headers: { "Content-Type": "text/event-stream" } });
  if (path === "/api/meta") return new Promise<Response>((resolve, reject) => {
    releaseMeta = resolve;
    failMeta = reject;
    init?.signal?.addEventListener("abort", () => reject(new Error("verification aborted")), { once: true });
  });
  if (path === "/api/health") return json({ daemon: "reachable" });
  if (path === "/api/issues") return json({ issues: [{
    id: "CAD-1", project: "cadence", title: "Wake check", status: "backlog", status_source: "file", priority: "P2",
    blocked_by: [], relates: [], refs: [], container: false, ready: true, blocked: false,
    created: "2026-10-11", rev: "fixture", counts: { comments: 0, artifacts: 0, refs: 0 }, checks: { done: 0, total: 0 },
  }] });
  if (path === "/api/projects") return json({ projects: [{ key: "cadence", prefix: "CAD", components: [], repos: [], issues: 0 }] });
  if (path === "/api/agents") return json({ agents: [], by_issue: {}, daemon: "reachable" });
  if (path === "/api/update/banner") return json(null);
  return json({});
};
function assert(value: unknown, what: string): asserts value { if (!value) throw new Error(what); }
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = async () => { for (let i = 0; i < 3; i++) await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); }); };
const text = () => host.textContent ?? "";
const visible = (element: Element) => {
  for (let node: Element | null = element; node; node = node.parentElement)
    if (win.getComputedStyle(node).display === "none") return false;
  return true;
};
const buttons = (label: string) => Array.from(host.querySelectorAll("button")).filter((item) => visible(item) && item.textContent?.trim() === label);
const canCreate = () => buttons("New issue").length > 0;
const signedOutPill = () => Array.from(host.querySelectorAll('[aria-label="Read-only. Sign in to make changes"]')).some(visible);
const writableMeta = { read_only: false, signed_in: true, operator: true, actor: "operator", session: { id: "s1", origin: "loopback", created: 1700000000, last_used: 1700000000, idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test", user: { sub: "operator", handle: "operator", name: "Operator", email: "operator@example.test", role: "operator" } } };
const KEY = "workspace-wake-access-test";
const refresh = async () => {
  await React.act(async () => window.dispatchEvent(new Event("focus")));
  await flush();
};
const assertUnknownGrantsNothing = (what: string) => {
  assert(host.querySelector("[data-auth-state='checking'] h1, [data-auth-state='unavailable'] h1"), `${what}: unknown access has a public verification state`);
  assert(!canCreate(), `${what}: unknown access offers no write control`);
  assert(!signedOutPill(), `${what}: unknown access is not shown as signed out`);
  assert(!text().includes("Sign in to send messages"), `${what}: no sign-in flow for unknown access`);
  assert(sessionKey() === KEY, `${what}: the tab's session key is kept`);
};
async function run() {
  setSessionKey(KEY);
  await React.act(async () => root.render(React.createElement(App)));
  await flush();
  // Positive control: proven authority shows the write control.
  assert(releaseMeta, "metadata read started");
  await React.act(async () => releaseMeta!(json(writableMeta)));
  await flush();
  assert(canCreate(), "positive control: a proven writable operator sees New issue");
  assert(host.querySelector("[data-auth-state='ready']"), "positive control: verification admits the workspace");

  // The workspace becomes unreachable: the check cannot answer.
  await refresh();
  await React.act(async () => failMeta!(new Error("board relay unreachable")));
  await flush();
  assertUnknownGrantsNothing("after a failed check");
  assert(buttons("Retry verification").length === 1, "unavailable verification offers an explicit retry");

  // A re-check in flight must not resurrect the earlier authority.
  await refresh();
  assertUnknownGrantsNothing("while the re-check is in flight");
  await React.act(async () => failMeta!(new Error("still unreachable")));
  await flush();
  assertUnknownGrantsNothing("after a second failed check");

  // Retry verifies, never reloads, and grants nothing before an answer.
  const retry = buttons("Retry verification");
  assert(retry.length === 1, "unavailable state offers one verification retry");
  const before = calls.length;
  await React.act(async () => { retry[0].click(); });
  await flush();
  assert(reloads === 0, "verification retry does not reload away in-memory drafts");
  const made = calls.slice(before);
  assert(made.length === 1 && made[0].method === "GET" && made[0].path === "/api/meta", `Retry issues exactly one metadata probe, no private read or write: ${JSON.stringify(made)}`);
  assertUnknownGrantsNothing("after Retry");
  assert(fakeIds.size > 0, "verification has an eight-second bound");
  await React.act(async () => { for (const [id, fn] of Array.from(fakeIds)) { fakeIds.delete(id); fn(); } });
  await flush();
  assertUnknownGrantsNothing("after bounded verification times out");
  assert(host.querySelector("[data-auth-state='unavailable'] h1"), "timed-out verification remains unknown, never signed out");

  // A later answered check restores only what the server proves.
  await refresh();
  await React.act(async () => releaseMeta!(json(writableMeta)));
  await flush();
  assert(canCreate() && host.querySelector("[data-auth-state='ready']"), "an answered check restores only the proven authority");

  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.log("workspaceWakeAccess: unknown access grants nothing through re-check and Retry");
}
run().catch(async (error) => { await React.act(async () => root.unmount()); await win.happyDOM.close(); console.error(error); throw error; });
export {};
