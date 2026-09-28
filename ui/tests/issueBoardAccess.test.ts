/** A new issue cannot be offered before the board knows this tab's access. */
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
const { setSessionKey } = require("../src/lib/sessionKey") as typeof import("../src/lib/sessionKey");
const json = (data: unknown) => new Response(JSON.stringify(data), { headers: { "Content-Type": "application/json" } });
let releaseMeta!: (response: Response) => void;
globalThis.fetch = async (input) => {
  const path = new URL(String(input), "http://localhost").pathname;
  if (path === "/api/meta") return new Promise<Response>((resolve) => { releaseMeta = resolve; });
  if (path === "/api/health") return json({ daemon: "reachable" });
  if (path === "/api/issues") return json({ issues: [{
    id: "CAD-1", project: "cadence", title: "Review access copy", status: "backlog", status_source: "file", priority: "P2",
    blocked_by: [], relates: [], refs: [], container: false, ready: true, blocked: false,
    created: "2026-09-28", rev: "fixture", counts: { comments: 0, artifacts: 0, refs: 0 }, checks: { done: 0, total: 0 },
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
const canCreate = () => Array.from(host.querySelectorAll("button")).some((item) => item.textContent?.trim() === "New issue");
async function run() {
  setSessionKey("issue-board-access-test");
  await React.act(async () => root.render(React.createElement(App)));
  await flush();
  assert(releaseMeta, "metadata read started");
  assert(!canCreate(), "creation stays hidden while write access is unknown");
  const card = host.querySelector("article");
  assert(card, "board card renders while access is checked");
  assert(card.getAttribute("title")?.includes("Checking write access"), "pending access explains why dragging is disabled");
  await React.act(async () => releaseMeta(json({ read_only: false, signed_in: true, operator: true, actor: "operator", session: { id: "sample", user: { name: "Operator", email: "operator@example.test", role: "operator" } } })));
  await flush();
  assert(canCreate(), "creation appears after this tab is proven writable");
  assert(!host.querySelector("article")?.getAttribute("title"), "authorized board cards no longer show an access warning");
  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.log("issueBoardAccess: pending access and writable reveal passed");
}
run().catch(async (error) => { await React.act(async () => root.unmount()); await win.happyDOM.close(); console.error(error); throw error; });
export {};
