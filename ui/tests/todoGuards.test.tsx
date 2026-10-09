export {};
/**
 * CAD-1273: the To do rail and review drawer's safety behaviour, checked
 * on the rendered NeedsRail. The server enforces all of it; each check
 * here starts from a state where only the UI guard stands in the way, so
 * deleting the guard fails the check:
 *  - a read-only board disables every write control on cards and in the drawer;
 *  - Publish posts the row's pinned head;
 *  - a 409 `head_moved` says the plain copy, not the server's text;
 *  - a failed "Fix it" says so in plain words and keeps the button;
 *  - an unknown kind still renders as a generic card.
 */
import type { Overview } from "../src/lib/types";
import type { ResourceState } from "../src/lib/cache";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
Object.defineProperty(globalThis, "EventSource", {
  configurable: true,
  value: class { onopen = null; onerror = null; addEventListener() {} removeEventListener() {} close() {} },
});
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
const { resetTodoLocal } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
const { HEAD_MOVED_COPY } = require("../src/features/home/MergeForm") as typeof import("../src/features/home/MergeForm");

// ---- a fake board: issue reads, and writes recorded and answered per path ----
const SHA = "c".repeat(40);
const issue = (id: string, extra: Record<string, unknown>) => ({
  id, project: "demo", title: `Ticket ${id}`, body: "", status: "review", status_source: "direct", priority: "P3",
  tags: [], blocked_by: [], relates: [], refs: [], container: false, ready: false, blocked: false,
  created: "2026-09-29T00:00:00Z", rev: "fnv1a:0123456789abcdef", counts: { comments: 0, artifacts: 0, refs: 0 },
  checks: { done: 0, total: 0 }, ...extra,
});
const issues: Record<string, unknown> = {
  "D-1": issue("D-1", {
    title: "Ship the rail",
    body: "## Goal\n\nThe rail ships.",
    plan: { state: "proposed", proposed_by: "master", tickets: [{ id: "D-3", title: "Step one", status: "ready" }], progress: { ratio: 0 } },
  }),
  "D-9": issue("D-9", { title: "Dark mode", body: "A calm dark theme." }),
  "D-2": issue("D-2", { title: "Light the fuselage", body: "Visitors see a calm message." }),
};
const posts: { path: string; body: any }[] = [];
let replies: Record<string, { status: number; body: unknown }> = {};
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if ((init?.method ?? "GET") === "POST") {
    posts.push({ path: url.pathname, body: JSON.parse(String(init?.body ?? "{}")) });
    const r = replies[url.pathname];
    return r ? json(r.body, r.status) : json({ state: "ok" });
  }
  const m = /^\/api\/issues\/([\w-]+)$/.exec(url.pathname);
  if (m && issues[m[1]]) return json(issues[m[1]]);
  if (url.pathname === "/api/approvals/state") return json({ state: "missing" });
  return json({});
}) as typeof fetch;

const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
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
const buttons = (scope: ParentNode) => Array.from(scope.querySelectorAll("button")) as HTMLButtonElement[];
const labelled = (scope: ParentNode, text: string) => buttons(scope).find((b) => b.textContent?.trim() === text);
const isDisabled = (b: HTMLButtonElement) => b.disabled || b.hasAttribute("disabled");

const rows = {
  permission: {
    kind: "master_permission", audience: "operator", title: "master permission", age: 30, project: "demo", command: "cadence x",
    subject: { kind: "permission", id: "p1" },
    permission: { id: "p1", command: "ls -la", cwd: "/tmp", reason: "List files", risk: "high", status: "pending" },
  },
  blocked: {
    kind: "blocked", audience: "operator", title: "D-5 blocked", age: 60, project: "demo", command: "cadence x",
    subject: { kind: "issue", id: "D-5" },
  },
  stopped: {
    kind: "stopped", audience: "operator", title: "w2 stopped", age: 75, project: "demo", command: "cadence x",
    subject: { kind: "agent", id: "w2" },
  },
  fenced: {
    kind: "fenced", audience: "operator", title: "w1 fenced", age: 90, project: "demo", command: "cadence x",
    subject: { kind: "agent", id: "w1" },
  },
  question: {
    kind: "question", audience: "operator", title: "D-4 question", age: 120, project: "demo", command: "cadence x",
    subject: { kind: "report", id: "D-4/q.md" },
    question: { issue: "D-4", report: "q.md", agent: "w1", options: ["hourly", "daily"] },
  },
  plan: {
    kind: "plan", audience: "operator", title: "D-1 plan", age: 150, project: "demo", command: "cadence x",
    subject: { kind: "issue", id: "D-1" }, plan: { epic: "D-1", proposed_by: "master" },
  },
  idea: {
    kind: "idea_plan", audience: "operator", title: "D-9 idea", age: 180, project: "demo", command: "cadence x",
    subject: { kind: "issue", id: "D-9" },
  },
  merge: {
    kind: "merge_decision", audience: "operator", title: "D-2 ready to merge", age: 210, project: "demo", command: "cadence x",
    subject: { kind: "issue", id: "D-2" },
    merge: { issue: "D-2", pr: "acme/app#9", sha: SHA, reviewer: "r1", verdict_summary: "PASS" },
  },
};

