/**
 * CAD-1811 review regressions, on the real App:
 *  - dismissing a Home decision returns keyboard focus to the card that
 *    opened it (never <body>);
 *  - a late decision reply for a dismissed item records its decision but
 *    cannot close or replace a newer My work selection.
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
  const App = () => React.createElement(LocaleProvider, null, React.createElement(AppComponent));
  const { setSessionKey } = require("../src/lib/sessionKey") as typeof import("../src/lib/sessionKey");
  const { resetTodoLocal } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");

  const json = (data: unknown, status = 200) =>
    new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json" } });
  const meta = {
    read_only: false, signed_in: true, operator: true, actor: "operator",
    session: {
      id: "work-item-selection", origin: "loopback", created: 1700000000, last_used: 1700000000,
      idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test",
      user: { sub: "operator", handle: "operator", name: "Operator", email: "operator@example.test", role: "operator" },
    },
  };
  // `GET /api/issues/<id>` is the detail contract: no `counts`, and
  // `comments` is the array of comment rows (board-api issue-detail).
  const row = (id: string, extra: Record<string, unknown>) => ({
    id, project: "demo", title: `Ticket ${id}`, body: "", status: "review", status_source: "direct", priority: "P3",
    tags: [], blocked_by: [], relates: [], refs: [], paths: [], container: false, ready: false, blocked: false,
    created: "2026-09-29T00:00:00Z", rev: "fnv1a:0123456789abcdef",
    checks: { done: 0, total: 0 }, work: { type: "task", type_source: "implicit", weight: 1 },
    frontmatter: {}, path: "issue.md",
    links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
    comments: [], notes_chain: [], artifacts: [], activity: [], commits: [], commits_skipped: [],
    ...extra,
  });
  const issues: Record<string, Record<string, unknown>> = {
    "D-1": row("D-1", {
      title: "Ship the rail",
      body: "## Goal\n\nThe rail ships.",
      work: { type: "epic", type_source: "implicit", weight: 1 },
      plan: { state: "proposed", proposed_by: "master", tickets: [{ id: "D-3", title: "Step one", status: "ready" }], progress: { ratio: 0 } },
    }),
    "D-5": row("D-5", { title: "Ship the queue", status: "doing", body: "## Outcome\n\nThe queue ships." }),
    "D-9": row("D-9", { title: "Dark mode", body: "A calm dark theme." }),
  };
  const planRow = {
    kind: "plan", audience: "operator", title: "D-1 plan", age: 150, project: "demo", command: "cadence x",
    subject: { kind: "issue", id: "D-1" }, plan: { epic: "D-1", proposed_by: "master" },
  };
  const ideaRow = {
    kind: "idea_plan", audience: "operator", title: "D-9 idea", age: 170, project: "demo", command: "cadence idea decide D-9 approve",
    subject: { kind: "issue", id: "D-9" },
  };
  let overviewRows: unknown[] = [planRow, ideaRow];
  let planGate: (() => void) | null = null;
  const posts: { path: string; body: any }[] = [];

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    const path = url.pathname;
    if (path.endsWith("/stream")) return streamFixture(url.href);
    if ((init?.method ?? "GET") !== "GET") {
      posts.push({ path, body: JSON.parse(String(init?.body ?? "{}")) });
      if (path === "/api/plans/D-1/approve") {
        await new Promise<void>((resolve) => { planGate = resolve; });
        return json({ state: "approved" });
      }
      return json({ state: "ok" });
    }
    if (path === "/api/meta") return json(meta);
    if (path === "/api/health") return json({ daemon: "reachable" });
    if (path === "/api/overview") return json({ needs_me: overviewRows, drift: {}, projects: [], github: { state: "ok" }, daemon: { reachable: true } });
    if (path === "/api/issues") {
      return json({ issues: Object.values(issues).map((issue) => ({ ...issue, counts: { comments: 0, artifacts: 0, refs: 0 } })) });
    }
    const issue = /^\/api\/issues\/([\w-]+)$/.exec(path);
    if (issue && issues[issue[1]]) return json(issues[issue[1]]);
    if (path === "/api/projects") return json({ projects: [{ key: "demo", prefix: "D", components: [], repos: [], issues: 2 }] });
    if (path === "/api/agents") return json({ agents: [], by_issue: {}, daemon: "reachable" });
    if (path === "/api/master/state") return json({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] });
    if (path === "/api/my-work") return json({
      owner: "operator", total: 1, next_cursor: null, projects: ["demo"],
      counts: { active: 1, done: 0, dropped: 0, all: 1 }, status_counts: { doing: 1 },
      issues: [{
        id: "D-5", project: "demo", title: "Ship the queue", status: "doing", priority: "P2",
        blocked: false, blocked_reason: null, container: false, checks: { done: 0, total: 0 },
        counts: { comments: 0 }, created: "2026-09-29T00:00:00Z", updated: null,
      }],
    });
    if (path === "/api/my-work/issues/D-5") return json({ owner: "operator", id: "D-5", assigned: true, assignment: null });
    if (path === "/api/setup") return json({ checks: [] });
    if (path === "/api/update/banner") return json(null);
    return json({});
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
  /** Let a resolved request's promise chain and follow-up reads settle. */
  const settleLate = async () => {
    for (let i = 0; i < 12; i++) {
      await flush();
      await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 10)); });
    }
  };
  const click = async (element: Element | undefined | null) => {
    assert(element, "click target exists");
    await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await flush();
  };
  const buttons = (scope: ParentNode) => Array.from(scope.querySelectorAll("button")) as HTMLButtonElement[];
  const labelled = (scope: ParentNode, text: string) => buttons(scope).find((b) => b.textContent?.trim() === text);
  const drawer = (kind: string) => document.querySelector(`[data-drawer="${kind}"]`);
  const navLink = (href: string) => Array.from(document.querySelectorAll("a")).find((a) => a.getAttribute("href") === href);

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  setSessionKey("work-item-selection");
  resetTodoLocal();
  await React.act(async () => root.render(React.createElement(App)));
  await settle(() => assert(document.querySelector('[data-need="plan"]'), "the Home rail renders the plan card"));

  const openPlan = async () => {
    const review = labelled(document.querySelector('[data-need="plan"]')!, "Review");
    assert(review, "the plan card offers Review");
    review.focus();
    await click(review);
    await settle(() => assert(drawer("review"), "the plan decision opens in the shared detail"));
    return review;
  };
  const openMyWorkIssue = async () => {
    const link = navLink("/my-work");
    assert(link, "the sidebar offers My work");
    await click(link);
    await settle(() => assert(document.querySelector('[data-my-work-inspect="D-5"]'), "My work lists the assigned row"));
    await click(document.querySelector('[data-my-work-inspect="D-5"]'));
    await settle(() => assert(drawer("work-item"), "the My work row opens the shared detail"));
    assert(drawer("work-item")!.textContent?.includes("From My work"), "the detail keeps its My work origin");
    assert(window.location.search.includes("issue=D-5"), "the My work issue is the selected URL item");
  };

  // 1. Keyboard dismissal returns focus to the opening card, never <body>.
  const review = await openPlan();
  await click(labelled(drawer("review")!, "Not now"));
  await settle(() => assert(document.querySelector("[data-drawer]") === null, "Not now closes the shared detail"));
  await settleLate();
  assert(document.activeElement === review, `dismissing returns focus to the opening card, got ${document.activeElement?.tagName}`);

  // 2. A late approval cannot replace a newer My work selection, even when
  //    the dismissed decision still has a following reading item.
  planGate = null;
  await openPlan();
  await click(labelled(drawer("review")!, "Approve plan"));
  await settle(() => assert(planGate, "the approval request is in flight"));
  await click(labelled(drawer("review")!, "Not now"));
  await settle(() => assert(document.querySelector("[data-drawer]") === null, "the in-flight decision can be dismissed"));
  await openMyWorkIssue();
  await React.act(async () => { planGate?.(); });
  await settleLate();
  assert(drawer("work-item"), "the late approval does not close the newer selection");
  assert(drawer("work-item")!.textContent?.includes("Ship the queue"), "the late approval does not replace the newer item");
  assert(drawer("work-item")!.textContent?.includes("From My work"), "the newer origin is unchanged");
  assert(window.location.search.includes("issue=D-5"), "the newer URL selection is unchanged");

  // 3. The same fence when the dismissed decision's queue is exhausted.
  await click(document.querySelector('[aria-label="Close work item"]'));
  await settle(() => assert(document.querySelector("[data-drawer]") === null, "the My work detail closes"));
  const homeLink = navLink("/");
  assert(homeLink, "the sidebar offers Home");
  await click(homeLink);
  await settle(() => assert(document.querySelector('[data-need="plan"]'), "Home returns"));
  overviewRows = [planRow];
  const { resources } = require("../src/lib/resources") as typeof import("../src/lib/resources");
  await React.act(async () => { await resources.overview.refresh(); });
  await settle(() => assert(document.querySelector('[data-need="plan"]'), "the rail reflects the exhausted queue"));
  planGate = null;
  await openPlan();
  await click(labelled(drawer("review")!, "Approve plan"));
  await settle(() => assert(planGate, "the second approval is in flight"));
  await click(labelled(drawer("review")!, "Not now"));
  await settle(() => assert(document.querySelector("[data-drawer]") === null, "the exhausted decision can be dismissed"));
  await openMyWorkIssue();
  await React.act(async () => { planGate?.(); });
  await settleLate();
  assert(drawer("work-item"), "an exhausted late approval does not close the newer selection");
  assert(drawer("work-item")!.textContent?.includes("Ship the queue"), "an exhausted late approval keeps the newer item");
  assert(window.location.search.includes("issue=D-5"), "an exhausted late approval keeps the newer URL");

  await React.act(async () => { root.unmount(); });
  console.log("work-item selection checks passed");
}

void main();
