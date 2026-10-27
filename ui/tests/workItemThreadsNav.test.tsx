/**
 * CAD-1811 review regression: the shared detail on Threads is an overlay on
 * the Threads URL. Closing it restores the conversation and every list
 * selector, and the scoped list request keeps its project.
 */
import { streamFixture } from "./streamFixture";
import { readThreadsUrl } from "../src/features/threads/threadsView";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const scope = "?c=conv-1&project=cadence&agent=cc-1&kind=agents&system=1";
  const { Window } = require("happy-dom");
  const win = new Window({ url: `http://localhost/threads${scope}&issue=D-5` });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "setInterval", { value: () => 1, configurable: true });
  Object.defineProperty(globalThis, "clearInterval", { value: () => undefined, configurable: true });
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
  const { setSessionKey } = require("../src/lib/sessionKey") as typeof import("../src/lib/sessionKey");

  const json = (data: unknown, status = 200) =>
    new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json" } });
  const meta = {
    read_only: false, signed_in: true, operator: true, actor: "operator",
    session: {
      id: "threads-nav", origin: "loopback", created: 1700000000, last_used: 1700000000,
      idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test",
      user: { sub: "operator", handle: "operator", name: "Operator", email: "operator@example.test", role: "operator" },
    },
  };
  const detail = {
    id: "D-5", project: "demo", title: "Ship the queue", status: "doing", status_source: "direct", priority: "P2",
    tags: [], blocked_by: [], relates: [], refs: [], paths: [], container: false, ready: false, blocked: false,
    created: "2026-09-29T00:00:00Z", rev: "fnv1a:0123456789abcdef",
    checks: { done: 0, total: 0 }, work: { type: "task", type_source: "implicit", weight: 1 }, body: "## Outcome\n\nThe queue.",
    frontmatter: {}, path: "issue.md", comments: [], notes_chain: [], artifacts: [], activity: [],
    commits: [], commits_skipped: [],
    links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
  };
  const requests: string[] = [];

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    requests.push(url.pathname + url.search);
    if (url.pathname.endsWith("/stream")) return streamFixture(url.href);
    if ((init?.method ?? "GET") !== "GET") return json({ state: "ok" });
    switch (url.pathname) {
      case "/api/meta": return json(meta);
      case "/api/health": return json({ daemon: "reachable" });
      case "/api/issues": return json({ issues: [{ ...detail, counts: { comments: 0, artifacts: 0, refs: 0 } }] });
      case "/api/issues/D-5": return json(detail);
      case "/api/projects": return json({ projects: [{ key: "demo", prefix: "D", components: [], repos: [], issues: 1 }] });
      case "/api/agents": return json({ agents: [], by_issue: {}, daemon: "reachable" });
      case "/api/master/state": return json({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] });
      case "/api/setup": return json({ checks: [] });
      case "/api/update/banner": return json(null);
      case "/api/human-threads/me": return json({ human: null });
      case "/api/human-threads/rooms": return json({ rooms: [] });
      case "/api/human-threads/people": return json({ people: [] });
      case "/api/human-threads/agents": return json({ agents: [] });
      case "/api/work-conversations": return json({
        conversations: [{
          id: "conv-1", kind: "operator", participants: [], last: null, last_activity: 1700000000,
          counts: { messages: 0, pending: 0, failed: 0 },
        }],
      });
      case "/api/work-conversations/conv-1": return json({
        conversation: { id: "conv-1", kind: "pair", participants: [] },
        messages: [], cursor: 0, more_before: false,
      });
      default: return json({});
    }
  }) as typeof fetch;

  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  setSessionKey("work-item-threads-nav");
  await React.act(async () => root.render(React.createElement(LocaleProvider, null, React.createElement(AppComponent))));
  for (let i = 0; i < 40; i++) await flush();

  const drawer = document.querySelector('[data-drawer="work-item"]');
  assert(drawer, "the deep-linked work item opens over Threads");
  assert(drawer.textContent?.includes("From Threads"), "the detail names its Threads origin");
  assert(drawer.textContent?.includes("Ship the queue"), "the detail shows the referenced issue");
  assert(window.location.search.includes("issue=D-5"), "the open detail is in the URL");
  assert(requests.some((r) => r.startsWith("/api/work-conversations?") && r.includes("project=cadence")), "the scoped list request keeps project=cadence");

  await React.act(async () => {
    document.querySelector<HTMLElement>('[aria-label="Close work item"]')?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  for (let i = 0; i < 60 && document.querySelector("[data-drawer]") !== null; i++) {
    await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
  }
  assert(document.querySelector("[data-drawer]") === null, "closing the detail removes the overlay");
  assert(!window.location.search.includes("issue="), `closing removes only the issue selection, got ${window.location.search}`);
  const scopeAfter = readThreadsUrl(window.location.search);
  assert(scopeAfter.conversation === "conv-1" && scopeAfter.project === "cadence" && scopeAfter.agent === "cc-1" && scopeAfter.kind === "agents" && scopeAfter.includeSystem,
    `every Threads selector survives the close, got ${window.location.search}`);
  assert(!readThreadsUrl(scope).conversation?.includes("issue"), "the conversation selection is untouched");

  await React.act(async () => { root.unmount(); });
  console.log("threads work-item navigation checks passed");
}

void main();