async function render(needs: unknown[], readOnly: boolean) {
  resetTodoLocal();
  posts.length = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const overview = { status: "ok", inFlight: false, error: null, asOf: Date.now(), data: { needs_me: needs } } as unknown as ResourceState<Overview>;
  await React.act(async () => {
    root.render(
      React.createElement(NeedsRail, {
        overview, readOnly, onOpenIssue: () => {}, overviewHref: "/overview", permissionsHref: "/settings/permissions",
        onAsk: () => {}, collapsed: false, onToggleCollapse: () => {}, onAskAgent: () => {},
      }),
    );
  });
  const card = (kind: string) => host.querySelector(`[data-need="${kind}"]`) as HTMLElement;
  const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); };
  return { host, card, done };
}

// ---- 1. read-only disables every write control, on cards and in the drawer ----
{
  const { host, card, done } = await render(Object.values(rows), true);
  await settle(() => assert(card("plan"), "cards render"));

  const permission = card("master_permission");
  for (const name of ["Allow once", "Always ▾", "Deny ▾"]) {
    const b = labelled(permission, name);
    assert(b, `permission control present: ${name}`);
    assert(isDisabled(b), `read-only disables "${name}"`);
  }

  const fix = labelled(card("blocked"), "Fix it");
  assert(fix && isDisabled(fix), 'read-only disables "Fix it"');

  // The fenced card opens by tapping its title; its unfence choices are still write controls.
  await click(card("fenced").querySelector(".todo-hit"));
  for (const name of ["It got cut off", "It finished", "It failed"]) {
    const b = labelled(card("fenced"), name);
    assert(b, `unfence choice present: ${name}`);
    assert(isDisabled(b), `read-only disables "${name}"`);
  }

  // A stopped agent's "Start it again" and the ⋯ menu's Snooze / Dismiss write too.
  await click(card("stopped").querySelector(".todo-hit"));
  const resume = labelled(card("stopped"), "Start it again");
  assert(resume && isDisabled(resume), 'read-only disables "Start it again"');
  await click(card("stopped").querySelector(".needmore"));
  for (const name of ["Snooze 24h", "Snooze 7d", "Dismiss"]) {
    const b = labelled(card("stopped"), name);
    assert(b, `menu item present: ${name}`);
    assert(isDisabled(b), `read-only disables "${name}"`);
  }

  for (const name of ["hourly", "daily"]) {
    const b = labelled(card("question"), name);
    assert(b, `answer option present: ${name}`);
    assert(isDisabled(b), `read-only disables the "${name}" answer`);
  }

  const drawerControls: [string, string[]][] = [
    ["plan", ["Approve plan", "Change something…", "Send back…"]],
    ["idea_plan", ["Approve idea", "Send back…", "Not ready? Park it for later"]],
    ["merge_decision", ["Publish", "Approve this version", "Send back…"]],
  ];
  for (const [kind, names] of drawerControls) {
    await click(labelled(card(kind), "Review"));
    await settle(() => {
      const d = host.querySelector('[data-drawer="review"]');
      assert(d && names.every((n) => labelled(d, n)), `${kind} drawer controls render`);
    });
    const drawer = host.querySelector('[data-drawer="review"]')!;
    for (const name of names) assert(isDisabled(labelled(drawer, name)!), `read-only disables "${name}" in the ${kind} drawer`);
    await click(labelled(drawer, "Not now"));
    await settle(() => assert(!host.querySelector('[data-drawer="review"]'), "Not now closes the drawer"));
  }
  assert(posts.length === 0, "a read-only board sends nothing");
  await done();
}

