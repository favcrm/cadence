import { streamFixture } from "./streamFixture";
export {};
/**
 * CAD-1273: the To do rail and its decision detail's safety behaviour,
 * checked on the rendered rail plus the shared work-item detail (CAD-1811). The server enforces all of it; each check
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
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const RailWithDetail = (require("./railHarness") as typeof import("./railHarness")).default;
const { resources } = require("../src/lib/resources") as typeof import("../src/lib/resources");
const { resetTodoLocal, useHomeCount } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
const { HEAD_MOVED_COPY } = require("../src/features/home/MergeForm") as typeof import("../src/features/home/MergeForm");

// ---- a fake board: issue reads, and writes recorded and answered per path ----
const SHA = "c".repeat(40);
const issue = (id: string, extra: Record<string, unknown>) => ({
  id, project: "demo", title: `Ticket ${id}`, body: "", status: "review", status_source: "direct", priority: "P3",
  tags: [], blocked_by: [], relates: [], refs: [], container: false, ready: false, blocked: false,
  created: "2026-09-29T00:00:00Z", rev: "fnv1a:0123456789abcdef", counts: { comments: 0, artifacts: 0, refs: 0 },
  comments: [], checks: { done: 0, total: 0 }, ...extra,
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
const approvalStateUrls: string[] = [];
let replies: Record<string, { status: number; body: unknown }> = {};
// A request that waits to be released, to hold a send in flight.
let holds: Record<string, Promise<void>> = {};
let approval: unknown = { state: "missing" };
let strictThreeTupleApprove = false;
let agentsReply: unknown = { agents: [], by_issue: {} };
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname.endsWith("/stream")) return streamFixture(url.href);
  if ((init?.method ?? "GET") === "POST") {
    const parsedBody = JSON.parse(String(init?.body ?? "{}"));
    posts.push({ path: url.pathname, body: parsedBody });
    await (holds[url.pathname] as Promise<void> | undefined);
    if (
      url.pathname === "/api/approvals/approve" &&
      strictThreeTupleApprove &&
      Object.keys(parsedBody).some((k) => k !== "repo" && k !== "pr" && k !== "head")
    ) {
      return json({ error: "Request body contains an unsupported field.", code: "validation_failed" }, 400);
    }
    const r = replies[url.pathname];
    return r ? json(r.body, r.status) : json({ state: "ok" });
  }
  const m = /^\/api\/issues\/([\w-]+)$/.exec(url.pathname);
  if (m && issues[m[1]]) return json(issues[m[1]]);
  if (url.pathname === "/api/approvals/state") {
    approvalStateUrls.push(url.pathname + url.search);
    return json(approval);
  }
  if (url.pathname === "/api/agents") return json(agentsReply);
  if (url.pathname.startsWith("/api/agents/")) {
    return json({ updates: [], tasks: [] });
  }
  if (url.pathname.startsWith("/api/threads/")) {
    return json({ entries: [] });
  }
  return json({});
}) as typeof fetch;

const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
// The shared detail closes through DrawerShell's exit transition (its 320ms
// fallback when no transition runs, as in happy-dom), so a settle that waits
// for it must let real time pass inside act.
const tick = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 12)); });
async function settle(check: () => void) {
  for (let i = 0; i < 80; i++) {
    await flush();
    await tick();
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
  q3: {
    kind: "question", audience: "operator", title: "D-6 question", age: 125, project: "demo", command: "cadence x",
    subject: { kind: "report", id: "D-6/q.md" },
    question: { issue: "D-6", report: "q.md", agent: "w1", options: ["red", "green", "blue"] },
  },
  q0: {
    kind: "question", audience: "operator", title: "D-7 question", age: 126, project: "demo", command: "cadence x",
    subject: { kind: "report", id: "D-7/q.md" },
    question: { issue: "D-7", report: "q.md", agent: "w1", options: [] },
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

function Badge({ needs }: { needs: unknown[] }) {
  return React.createElement("span", { "data-badge": true }, String(useHomeCount(needs as never)));
}

async function render(needs: unknown[], readOnly: boolean, opts: { keepLocal?: boolean; noBadge?: boolean; hosted?: boolean } = {}) {
  if (!opts.keepLocal) {
    resetTodoLocal();
    posts.length = 0;
  }
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const update = async (rows: unknown[], ro: boolean) => {
    const overview = { status: "ok", inFlight: false, error: null, asOf: Date.now(), data: { needs_me: rows } } as unknown as ResourceState<Overview>;
    await React.act(async () => {
      root.render(
        React.createElement(
          React.Fragment,
          null,
          // Without the badge nothing reconciles: the marks alone must not hide a changed need.
          opts.noBadge ? null : React.createElement(Badge, { needs: rows }),
          React.createElement(RailWithDetail, {
            overview, readOnly: ro, hosted: opts.hosted, onOpenIssue: () => {}, permissionsHref: "/settings/permissions",
            onAsk: () => {}, onAskAgent: () => {},
          }),
        ),
      );
    });
    await flush();
  };
  await update(needs, readOnly);
  const card = (kind: string) => host.querySelector(`[data-need="${kind}"]`) as HTMLElement;
  const badge = () => host.querySelector("[data-badge]")!.textContent;
  const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); };
  return { host, card, badge, update, done };
}

// ---- 1. read-only disables every write control, inside todo-detail and review drawers ----
{
  const { host, card, done } = await render(Object.values(rows), true);
  await settle(() => assert(card("plan"), "cards render"));
  const todoDrawer = () => document.querySelector('[data-drawer="todo-detail"]') as HTMLElement | null;
  const openTodo = async (el: HTMLElement) => {
    await click(el.querySelector(".todo-hit"));
    await settle(() => assert(todoDrawer(), "todo-detail drawer renders"));
    return todoDrawer()!;
  };
  const closeTodo = async () => {
    const d = todoDrawer();
    if (!d) return;
    await click(labelled(d, "Not now"));
    await settle(() => assert(!todoDrawer(), "todo-detail closes"));
  };

  {
    const d = await openTodo(card("master_permission"));
    for (const name of ["Allow once", "Always ▾", "Deny ▾"]) {
      const b = labelled(d, name);
      assert(b, `permission control present: ${name}`);
      assert(isDisabled(b), `read-only disables "${name}"`);
    }
    await closeTodo();
  }

  {
    const d = await openTodo(card("blocked"));
    const fix = labelled(d, "Fix it");
    assert(fix && isDisabled(fix), 'read-only disables "Fix it"');
    await closeTodo();
  }

  {
    const d = await openTodo(card("fenced"));
    for (const name of ["It got cut off", "It finished", "It failed"]) {
      const b = labelled(d, name);
      assert(b, `unfence choice present: ${name}`);
      assert(isDisabled(b), `read-only disables "${name}"`);
    }
    await closeTodo();
  }

  {
    const d = await openTodo(card("stopped"));
    const resume = labelled(d, "Start it again");
    assert(resume && isDisabled(resume), 'read-only disables "Start it again"');
    const fix = labelled(d, "Fix it");
    assert(fix && isDisabled(fix), 'read-only disables "Fix it" on stopped card');
    await click(d.querySelector('[aria-label="More actions"]'));
    for (const name of ["Snooze 24h", "Snooze 7d", "Dismiss"]) {
      const b = labelled(d, name);
      assert(b, `menu item present: ${name}`);
      assert(b.getAttribute("role") === "menuitem" && b.getAttribute("aria-disabled") === "true", `read-only marks menuitem "${name}" aria-disabled`);
      await click(b);
      assert(posts.length === 0, `clicking aria-disabled menuitem "${name}" sends no write`);
    }
    await closeTodo();
  }

  {
    const d = await openTodo(host.querySelector('[data-need-key="report:D-4/q.md"]') as HTMLElement);
    for (const name of ["hourly", "daily"]) {
      const b = labelled(d, name);
      assert(b, `answer option present: ${name}`);
      assert(isDisabled(b), `read-only disables the "${name}" answer`);
    }
    await closeTodo();
  }

  // Questions with other than two options use the answer form: no option, no text box, no send.
  for (const key of ["q3", "q0"]) {
    const row = rows[key as "q3" | "q0"];
    const c = host.querySelector(`[data-need-key="${row.subject.kind}:${row.subject.id}"]`) as HTMLElement;
    const d = await openTodo(c);
    assert(/read-only/i.test(d.textContent ?? ""), `${key}: the read-only copy shows in place of the form`);
    assert(!d.querySelector("textarea"), `${key}: read-only offers no answer box`);
    assert(!labelled(d, "Send answer"), `${key}: read-only offers no Send answer`);
    for (const o of ["red", "green", "blue"]) assert(!labelled(d, o), `${key}: read-only offers no "${o}" answer`);
    await closeTodo();
  }

  const drawerControls: [string, string[]][] = [
    ["plan", ["Approve plan", "Change something…", "Send back…"]],
    ["idea_plan", ["Approve idea", "Send back…", "Not ready? Park it for later"]],
    ["merge_decision", ["Publish", "Approve this version", "Send back…"]],
  ];
  for (const [kind, names] of drawerControls) {
    await click(card(kind).querySelector(".todo-hit"));
    await settle(() => {
      const d = document.querySelector('[data-drawer="review"]');
      assert(d && names.every((n) => labelled(d, n)), `${kind} drawer controls render`);
    });
    const drawer = document.querySelector('[data-drawer="review"]')!;
    for (const name of names) assert(isDisabled(labelled(drawer, name)!), `read-only disables "${name}" in the ${kind} drawer`);
    await click(labelled(drawer, "Not now"));
    await settle(() => assert(!document.querySelector('[data-drawer="review"]'), "Not now closes the drawer"));
  }
  assert(posts.length === 0, "a read-only board sends nothing");
  await done();
}

// ---- 2 + 3. Publish carries the row's pinned head; a moved head says the plain copy ----
async function publish() {
  const view = await render([rows.merge], false);
  await click(view.card("merge_decision").querySelector(".todo-hit"));
  await settle(() => assert(labelled(document.querySelector('[data-drawer="review"]')!, "Publish"), "merge drawer renders"));
  const drawer = document.querySelector('[data-drawer="review"]')!;
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
  const { card, done } = await render([rows.plan], false);
  await click(card("plan").querySelector(".todo-hit"));
  await settle(() => assert(!isDisabled(labelled(document.querySelector('[data-drawer="review"]')!, "Approve plan") ?? ({ disabled: true } as HTMLButtonElement)), "plan drawer ready"));
  await click(labelled(document.querySelector('[data-drawer="review"]')!, "Approve plan"));
  await settle(() => assert(document.querySelector('[data-drawer="review"] .rv-error'), "a refused decision shows an error"));
  const text = document.querySelector('[data-drawer="review"] .rv-error')!.textContent ?? "";
  assert(!/cadence|pid/.test(text), `no server text or command in the drawer, got ${JSON.stringify(text)}`);
  await done();
}

// ---- 4. a failed "Fix it" says so in plain words and keeps the button ----
{
  replies = {
    "/api/threads/master/messages": { status: 500, body: { error: "turn failed: cadence agent send exited 1 (pid 7)" } },
  };
  const { card, done } = await render([rows.blocked], false);
  await click(card("blocked").querySelector(".todo-hit"));
  await settle(() => assert(document.querySelector('[data-drawer="todo-detail"]'), "todo-detail renders"));
  const d = () => document.querySelector('[data-drawer="todo-detail"]')!;
  await click(labelled(d(), "Fix it"));
  await settle(() => assert(d().querySelector('[role="alert"]'), "a failed send shows an error"));
  const alert = d().querySelector('[role="alert"]')!.textContent ?? "";
  assert(/couldn't be sent to Master/.test(alert), `the error is plain, got ${JSON.stringify(alert)}`);
  assert(!/pid|cadence|exited/.test(alert), `no raw server text on the drawer alert, got ${JSON.stringify(alert)}`);
  const again = labelled(d(), "Fix it");
  assert(again && !isDisabled(again), '"Fix it" stays available after a failed send');
  assert(!/Sent to Master/.test(card("blocked").textContent ?? ""), "a failed send is not shown as sent");
  await done();
}

// ---- a mark applies only to the need it was made for (the server merges causes into one row per subject) ----
const stoppedW1 = { ...rows.stopped, subject: { kind: "agent", id: "w1" } };
{
  // (a) agent w1 fenced → "It got cut off" → the same key now lists a stopped agent
  const v = await render([rows.fenced], false);
  const td = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(v.card("fenced").querySelector(".todo-hit"));
  await settle(() => assert(td(), "fenced drawer renders"));
  await click(labelled(td()!, "It got cut off"));
  await settle(() => assert(!td() && /Recorded/.test(v.card("fenced").textContent ?? ""), "the decided card says what was recorded"));
  assert(v.badge() === "0", `the badge drops with the decided card, got ${v.badge()}`);
  await v.update([stoppedW1], false);
  assert(!/Recorded/.test(v.card("stopped").textContent ?? ""), "the new need is not shown as decided");
  await click(v.card("stopped").querySelector(".todo-hit"));
  await settle(() => assert(td() && labelled(td()!, "Fix it"), "the new need offers its control"));
  await click(labelled(td()!, "Not now"));
  await settle(() => assert(!td(), "drawer closes"));
  assert(v.badge() === "1", `the badge counts the new need, got ${v.badge()}`);
  await v.update([rows.fenced], false);
  assert(!/Recorded/.test(v.card("fenced").textContent ?? ""), "the old mark is gone, not waiting to come back");
  assert(v.badge() === "1", `the old mark no longer hides the card, got ${v.badge()}`);
  await v.done();
}
{
  // (b) Publish D-2 at one head → the same key lists a merge decision at a new head
  replies = { "/api/delivery/D-2/merge": { status: 200, body: { state: "queued" } } };
  const v = await publish();
  await settle(() => assert(/Published/.test(v.card("merge_decision").textContent ?? ""), "the published card says so"));
  const moved = { ...rows.merge, merge: { ...rows.merge.merge, sha: "d".repeat(40) } };
  await v.update([moved], false);
  assert(!/Published/.test(v.card("merge_decision").textContent ?? ""), "a new head is not shown as published");
  assert(v.card("merge_decision").querySelector(".todo-hit") && !v.card("merge_decision").hasAttribute("data-done"), "the new head offers its control");
  assert(v.badge() === "1", `the badge counts the new head, got ${v.badge()}`);
  await v.update([rows.merge], false);
  assert(!/Published/.test(v.card("merge_decision").textContent ?? ""), "the old mark is gone");
  await v.done();
}
{
  // (c) a plan approved → the same issue key now lists a blocked_ready need
  replies = { "/api/plans/D-1/approve": { status: 200, body: { state: "approved" } } };
  const v = await render([rows.plan], false);
  const td = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(v.card("plan").querySelector(".todo-hit"));
  await settle(() => assert(!isDisabled(labelled(document.querySelector('[data-drawer="review"]')!, "Approve plan") ?? ({ disabled: true } as HTMLButtonElement)), "plan drawer ready"));
  await click(labelled(document.querySelector('[data-drawer="review"]')!, "Approve plan"));
  await settle(() => assert(/Plan approved/.test(v.card("plan").textContent ?? ""), "the approved card says so"));
  const ready = { ...rows.blocked, kind: "blocked_ready", subject: { kind: "issue", id: "D-1" } };
  await v.update([ready], false);
  assert(!/Plan approved/.test(v.card("blocked_ready").textContent ?? ""), "the new need is not shown as an approved plan");
  await click(v.card("blocked_ready").querySelector(".todo-hit"));
  await settle(() => assert(td() && labelled(td()!, "Fix it"), "the new need offers its control"));
  await click(labelled(td()!, "Not now"));
  await settle(() => assert(!td(), "drawer closes"));
  assert(v.badge() === "1", `the badge counts the new need, got ${v.badge()}`);
  await v.update([rows.plan], false);
  assert(!/Plan approved/.test(v.card("plan").textContent ?? ""), "the old mark is gone");
  await v.done();
}

{
  // Before any reconcile has run, a mark still must not apply to a changed need.
  const v = await render([rows.fenced], false, { noBadge: true });
  const td = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(v.card("fenced").querySelector(".todo-hit"));
  await settle(() => assert(td(), "fenced drawer renders"));
  await click(labelled(td()!, "It got cut off"));
  await settle(() => assert(!td() && /Recorded/.test(v.card("fenced").textContent ?? ""), "decided"));
  await v.update([stoppedW1], false);
  assert(!/Recorded/.test(v.card("stopped").textContent ?? ""), "a decided mark does not cover a changed need");
  await v.done();
  const h = await render([rows.fenced], false, { noBadge: true });
  const htd = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(h.card("fenced").querySelector(".todo-hit"));
  await settle(() => assert(htd(), "fenced drawer renders"));
  await click(htd()!.querySelector('[aria-label="More actions"]'));
  await click(labelled(htd()!, "Dismiss"));
  await settle(() => assert(!h.card("fenced"), "a dismissed card leaves the list"));
  await h.update([stoppedW1], false);
  assert(h.card("stopped"), "a dismissal does not hide a changed need");
  await h.done();
}

// ---- a clock the server recomputes (stalled's `since`) never changes what a mark covers ----
{
  const row = (since?: number) => ({ kind: "stalled", audience: "operator", title: "w3 stalled", age: 60, project: "demo", command: "cadence x", subject: { kind: "agent", id: "w3" }, ...(since === undefined ? {} : { since }) });
  const v = await render([row(100)], false);
  const td = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(v.card("stalled").querySelector(".todo-hit"));
  await settle(() => assert(td(), "stalled drawer renders"));
  await click(labelled(td()!, "Fix it"));
  await settle(() => assert(!td() && /Sent to Master/.test(v.card("stalled").textContent ?? ""), "the send lands as sent"));
  for (const next of [row(101), row(), row(99)]) {
    await v.update([next], false);
    assert(/Sent to Master/.test(v.card("stalled").textContent ?? ""), "a drifting or missing `since` keeps the card sent");
    await click(v.card("stalled").querySelector(".todo-hit"));
    await settle(() => assert(td(), "stalled drawer opens"));
    assert(!labelled(td()!, "Fix it"), 'no second "Fix it"');
    await click(labelled(td()!, "Not now"));
    await settle(() => assert(!td(), "drawer closes"));
  }
  assert(posts.filter((p) => p.path === "/api/threads/master/messages").length === 1, "one send for one occurrence");
  await v.done();
}

// ---- leaving Home and coming back is a new visit: decided and sent marks are dropped, for every kind ----
for (const row of [
  { kind: "review_escalated", audience: "operator", title: "D-5 review escalated", age: 60, project: "demo", command: "cadence x", subject: { kind: "issue", id: "D-5" }, since: 1000 },
  { ...rows.stopped, subject: { kind: "agent", id: "w2" } },
]) {
  const a = await render([row], false);
  const atd = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(a.card(row.kind).querySelector(".todo-hit"));
  await settle(() => assert(atd(), "drawer opens"));
  await click(labelled(atd()!, "Fix it"));
  await settle(() => assert(!atd() && /Sent to Master/.test(a.card(row.kind).textContent ?? ""), "the send lands as sent"));
  await a.done();
  const b = await render([row], false, { keepLocal: true });
  const btd = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(b.card(row.kind).querySelector(".todo-hit"));
  await settle(() => assert(btd(), "drawer opens on new visit"));
  assert(labelled(btd()!, "Fix it"), `${row.kind}: a new visit offers Fix it again`);
  await b.done();
}
{
  const a = await render([rows.fenced], false);
  const atd = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(a.card("fenced").querySelector(".todo-hit"));
  await settle(() => assert(atd(), "drawer opens"));
  await click(labelled(atd()!, "It got cut off"));
  await settle(() => assert(!atd() && /Recorded/.test(a.card("fenced").textContent ?? ""), "decided"));
  await a.done();
  const b = await render([rows.fenced], false, { keepLocal: true });
  assert(!/Recorded/.test(b.card("fenced").textContent ?? ""), "a new visit drops the decided mark");
  await b.done();
}
{
  // Without the badge nothing reconciles: a sent mark alone must not cover a changed need.
  const a = await render([rows.blocked], false, { noBadge: true });
  const atd = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(a.card("blocked").querySelector(".todo-hit"));
  await settle(() => assert(atd(), "drawer opens"));
  await click(labelled(atd()!, "Fix it"));
  await settle(() => assert(!atd() && /Sent to Master/.test(a.card("blocked").textContent ?? ""), "sent"));
  await a.update([{ ...rows.blocked, kind: "blocked_ready" }], false);
  await click(a.card("blocked_ready").querySelector(".todo-hit"));
  await settle(() => assert(atd(), "blocked_ready drawer opens"));
  assert(labelled(atd()!, "Fix it"), "a sent mark does not cover a changed need");
  await a.done();
}
{
  // A failed snooze says so in plain words; the server's text is only under Details.
  replies = { "/api/needs/snooze": { status: 500, body: { error: "pid 4242: `cadence needs snooze` failed (pm.yaml)" } } };
  const v = await render([rows.blocked], false);
  const td = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(v.card("blocked").querySelector(".todo-hit"));
  await settle(() => assert(td(), "drawer opens"));
  await click(td()!.querySelector('[aria-label="More actions"]'));
  await click(labelled(td()!, "Snooze 24h"));
  await settle(() => assert(td()!.querySelector('[role="alert"]'), "a failed snooze shows an error"));
  assert(!/pid|cadence|pm\.yaml/.test(td()!.querySelector('[role="alert"]')!.textContent ?? ""), "no raw server text in the alert");
  const detailsBtn = buttons(td()!).filter((b) => b.textContent?.trim() === "Details").pop();
  await click(detailsBtn);
  assert(/pid 4242/.test(td()!.querySelector(".todo-tech")?.textContent ?? ""), "the server's words are under Details");
  replies = {};
  await v.done();
}
{
  // within one visit, closing and reopening the narrow slide-over keeps the card sent
  const v = await render([rows.blocked], false);
  await click(v.host.querySelector(".needbtn"));
  const slide = () => document.querySelector(".needsdrawer") as HTMLElement | null;
  const td = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(slide()!.querySelector('[data-need="blocked"] .todo-hit'));
  await settle(() => assert(td(), "drawer opens from slide-over"));
  await click(labelled(td()!, "Fix it"));
  await settle(() => assert(!td(), "drawer closes after send"));
  await click(v.host.querySelector(".needbtn"));
  const again = slide()!.querySelector('[data-need="blocked"]') as HTMLElement;
  assert(/Sent to Master/.test(again.textContent ?? ""), "the reopened slide-over still says sent");
  await click(again.querySelector(".todo-hit"));
  await settle(() => assert(td(), "reopened drawer renders"));
  assert(!labelled(td()!, "Fix it"), 'no second "Fix it"');
  await v.done();
}

// ---- "Fix it" in flight survives a remount of the list: no second send ----
{
  let release!: () => void;
  holds = { "/api/threads/master/messages": new Promise<void>((r) => { release = r; }) };
  replies = {};
  const a = await render([rows.blocked], false);
  const atd = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(a.card("blocked").querySelector(".todo-hit"));
  await settle(() => assert(atd(), "drawer opens"));
  await click(labelled(atd()!, "Fix it"));
  await a.done();
  const b = await render([rows.blocked], false, { keepLocal: true });
  const btd = () => document.querySelector('[data-drawer="todo-detail"]');
  await click(b.card("blocked").querySelector(".todo-hit"));
  await settle(() => assert(btd(), "remounted drawer opens"));
  const again = labelled(btd()!, "Fix it");
  assert(again && isDisabled(again), 'a remount mid-send does not offer "Fix it" again');
  await click(again);
  release();
  holds = {};
  await settle(() => assert(/Sent to Master/.test(b.card("blocked").textContent ?? "") && !labelled(btd()!, "Fix it"), "the send lands as sent"));
  assert(posts.filter((p) => p.path === "/api/threads/master/messages").length === 1, "one send, not two");
  await b.done();
}

// ---- "Take my approval back" is unavailable once the board turns read-only ----
{
  approval = { state: "in-force", approval_id: "a1", board_revocable: true };
  const v = await render([rows.merge], false);
  await click(v.card("merge_decision").querySelector(".todo-hit"));
  const drawer = () => document.querySelector('[data-drawer="review"]')!;
  await settle(() => assert(labelled(drawer(), "Take my approval back"), "an approval on record can be taken back"));
  assert(!isDisabled(labelled(drawer(), "Take my approval back")!), "writable: take-back is enabled");
  await v.update([rows.merge], true);
  assert(!labelled(drawer(), "Take my approval back"), "read-only hides take-back");
  assert(!posts.some((p) => /^\/api\/approvals\/[^/]+\/revoke$/.test(p.path)), "read-only sends no revoke request");
  approval = { state: "missing" };
  await v.done();
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
  await click(c.querySelector(".todo-hit"));
  await settle(() => assert(document.querySelector('[data-drawer="todo-detail"]'), "generic drawer opens"));
  assert(labelled(document.querySelector('[data-drawer="todo-detail"]')!, "Fix it"), 'the generic drawer offers "Fix it"');
  await done();
}

// ---- 6. hosted merge review promotes Approve this version, sends optional issue/project context with 3-tuple 400 fallback, renders board pre-check verdicts, and shows CLI enqueue handoff ----
{
  issues["D-2"] = issue("D-2", {
    title: "Light the fuselage",
    body: "Visitors see a calm message.",
    reports: [],
  });
  approvalStateUrls.length = 0;
  strictThreeTupleApprove = true;
  const boardVerdicts = [{ name: "standards", agent: "svc_review_standards", verdict: "PASS", sha: SHA }];
  approval = { state: "missing", verdicts: boardVerdicts };
  const ro = await render([rows.merge], true, { hosted: true });
  await click(ro.card("merge_decision").querySelector(".todo-hit"));
  const drawer = () => document.querySelector('[data-drawer="review"]')!;
  await settle(() => assert(labelled(drawer(), "Approve this version"), "hosted merge renders primary Approve this version"));
  assert(!labelled(drawer(), "Publish"), "hosted merge hides local Publish button");
  assert(labelled(drawer(), "Ask for changes…"), "hosted merge renders Ask for changes…");
  assert(isDisabled(labelled(drawer(), "Approve this version")!), "read-only disables Approve this version");
  assert(isDisabled(labelled(drawer(), "Ask for changes…")!), "read-only disables Ask for changes…");
  await ro.done();

  const rw = await render([rows.merge], false, { hosted: true });
  approvalStateUrls.length = 0;
  await click(rw.card("merge_decision").querySelector(".todo-hit"));
  const d = () => document.querySelector('[data-drawer="review"]')!;
  await settle(() => assert(!isDisabled(labelled(d(), "Approve this version")!), "writable hosted approval enabled"));
  assert(
    approvalStateUrls.some((u) => u.includes("issue=D-2") && u.includes("project=demo")),
    `approvalState carries optional issue/project context (${JSON.stringify(approvalStateUrls)})`,
  );
  assert(
    (d().textContent ?? "").includes("Recorded review verdicts") &&
      (d().textContent ?? "").includes("svc_review_standards") &&
      (d().textContent ?? "").includes("matches this version"),
    "board pre-check verdicts render even when issue.reports is empty",
  );
  approval = { state: "in-force", approval_id: "a-hosted", board_revocable: true, verdicts: boardVerdicts };
  await click(labelled(d(), "Approve this version"));
  await settle(() => assert(labelled(d(), "Done"), "approved hosted merge switches primary to Done"));
  const approvePosts = posts.filter((p) => p.path === "/api/approvals/approve");
  assert(
    approvePosts.length === 2 &&
      approvePosts[0].body.issue === "D-2" &&
      approvePosts[0].body.project === "demo" &&
      approvePosts[1].body.repo === "acme/app" &&
      approvePosts[1].body.pr === 9 &&
      approvePosts[1].body.head === SHA &&
      Object.keys(approvePosts[1].body).length === 3,
    `approve sends optional context first and degrades cleanly to strict 3-tuple on 400 (${JSON.stringify(approvePosts)})`,
  );
  assert((d().textContent ?? "").includes("Approved on this board for this exact version."), "hosted approval status text");
  assert(labelled(d(), "Take my approval back"), "revocable take-back stays available");
  await click(labelled(d(), "Local enqueue command"));
  assert((d().textContent ?? "").includes(`scripts/enqueue-reviewed 9 --head ${SHA}`), "CLI enqueue handoff discloses exact command");
  await click(labelled(d(), "Take my approval back"));
  const revokeBox = d().querySelector(".rv-sendback textarea") as HTMLTextAreaElement | null;
  assert(revokeBox !== null, "take-back reason textarea expands");
  await React.act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    setter?.call(revokeBox, "Need one more check");
    revokeBox.dispatchEvent(new Event("input", { bubbles: true }));
    revokeBox.dispatchEvent(new Event("change", { bubbles: true }));
  });
  approval = { state: "revoked", approval_id: "a-hosted", board_revocable: false };
  await click(labelled(d(), "Take it back"));
  await settle(() => assert((d().textContent ?? "").includes("Your approval was taken back. A new version needs a fresh approval."), "revoked copy shown after take-back"));
  const revokePost = posts.find((p) => p.path === "/api/approvals/a-hosted/revoke");
  assert(revokePost && revokePost.body.reason === "Need one more check", `revoke payload sends trimmed reason (${JSON.stringify(revokePost?.body)})`);
  strictThreeTupleApprove = false;
  approval = { state: "missing" };
  await rw.done();
}

// ---- 7. CAD-2311: hosted vs non-hosted copy, stale verdict badge, and exact 40/64 hex safeHead guard ----
{
  const noIssueRow = {
    kind: "merge_decision", audience: "operator", title: "D-22 ready", age: 210, project: "demo", command: "cadence x",
    subject: { kind: "issue", id: "D-22" },
    merge: { issue: "D-22", pr: "acme/app#11", sha: SHA, reviewer: "r1", verdict_summary: "PASS" },
  };
  const localView = await render([noIssueRow], false, { hosted: false });
  await click(localView.card("merge_decision").querySelector(".todo-hit"));
  await settle(() => assert(document.querySelector('[data-review-verb="Publish change"]'), "non-hosted merge uses Publish change verb"));
  assert(
    (document.querySelector('[data-drawer="review"]')?.textContent ?? "").includes("Publish a change"),
    "non-hosted merge falls back to Publish a change title",
  );
  await localView.done();

  const hostedFallback = await render([noIssueRow], false, { hosted: true });
  await click(hostedFallback.card("merge_decision").querySelector(".todo-hit"));
  await settle(() => assert(document.querySelector('[data-review-verb="Approve change"]'), "hosted merge uses Approve change verb"));
  assert(
    (document.querySelector('[data-drawer="review"]')?.textContent ?? "").includes("Approve a change"),
    "hosted merge falls back to Approve a change title",
  );
  await hostedFallback.done();

  const oldSha = "b".repeat(40);
  approval = {
    state: "in-force",
    approval_id: "a-stale",
    board_revocable: true,
    verdicts: [
      { name: "standards", agent: "svc_review_standards", verdict: "PASS", sha: oldSha },
      { name: "spec", agent: "svc_review_spec", verdict: "PASS", sha: SHA },
    ],
  };
  const staleView = await render([rows.merge], false, { hosted: true });
  await click(staleView.card("merge_decision").querySelector(".todo-hit"));
  const staleDrawer = () => document.querySelector('[data-drawer="review"]')!;
  await settle(() => assert(labelled(staleDrawer(), "Local enqueue command"), "40-hex head exposes Local enqueue command"));
  const verdictText = staleDrawer().querySelector(".rv-verdicts")?.textContent ?? "";
  assert(verdictText.includes(`${oldSha.slice(0, 12)} · older version`), `mismatched verdict SHA renders older version badge (${verdictText})`);
  assert(verdictText.includes(`${SHA.slice(0, 12)} · matches this version`), `matching verdict SHA renders matches this version badge (${verdictText})`);
  await staleView.done();

  const sha64 = "d".repeat(64);
  const sha64Row = { ...rows.merge, merge: { ...rows.merge.merge, sha: sha64 } };
  approval = { state: "in-force", approval_id: "a-64", board_revocable: false, verdicts: [] };
  const view64 = await render([sha64Row], false, { hosted: true });
  await click(view64.card("merge_decision").querySelector(".todo-hit"));
  await settle(() => assert(labelled(document.querySelector('[data-drawer="review"]')!, "Local enqueue command"), "64-hex head exposes Local enqueue command"));
  await view64.done();

  const midLenRow = { ...rows.merge, merge: { ...rows.merge.merge, sha: "a".repeat(45) } };
  approval = { state: "in-force", approval_id: "a-mid", board_revocable: false, verdicts: [] };
  const midView = await render([midLenRow], false, { hosted: true });
  await click(midView.card("merge_decision").querySelector(".todo-hit"));
  await settle(() => assert(labelled(document.querySelector('[data-drawer="review"]')!, "Done"), "approved hosted drawer renders with 45-char head"));
  assert(
    !labelled(document.querySelector('[data-drawer="review"]')!, "Local enqueue command"),
    "45-char non-SHA hex head must not expose Local enqueue command",
  );
  approval = { state: "missing" };
  await midView.done();
}

// ---- 8. CAD-2590: To do category filter strip renders only when >1 category is present ----
{
  const single = await render([rows.stopped], false);
  assert(
    single.host.querySelector('[aria-label="Filter to do"]') === null,
    "single-category To do rail omits filter strip",
  );
  await single.done();

  const multi = await render([rows.merge, rows.stopped], false);
  const filterGroup = multi.host.querySelector('[aria-label="Filter to do"]');
  assert(filterGroup !== null, "multi-category To do rail renders filter strip");
  assert(multi.host.querySelectorAll(".todo-card").length === 2, "All shows both cards");
  const blockedPill = buttons(filterGroup).find((b) => b.textContent?.includes("Blocked"));
  assert(blockedPill, "Blocked pill present when blocked card exists");
  await click(blockedPill);
  assert(multi.host.querySelectorAll(".todo-card").length === 1, "Blocked filter narrows to 1 card");
  const allPill = buttons(filterGroup).find((b) => b.textContent?.includes("All"));
  assert(allPill, "All pill present in filter strip");
  await click(allPill);
  assert(multi.host.querySelectorAll(".todo-card").length === 2, "All filter restores both cards");
  await multi.done();
}

// ---- 9. CAD-2590: read-only Member Home still renders agent Feed without Ask action ----
{
  agentsReply = {
    agents: [
      { alias: "writer-1", state: "running", running: 1, queued: 0, last_activity: new Date().toISOString() },
    ],
    by_issue: {},
    daemon: "reachable",
  };
  await resources.agents.refresh();
  resetTodoLocal();
  posts.length = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await React.act(async () => {
    root.render(
      React.createElement(RailWithDetail, {
        overview: undefined,
        readOnly: true,
        onOpenIssue: () => {},
        onAskAgent: undefined,
        hasLiveAgents: true,
      }),
    );
  });
  await flush();
  const feedTab = buttons(host).find((b) => b.textContent?.trim().startsWith("Feed"));
  assert(feedTab, "Feed tab present on read-only Member Home");
  await click(feedTab);
  await settle(() => assert(host.querySelector('[data-agent="writer-1"]'), "read-only Member Home renders agent Feed card"));
  const cardBtn = host.querySelector('[data-agent="writer-1"] .rc-title') as HTMLButtonElement | null;
  await click(cardBtn);
  const feedDrawer = () => document.querySelector('[data-drawer="feed-detail"]') as HTMLElement | null;
  await settle(() => assert(feedDrawer(), "read-only Feed drawer opens"));
  assert((feedDrawer()!.textContent ?? "").includes("Talk to writer-1"), "Feed drawer preserves Talk to writer-1 link");
  assert(
    !buttons(feedDrawer()!).some((b) => (b.textContent ?? "").includes("Ask")),
    "read-only Feed drawer omits Ask action",
  );
  assert(
    !buttons(feedDrawer()!).some((b) => ["Approve", "Approve & send", "Deny"].includes(b.textContent?.trim() ?? "")),
    "Feed drawer exposes no approval buttons",
  );
  agentsReply = { agents: [], by_issue: {} };
  await resources.agents.refresh();
  await React.act(async () => { root.unmount(); });
  host.remove();
}

console.log("todo guard checks passed");
}
void main();
