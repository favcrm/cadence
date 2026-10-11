/**
 * CAD-1351 independent acceptance check (reviewer-authored; the
 * implementer may not edit or weaken it). Bad case: while the access
 * check cannot reach the workspace, unknown access is never rendered as
 * signed out or as affirmative authority, and the bounded re-check and
 * the "Retry workspace" reload grant nothing.
 *
 * Mounts the real App (same rig as issueBoardAccess.test.ts) so the real
 * refresh/metaProbe guard and the real SignIn chip are exercised:
 *  - positive control: a proven writable operator sees "New issue";
 *  - the metadata read then fails: the waking/unreachable chip shows,
 *    "New issue" is gone, no signed-out pill renders, the tab key stays;
 *  - a re-check in flight keeps the same state (no stale authority);
 *  - Retry is withheld during the 60 s window and offered after it;
 *  - Retry performs exactly one page reload and issues no write, no
 *    extra metadata read, and enables nothing;
 *  - a later answered check restores the proven state.
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
// Capture only the 60 s recheck window; every other timer stays real.
const realSetTimeout = win.setTimeout.bind(win);
const realClearTimeout = win.clearTimeout.bind(win);
let fakeId = 1_000_000;
const fakeIds = new Map<number, () => void>();
win.setTimeout = (fn: () => void, ms?: number, ...rest: unknown[]) => {
  if (ms === 60_000) {
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
const App = (require("../src/App") as typeof import("../src/App")).default;
const { sessionKey, setSessionKey } = require("../src/lib/sessionKey") as typeof import("../src/lib/sessionKey");
const json = (data: unknown) => new Response(JSON.stringify(data), { headers: { "Content-Type": "application/json" } });
const calls: { path: string; method: string }[] = [];
let releaseMeta: ((response: Response) => void) | null = null;
let failMeta: ((error: Error) => void) | null = null;
globalThis.fetch = async (input, init) => {
  const path = new URL(String(input), "http://localhost").pathname;
  calls.push({ path, method: init?.method ?? "GET" });
  if (path === "/api/meta") return new Promise<Response>((resolve, reject) => { releaseMeta = resolve; failMeta = reject; });
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
const buttons = (label: string) => Array.from(host.querySelectorAll("button")).filter((item) => item.textContent?.trim() === label);
const canCreate = () => buttons("New issue").length > 0;
const signedOutPill = () => host.querySelector('[aria-label="Read-only. Sign in to make changes"]');
const writableMeta = { read_only: false, signed_in: true, operator: true, actor: "operator", session: { id: "s1", user: { name: "Operator", email: "operator@example.test", role: "operator" } } };
const KEY = "workspace-wake-access-test";
const refresh = async () => {
  const control = host.querySelector('[aria-label="Refresh board"]');
  assert(control, "refresh control exists");
  await React.act(() => control.dispatchEvent(new MouseEvent("click", { bubbles: true })));
  await flush();
};
const assertUnknownGrantsNothing = (what: string) => {
  assert(text().includes("Workspace waking or unreachable"), `${what}: the waking/unreachable state is shown`);
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
  assert(!text().includes("Workspace waking"), "positive control: no waking state");

  // The workspace becomes unreachable: the check cannot answer.
  await refresh();
  await React.act(async () => failMeta!(new Error("board relay unreachable")));
  await flush();
  assertUnknownGrantsNothing("after a failed check");
  assert(buttons("Retry workspace").length === 0, "Retry is withheld during the 60 s re-check window");

  // A re-check in flight must not resurrect the earlier authority.
  await refresh();
  assertUnknownGrantsNothing("while the re-check is in flight");
  await React.act(async () => failMeta!(new Error("still unreachable")));
  await flush();
  assertUnknownGrantsNothing("after a second failed check");

  // The 60 s window ends: Retry is offered; it reloads and grants nothing.
  assert(fakeIds.size > 0, "the bounded re-check window is armed");
  await React.act(async () => { for (const [id, fn] of Array.from(fakeIds)) { fakeIds.delete(id); fn(); } });
  await flush();
  const retry = buttons("Retry workspace");
  assert(retry.length > 0, "Retry workspace is offered after the window");
  const before = calls.length;
  await React.act(async () => { retry[0].click(); });
  await flush();
  assert(reloads === 1, `Retry performs exactly one page reload (got ${reloads})`);
  const made = calls.slice(before);
  assert(!made.some((c) => c.method !== "GET"), `Retry issues no write: ${JSON.stringify(made)}`);
  assert(!made.some((c) => c.path === "/api/meta"), `Retry does not stack an extra metadata read: ${JSON.stringify(made)}`);
  assertUnknownGrantsNothing("after Retry");

  // A later answered check restores only what the server proves.
  await refresh();
  await React.act(async () => releaseMeta!(json(writableMeta)));
  await flush();
  assert(canCreate() && !text().includes("Workspace waking"), "an answered check restores the proven state");

  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.log("workspaceWakeAccess: unknown access grants nothing through re-check and Retry");
}
run().catch(async (error) => { await React.act(async () => root.unmount()); await win.happyDOM.close(); console.error(error); throw error; });
export {};