// ---- 2 + 3. Publish carries the row's pinned head; a moved head says the plain copy ----
async function publish() {
  const view = await render([rows.merge], false);
  await click(labelled(view.card("merge_decision"), "Review"));
  await settle(() => assert(labelled(view.host.querySelector('[data-drawer="review"]')!, "Publish"), "merge drawer renders"));
  const drawer = view.host.querySelector('[data-drawer="review"]')!;
  await settle(() => assert(!isDisabled(labelled(drawer, "Publish")!), "Publish is offered for a row that names its head"));
  await click(labelled(drawer, "Publish"));
  return { ...view, drawer };
}
{
  replies = { "/api/delivery/D-2/merge": { status: 200, body: { state: "queued" } } };
  const view = await publish();
  const merge = posts.find((p) => p.path === "/api/delivery/D-2/merge");
  assert(merge, "Publish posts the merge");
  assert(merge.body.sha === SHA, `the merge carries the row's pinned head, got ${JSON.stringify(merge.body)}`);
  await view.done();
}
{
  replies = {
    "/api/delivery/D-2/merge": {
      status: 409,
      body: { error: "head moved: gh pr merge exited 1 (pid 4242): cadence delivery merge D-2", code: "head_moved" },
    },
  };
  const view = await publish();
  await settle(() => assert(view.drawer.querySelector(".rv-error"), "a refused merge shows an error"));
  const text = view.drawer.querySelector(".rv-error")!.textContent;
  assert(text === HEAD_MOVED_COPY, `a moved head says the plain copy, got ${JSON.stringify(text)}`);
  await view.done();
}

// ---- a refused plan decision shows plain words in the drawer, never the server's text ----
{
  replies = { "/api/plans/D-1/approve": { status: 409, body: { error: "plan already decided: run cadence plan approve D-1 (pid 55)" } } };
  const { host, card, done } = await render([rows.plan], false);
  await click(labelled(card("plan"), "Review"));
  await settle(() => assert(!isDisabled(labelled(host.querySelector('[data-drawer="review"]')!, "Approve plan") ?? ({ disabled: true } as HTMLButtonElement)), "plan drawer ready"));
  await click(labelled(host.querySelector('[data-drawer="review"]')!, "Approve plan"));
  await settle(() => assert(host.querySelector('[data-drawer="review"] .rv-error'), "a refused decision shows an error"));
  const text = host.querySelector('[data-drawer="review"] .rv-error')!.textContent ?? "";
  assert(!/cadence|pid/.test(text), `no server text or command in the drawer, got ${JSON.stringify(text)}`);
  await done();
}

// ---- 4. a failed "Fix it" says so in plain words and keeps the button ----
{
  replies = {
    "/api/threads/master/messages": { status: 500, body: { error: "turn failed: cadence agent send exited 1 (pid 7)" } },
  };
  const { card, done } = await render([rows.blocked], false);
  await click(labelled(card("blocked"), "Fix it"));
  await settle(() => assert(card("blocked").querySelector('[role="alert"]'), "a failed send shows an error"));
  const alert = card("blocked").querySelector('[role="alert"]')!.textContent ?? "";
  assert(/couldn't be sent to Master/.test(alert), `the error is plain, got ${JSON.stringify(alert)}`);
  assert(!/pid|cadence|exited/.test(alert), `no raw server text on the card, got ${JSON.stringify(alert)}`);
  const again = labelled(card("blocked"), "Fix it");
  assert(again && !isDisabled(again), '"Fix it" stays available after a failed send');
  assert(!/Sent to Master/.test(card("blocked").textContent ?? ""), "a failed send is not shown as sent");
  await done();
}

// ---- 5. an unknown kind still renders as a generic card ----
{
  const { card, done } = await render(
    [{ kind: "brand_new_kind", audience: "operator", title: "something odd", age: 30, project: "demo", command: "cadence x" }],
    false,
  );
  const c = card("brand_new_kind");
  assert(c, "an unknown kind renders a card");
  assert(c.querySelector(".todo-title")?.textContent === "Something needs your attention", "the generic title");
  assert(labelled(c, "Fix it"), 'the generic card offers "Fix it"');
  await done();
}

console.log("todo guard checks passed");
}
void main();
