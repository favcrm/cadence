/**
 * CAD-1811 review regression: "Change something…" hands the item to the
 * composer. The detail close must not steal the composer's focus back to
 * the card that opened the decision.
 */
import { streamFixture } from "./streamFixture";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
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
  const { resetTodoLocal } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");

  const json = (data: unknown, status = 200) =>
    new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json" } });
  const meta = {
    read_only: false, signed_in: true, operator: true, actor: "operator",
    session: {
      id: "composer-focus", origin: "loopback", created: 1700000000, last_used: 1700000000,
      idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test",
      user: { sub: "operator", handle: "operator", name: "Operator", email: "operator@example.test", role: "operator" },
    },
  };
  const plan = {
    id: "D-1", project: "demo", title: "Ship the rail", body: "## Goal\n\nThe rail ships.", status: "review",
    status_source: "direct", priority: "P3", tags: [], blocked_by: [], relates: [], refs: [], paths: [], container: false,
    ready: false, blocked: false, created: "2026-09-29T00:00:00Z", rev: "fnv1a:0123456789abcdef",
    checks: { done: 0, total: 0 }, work: { type: "epic", type_source: "implicit", weight: 1 },
    frontmatter: {}, path: "issue.md", links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
    comments: [], notes_chain: [], artifacts: [], activity: [], commits: [], commits_skipped: [],
    plan: { state: "proposed", proposed_by: "master", tickets: [{ id: "D-3", title: "Step one", status: "ready" }], progress: { ratio: 0 } },
  };
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname.endsWith("/stream")) return streamFixture(url.href);
    if ((init?.method ?? "GET") !== "GET") return json({ state: "ok" });
    switch (url.pathname) {
      case "/api/meta": return json(meta);
      case "/api/health": return json({ daemon: "reachable" });
      case "/api/overview": return json({
        needs_me: [{
          kind: "plan", audience: "operator", title: "D-1 plan", age: 150, project: "demo", command: "cadence x",
          subject: { kind: "issue", id: "D-1" }, plan: { epic: "D-1", proposed_by: "master" },
        }],
        drift: {}, projects: [], github: { state: "ok" }, daemon: { reachable: true },
      });
      case "/api/issues": return json({ issues: [{ ...plan, counts: { comments: 0, artifacts: 0, refs: 0 } }] });
      case "/api/issues/D-1": return json(plan);
      case "/api/projects": return json({ projects: [{ key: "demo", prefix: "D", components: [], repos: [], issues: 1 }] });
      case "/api/agents": return json({ agents: [{ alias: "master", provider: "fixture", state: "idle" }], by_issue: {}, daemon: "reachable" });
      case "/api/master/state": return json({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] });
      case "/api/setup": return json({ checks: [] });
      case "/api/update/banner": return json(null);
      default: return json({});
    }
  }) as typeof fetch;

  const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
  async function settle(check: () => void) {
    for (let i = 0; i < 120; i++) {
      await flush();
      await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 10)); });
      try { check(); return; } catch { /* keep polling */ }
    }
    await flush();
    check();
  }
  const click = async (element: Element | undefined | null) => {
    assert(element, "click target exists");
    await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  };
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  setSessionKey("work-item-composer");
  resetTodoLocal();
  await React.act(async () => root.render(React.createElement(LocaleProvider, null, React.createElement(AppComponent))));
  await settle(() => assert(document.querySelector('[data-need="plan"]'), "the Home rail renders the plan card"));

  const review = Array.from(document.querySelectorAll("button")).find((b) => b.textContent?.trim() === "Review");
  assert(review, "the plan card offers Review");
  review.focus();
  await click(review);
  await settle(() => assert(document.querySelector('[data-drawer="review"]'), "the plan decision opens"));
  const change = Array.from(document.querySelectorAll('[data-drawer="review"] button')).find((b) => b.textContent?.trim() === "Change something…");
  await click(change);
  await settle(() => assert(document.querySelector("[data-drawer]") === null, "the detail closes on the hand-off"));

  // The composer owns focus after the hand-off; the close must not reclaim it.
  await settle(() => assert(document.activeElement instanceof HTMLTextAreaElement, "the composer holds focus"));
  const composer = document.activeElement as HTMLTextAreaElement;
  assert(composer.value.startsWith("I'd like to change something"), `the composer carries the ask draft, got ${JSON.stringify(composer.value.slice(0, 40))}`);
  await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 200)); });
  assert(document.activeElement === composer, "the close does not steal the composer's focus");

  await React.act(async () => { root.unmount(); });
  console.log("composer focus checks passed");
}

void main();
