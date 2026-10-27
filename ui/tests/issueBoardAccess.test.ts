import { streamFixture } from "./streamFixture";
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
const { setSessionKey } = require("../src/lib/sessionKey") as typeof import("../src/lib/sessionKey");
const json = (data: unknown) => new Response(JSON.stringify(data), { headers: { "Content-Type": "application/json" } });
let releaseMeta!: (response: Response) => void;
let failMeta!: (error: Error) => void;
globalThis.fetch = async (input) => {
  const path = new URL(String(input), "http://localhost").pathname;
  if (path.endsWith("/stream")) return streamFixture(path);
  if (path === "/api/meta") return new Promise<Response>((resolve, reject) => { releaseMeta = resolve; failMeta = reject; });
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
const visible = (element: Element) => {
  for (let node: Element | null = element; node; node = node.parentElement)
    if (win.getComputedStyle(node).display === "none") return false;
  return true;
};
const writableMeta = (id: string) => ({ read_only: false, signed_in: true, operator: true, actor: "operator", session: { id, origin: "loopback", created: 1700000000, last_used: 1700000000, idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test", user: { sub: "operator", handle: "operator", name: "Operator", email: "operator@example.test", role: "operator" } } });
const refresh = async () => {
  await React.act(async () => window.dispatchEvent(new Event("focus")));
  await flush();
};
async function run() {
  setSessionKey("issue-board-access-test");
  await React.act(async () => root.render(React.createElement(App)));
  await flush();
  assert(releaseMeta, "metadata read started");
  assert(!canCreate(), "creation stays hidden while write access is unknown");
  assert(!host.querySelector("article, aside, form.board-new-issue"), "initial verification mounts no private board or creation form");
  assert(host.querySelector("[data-auth-state='checking'] h1"), "initial checking is a public auth state, not a private write-access placeholder");
  await React.act(async () => releaseMeta(json(writableMeta("sample"))));
  await flush();
  assert(canCreate(), "creation appears after this tab is proven writable");
  assert(!host.querySelector("article")?.getAttribute("title"), "authorized board cards no longer show an access warning");
  const openCreate = () => Array.from(host.querySelectorAll("button")).find((item) => item.textContent?.trim() === "New issue")?.click();
  await React.act(openCreate);
  const title = host.querySelector('form.board-new-issue input[name="title"]') as HTMLInputElement | null;
  assert(title, "create form opens");
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(title, "A draft to keep");
    title.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await refresh();
  await React.act(async () => failMeta(new Error("temporary metadata failure")));
  await flush();
  assert(host.querySelector('form.board-new-issue input[name="title"]') === title && title.value === "A draft to keep", "transient metadata failure preserves the open draft");
  assert(host.querySelector("[data-auth-state='unavailable'] h1"), "unknown identity is blocked by the public auth boundary");
  assert(!visible(title), "preserved issue draft is hidden and cannot be submitted while identity is unknown");
  await refresh();
  await React.act(async () => releaseMeta(json(writableMeta("sample"))));
  await flush();
  assert(host.querySelector('form.board-new-issue input[name="title"]') === title && visible(title), "same-session recovery restores the editable draft");
  await React.act(async () => setSessionKey("another-session-key"));
  await refresh();
  assert(!host.querySelector("form.board-new-issue"), "credential change clears the prior draft before the next metadata response");
  await React.act(async () => releaseMeta(json(writableMeta("another-session"))));
  await flush();
  assert(!host.querySelector("form.board-new-issue") && canCreate(), "a different session cannot inherit the draft");
  await React.act(openCreate);
  assert((host.querySelector('form.board-new-issue input[name="title"]') as HTMLInputElement)?.value === "", "new session starts a blank issue");
  await React.act(async () => root.unmount());
  await win.happyDOM.close();
  console.log("issueBoardAccess: pending access, draft recovery and session isolation passed");
}
run().catch(async (error) => { await React.act(async () => root.unmount()); await win.happyDOM.close(); console.error(error); throw error; });
export {};
