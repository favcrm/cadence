export {};
/**
 * CAD-2186: owner decisions staged by the assistant or the hosted CLI, in the
 * Home rail's To do and a detail drawer, checked on the rendered rail.
 *
 *  - pending `app_call` and `board_create_issue` rows merge into To do (never
 *    through `needs_me`); the count and the badge include them;
 *  - a board with no route (404/501) or a refused viewer stays as it was;
 *  - the detail shows changes against the current record, no `revision`, a raw
 *    call that opens without remounting the drawer;
 *  - one submit per decision, no retry, an unconfirmed reply re-reads;
 *  - the queue advances inside the same drawer node and closes after the last;
 *  - a malformed row is not a decision.
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
const NeedsRail = (require("../src/features/home/NeedsRail") as typeof import("../src/features/home/NeedsRail")).default;
const { resetTodoLocal, useHomeCount } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
const { resetDecisions, useDecisions } = require("../src/features/decisions/useDecisions") as typeof import("../src/features/decisions/useDecisions");
const { parseStagedRow } = require("../src/features/decisions/stagedWrite") as typeof import("../src/features/decisions/stagedWrite");

// ---- a fake board: the staged list and the decide routes ----
const AGO = 2 * 60_000;
const customer = (id: string, extra: Record<string, unknown> = {}) => ({
  id, tool: "app_call", status: "pending", createdAt: Date.now() - AGO, decidedAt: null,
  title: "Update Marlow Books", app: "CRM",
  target: { kind: "customer", label: "Marlow Books", id: "cus_91" },
  changes: [
    { field: "stage", label: "Stage", kind: "changed", before: "Qualified", after: "Proposal sent" },
    { field: "owner", label: "Owner", kind: "added", after: "Priya Nair" },
    { field: "phone", label: "Phone", kind: "changed", before: "+852 1111", after: null },
  ],
  unchanged_count: 3, changes_complete: true,
  reason: "Closed as lost; their phone is out of date.",
  consent: { channel: "whatsapp", captured: "2026-10-30" },
  effects: ["Runs as you. Nothing is sent to anyone."],
  args: JSON.stringify({ record_id: "cus_91", expected_revision: 7, profile: { stage: "Proposal sent" } }),
  origin: { via: "cli", kind: "agent", label: "Atlas" },
  result: null,
  ...extra,
});
const issueRow = (id: string) => ({
  id, tool: "board_create_issue", status: "pending", createdAt: Date.now() - AGO, decidedAt: null,
  input: { title: "Add the gift pack to the checklist", project: "OPS", priority: "high", body: "Every stockist asks." },
});
let actions: unknown[] = [];
let listStatus = 200;
let listCalls = 0;
const posts: { path: string; body: any }[] = [];
let decideStatus: Record<string, number> = {};
let decideThrows: Record<string, boolean> = {};
let onDecide: ((id: string, verb: string) => void) | null = null;
let hold: Promise<void> | null = null;
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname === "/api/assist/actions") {
    listCalls++;
    return listStatus === 200 ? json({ actions }) : json({ error: { code: "x" } }, listStatus);
  }
  const m = /^\/api\/assist\/actions\/([^/]+)\/(approve|deny)$/.exec(url.pathname);
  if (m && init?.method === "POST") {
    posts.push({ path: url.pathname, body: JSON.parse(String(init.body ?? "{}")) });
    await hold;
    if (decideThrows[m[1]]) throw new TypeError("network");
    const status = decideStatus[m[1]] ?? 200;
    if (status === 200) onDecide?.(m[1], m[2]);
    return json(status === 200 ? { ok: true } : { error: { code: "refused" } }, status);
  }
  return json({});
}) as typeof fetch;

const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
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
const drawerEl = () => document.querySelector('.crm-drawer[data-drawer="staged"]') as HTMLElement | null;
const sent = (): number => posts.length;
const text = (el: Element | null) => el?.textContent?.replace(/\s+/g, " ").trim() ?? "";

function Badge({ needs, owner }: { needs: unknown[]; owner: boolean }) {
  const decisions = useDecisions(owner);
  return React.createElement("span", { "data-badge": true }, String(useHomeCount(needs as never, decisions.pending.length)));
}

async function render(opts: { needs?: unknown[]; decisions?: boolean; readOnly?: boolean } = {}) {
  resetTodoLocal();
  posts.length = 0;
  listCalls = 0;
  const needs = opts.needs ?? [];
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const overview = { status: "ok", inFlight: false, error: null, asOf: Date.now(), data: { needs_me: needs } } as unknown as ResourceState<Overview>;
  await React.act(async () => {
    root.render(
      React.createElement(
        React.Fragment,
        null,
        React.createElement(Badge, { needs, owner: opts.decisions ?? true }),
        React.createElement(NeedsRail, {
          overview, readOnly: opts.readOnly ?? false, onOpenIssue: () => {}, overviewHref: "/overview", permissionsHref: "/settings/permissions",
          onAsk: () => {}, onOpenDecision: () => {}, collapsed: false, onToggleCollapse: () => {}, onAskAgent: () => {},
          decisions: opts.decisions ?? true,
        }),
      ),
    );
  });
  await settle(() => assert(listCalls > 0 || opts.decisions === false, "the list is read"));
  await flush();
  const cards = () => Array.from(host.querySelectorAll('[data-need="staged_write"]')) as HTMLElement[];
  const badge = () => host.querySelector("[data-badge]")!.textContent;
  const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); resetDecisions(); };
  return { host, cards, badge, done };
}

// ---- 1. staged writes join To do; the count and badge include them; the card stays compact ----
{
  actions = [customer("a1"), issueRow("a2"), { id: "bad", tool: "app_call", status: "pending", createdAt: 1, title: "x", changes: "nope" }];
  const v = await render({ needs: [{ kind: "blocked", audience: "operator", title: "D-5 blocked", age: 60, project: "demo", command: "x", subject: { kind: "issue", id: "D-5" } }] });
  await settle(() => assert(v.cards().length === 2, "both renderable pending rows are cards; the malformed row is not"));
  assert(v.badge() === "3", `the badge counts needs_me plus staged (got ${v.badge()})`);
  assert(text(v.host.querySelector(".todo-count")) === "3", "the To do count includes them");
  assert(!/Nothing needs you/.test(text(v.host)), "Nothing needs you is untrue while any waits");
  const c = v.cards()[0];
  assert(c.getAttribute("data-type") === "ok", "type ok");
  assert(text(c.querySelector(".todo-title")) === "Update Marlow Books", "title");
  assert(text(c.querySelector(".todo-meta")) === "CRM · Atlas (agent) · 2 min", `meta line (got "${text(c.querySelector(".todo-meta"))}")`);
  assert(labelled(c, "Review"), "a Review button");
  assert(!c.querySelector(".needmore"), "no snooze/dismiss menu: it would act on needs_me");
  await click(c.querySelector(".todo-hit"));
  assert(!c.hasAttribute("data-open") && text(c) === text(v.cards()[0]) && !c.querySelector("dl, pre"), "nothing expands in the rail");
  assert(!document.querySelector('.crm-drawer[data-drawer="review"]'), "not the issue-backed drawer");
  await v.done();
}

// ---- 2. a board with no route, a refused viewer, or no ownership stays as it is ----
for (const status of [404, 501, 403]) {
  listStatus = status;
  actions = [customer("a1")];
  const v = await render();
  await settle(() => assert(listCalls > 0, "asked once"));
  assert(v.cards().length === 0, `${status}: no cards`);
  assert(v.badge() === "0", `${status}: no count`);
  assert(/Nothing needs you/.test(text(v.host)), `${status}: the rail says nothing needs you`);
  await v.done();
}
listStatus = 200;
{
  const v = await render({ decisions: false });
  assert(listCalls === 0, "a board that is not asked is never queried");
  assert(v.cards().length === 0 && v.badge() === "0", "and shows no decisions");
  await v.done();
}

// ---- 3. the detail: changes against the record, a quiet header, raw closed ----
{
  actions = [customer("a1"), customer("a2", { title: "Update Harbour Tea" })];
  const v = await render();
  await settle(() => assert(v.cards().length === 2, "two cards"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "the staged drawer opens"));
  const d = drawerEl()!;
  assert(d.getAttribute("aria-modal") === "true", "the drawer is modal (it traps focus)");
  const body = text(d);
  const rows = Array.from(d.querySelectorAll(".dx-row")).map((r) => `${text(r.querySelector("dt"))}: ${text(r.querySelector("dd"))}`);
  assert(text(d.querySelector(".crm-drawer-title")) === "Update Marlow Books", "title");
  assert(/CRM · Marlow Books/.test(text(d.querySelector(".crm-drawer-sub"))), "app and target");
  assert(/Asked by Atlas \(agent\), via CLI · 2 min/.test(body), `asked by (got ${body.slice(0, 200)})`);
  assert(rows.includes("Stage: Qualified → Proposal sent"), `a changed field reads before → after (${rows.join("|")})`);
  assert(rows.includes("Owner: — → Priya Nair"), "an added field on an update reads — → value");
  assert(rows.includes("Phone: +852 1111 → (cleared)"), "a value set to null reads (cleared)");
  assert(!rows.some((r) => r.startsWith("Removed")), "an update removes nothing");
  assert(body.includes("3 fields unchanged"), "the unchanged count");
  assert(!/revision/i.test(body), "revision lives only under Show raw");
  assert(body.includes("Atlas's note · not verified") && body.includes("Closed as lost"), "the agent's note, labelled");
  assert(body.includes("Consent supplied with this write") && body.includes("channel: whatsapp"), "consent");
  assert(body.includes("Runs as you. Nothing is sent to anyone."), "one effects line, as given");
  assert(!d.querySelector("pre"), "raw is closed");
  assert(body.indexOf("Stage") < body.indexOf("Atlas's note") && body.indexOf("Atlas's note") < body.indexOf("Consent") && body.indexOf("Consent") < body.indexOf("Runs as you"), "order: changes, note, consent, effects");
  const foot = d.querySelector(".crm-drawer-foot")!;
  assert(["Approve", "Deny", "Not now"].every((n) => labelled(foot, n)), "footer: Approve, Deny, Not now");
  assert(text(foot).includes("1 of 2"), "n of m");

  // Show raw: the same drawer, scrim and title nodes; nothing mounted or animated again.
  const scrim = document.querySelector('.crm-drawer-scrim[data-scrim="board"]');
  const title = d.querySelector(".crm-drawer-title");
  const added: string[] = [];
  const watch = new MutationObserver((records: any[]) => {
    for (const r of records) for (const n of Array.from(r.addedNodes as Node[])) if (n instanceof Element && (n.matches(".crm-drawer, .crm-drawer-scrim") || n.querySelector(".crm-drawer, .crm-drawer-scrim"))) added.push(n.className);
  });
  watch.observe(document.body, { childList: true, subtree: true });
  await click(labelled(d, "Show raw"));
  assert(d.querySelector("pre")?.textContent?.includes('"expected_revision": 7'), "the exact call, with its revision");
  assert(labelled(d, "Hide raw")?.getAttribute("aria-expanded") === "true", "disclosure state");
  await click(labelled(d, "Hide raw"));
  assert(!d.querySelector("pre"), "closed again");
  await flush();
  watch.disconnect();
  assert(added.length === 0, `Show raw mounted nothing new (${added.join(",")})`);
  assert(drawerEl() === d && document.querySelector('.crm-drawer-scrim[data-scrim="board"]') === scrim && d.querySelector(".crm-drawer-title") === title, "same nodes");
  await v.done();
}

// ---- 4. an incomplete summary says so and opens raw; an old API row falls back ----
{
  actions = [customer("a1", { changes_complete: false }), { id: "old", tool: "app_call", status: "pending", createdAt: Date.now() - AGO, input: { subject: "crm: records.update (record cus_9) - stage: Won", app: "crm", operation: "records.update", args: '{"record_id":"cus_9"}' } }];
  const v = await render();
  await settle(() => assert(v.cards().length === 2, "both"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  assert(text(drawerEl()).includes("This summary may be incomplete"), "the incomplete summary is named");
  assert(drawerEl()!.querySelector("pre"), "raw is open by default when the summary is incomplete");
  assert(!Array.from(drawerEl()!.querySelectorAll(".dx-row dd")).some((e) => text(e).startsWith("—")), "no comparison is implied when none was made");
  await click(labelled(drawerEl()!, "Not now"));
  await settle(() => assert(text(drawerEl()!.querySelector(".crm-drawer-title")) === "crm: records.update (record cus_9) - stage: Won", "the old row shows its own sentence"));
  assert(text(drawerEl()!).includes("This summary may be incomplete") && drawerEl()!.querySelector("pre")?.textContent?.includes("cus_9"), "and its raw call, summary treated as incomplete");
  await v.done();
}

// ---- 5. a malformed row is not a decision ----
{
  const ok = parseStagedRow(customer("z"));
  assert(ok && ok.changes.length === 3, "a well-formed row parses");
  for (const bad of [
    customer("z", { changes: [{ field: "stage", kind: "weird" }] }),
    customer("z", { result: { ok: "yes" } }),
    customer("z", { status: "maybe" }),
    { ...customer("z"), createdAt: "now" },
    { id: "z", tool: "app_call", status: "pending", createdAt: 1 },
    null,
  ]) assert(parseStagedRow(bad) === null, "malformed rows are dropped, never success");
}

// ---- 6. approve once; the queue advances in the same drawer; the last one closes ----
{
  actions = [customer("a1"), customer("a2", { title: "Update Harbour Tea" })];
  onDecide = (id, verb) => {
    actions = actions.map((a: any) => a.id === id ? { ...a, status: verb === "approve" ? "approved" : "denied", decidedAt: Date.now(), result: verb === "approve" ? { ok: true, record: { id: "cus_91" } } : null } : a);
  };
  const v = await render();
  await settle(() => assert(v.cards().length === 2, "two cards"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  const d = drawerEl()!;
  let release!: () => void;
  hold = new Promise<void>((r) => { release = r; });
  const approve = labelled(d, "Approve")!;
  await click(approve);
  await click(approve);
  await click(labelled(d, "Deny"));
  assert(sent() === 1 && posts[0].path === "/api/assist/actions/a1/approve", `one submit while one is in flight (${posts.length})`);
  assert(approve.getAttribute("aria-busy") === "true" && text(approve).includes("Approving"), "approving state");
  release(); hold = null;
  await settle(() => assert(text(drawerEl()!.querySelector(".crm-drawer-title")) === "Update Harbour Tea", "advanced to the next waiting item"));
  assert(drawerEl() === d, "the same drawer node: no new open animation");
  assert(text(d.querySelector(".crm-drawer-foot")).includes("2 of 2"), "2 of 2");
  assert(text(d).includes("Applied · Update Marlow Books"), "the outcome is said");
  assert(document.activeElement === d.querySelector(".crm-drawer-title"), "focus lands on the new title");
  await click(labelled(d, "Deny"));
  await settle(() => assert(!drawerEl(), "the last decision closes the drawer"));
  assert(sent() === 2 && posts[1].path === "/api/assist/actions/a2/deny", "deny posts to deny");
  assert(v.cards().length === 0 && v.badge() === "0", "nothing waits; the count drops");
  await tick(); await tick();
  assert(document.activeElement && v.host.contains(document.activeElement), "focus returns to the rail");
  await click(labelled(v.host, "Updates"));
  const lines = text(v.host.querySelector("[data-todo-decisions]"));
  assert(lines.includes("Update Marlow Books") && lines.includes("Applied") && lines.includes("Denied"), `decided lines in Updates (got ${lines})`);
  onDecide = null;
  await v.done();
}

// ---- 7. a failed apply: plain reason, code under raw, a dot on Updates until viewed ----
{
  actions = [customer("a1")];
  onDecide = (id) => {
    actions = actions.map((a: any) => a.id === id ? { ...a, status: "approved", decidedAt: Date.now(), result: { ok: false, code: "revision_conflict" } } : a);
  };
  const v = await render();
  await settle(() => assert(v.cards().length === 1, "one card"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  await click(labelled(drawerEl()!, "Approve"));
  await settle(() => assert(!drawerEl(), "closed"));
  assert(sent() === 1, "no retry of a failed write");
  assert(text(v.host.querySelector(".todo-note")).includes("did not apply: the record changed after it was staged"), "the plain reason");
  assert(v.host.querySelector(".dx-dot"), "a dot on Updates");
  await click(labelled(v.host, "Updates"));
  const line = v.host.querySelector("[data-fail]")!;
  assert(text(line).includes("Did not apply") && text(line).includes("the record changed after it was staged"), "the failed line says why");
  assert(!line.querySelector("pre"), "the code is under raw");
  await click(labelled(line, "Show raw"));
  assert(line.querySelector("pre")?.textContent?.includes("revision_conflict"), "code under raw");
  assert(!v.host.querySelector(".dx-dot"), "viewing Updates clears the dot");
  await click(buttons(v.host).find((b) => b.textContent?.startsWith("To do")));
  assert(!v.host.querySelector(".dx-dot"), "and it stays cleared");
  onDecide = null;
  await v.done();
}

// ---- 8. network unconfirmed: re-read, never resubmit; refusals ----
{
  actions = [customer("a1")];
  const v = await render();
  await settle(() => assert(v.cards().length === 1, "one card"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  decideThrows.a1 = true;
  await click(labelled(drawerEl()!, "Approve"));
  await settle(() => assert(text(drawerEl()!).includes("We could not confirm the result"), "unconfirmed is said"));
  assert(!labelled(drawerEl()!, "Approve") && !labelled(drawerEl()!, "Deny"), "no Approve or Deny while unsure");
  decideThrows.a1 = false;
  const before = sent();
  await click(labelled(drawerEl()!, "Check again"));
  await settle(() => assert(labelled(drawerEl()!, "Approve"), "still pending: nothing was recorded, so Approve is back"));
  assert(sent() === before, "Check again re-reads and posts nothing");
  assert(text(drawerEl()!).includes("still waiting"), "and says so");
  // It had gone through after all.
  decideThrows.a1 = true;
  await click(labelled(drawerEl()!, "Approve"));
  await settle(() => assert(labelled(drawerEl()!, "Check again"), "unsure again"));
  decideThrows.a1 = false;
  actions = actions.map((a: any) => ({ ...a, status: "approved", decidedAt: Date.now(), result: { ok: true } }));
  const n = sent();
  await click(labelled(drawerEl()!, "Check again"));
  await settle(() => assert(!drawerEl(), "it had gone through: the queue ends"));
  assert(sent() === n, "still no second submit");
  await v.done();

  actions = [customer("a1")];
  const w = await render();
  await settle(() => assert(w.cards().length === 1, "one card"));
  await click(labelled(w.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  decideStatus.a1 = 403;
  await click(labelled(drawerEl()!, "Approve"));
  await settle(() => assert(text(drawerEl()!).includes("Only the workspace owner"), "access lost is said"));
  assert(drawerEl()!.querySelector(".crm-drawer-foot [aria-disabled='true']"), "and decisions are off");
  decideStatus.a1 = 200;
  await w.done();

  actions = [customer("a1")];
  const x = await render();
  await settle(() => assert(x.cards().length === 1, "one card"));
  await click(labelled(x.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  decideStatus.a1 = 409;
  actions = actions.map((a: any) => ({ ...a, status: "denied", decidedAt: Date.now() }));
  await click(labelled(drawerEl()!, "Approve"));
  await settle(() => assert(text(drawerEl()!).includes("already decided elsewhere"), "decided elsewhere is said"));
  await click(labelled(drawerEl()!, "Continue"));
  await settle(() => assert(!drawerEl(), "and Continue ends the queue"));
  decideStatus.a1 = 200;
  await x.done();
}

// ---- 9. Not now decides nothing; read-only decides nothing; no shortcut approves; Tab stays inside ----
{
  actions = [customer("a1"), customer("a2", { title: "Update Harbour Tea" })];
  const v = await render();
  await settle(() => assert(v.cards().length === 2, "two cards"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  const d = drawerEl()!;
  for (const key of ["Enter", "a", "y", "Meta"]) await React.act(async () => { document.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true })); });
  assert(sent() === 0, "no shortcut approves");
  // Tab from the last control wraps to the first; Shift+Tab from the title wraps to the last.
  const tabbable = Array.from(d.querySelectorAll('button:not([disabled]), a[href], pre[tabindex="0"]')) as HTMLElement[];
  tabbable[tabbable.length - 1].focus();
  const tab = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
  await React.act(async () => { document.dispatchEvent(tab); });
  assert(tab.defaultPrevented && document.activeElement === tabbable[0], "Tab wraps to the first control");
  (d.querySelector(".crm-drawer-title") as HTMLElement).focus();
  const back = new KeyboardEvent("keydown", { key: "Tab", shiftKey: true, bubbles: true, cancelable: true });
  await React.act(async () => { document.dispatchEvent(back); });
  assert(back.defaultPrevented && document.activeElement === tabbable[tabbable.length - 1], "Shift+Tab wraps to the last control");
  await click(labelled(d, "Not now"));
  await settle(() => assert(text(drawerEl()!.querySelector(".crm-drawer-title")) === "Update Harbour Tea", "Not now moves on"));
  assert(sent() === 0 && v.cards().length === 2, "and decided nothing");
  await click(labelled(drawerEl()!, "Not now"));
  await settle(() => assert(!drawerEl(), "Not now on the last closes"));
  assert(v.cards().length === 2 && v.badge() === "2", "both still wait");
  await v.done();

  const r = await render({ readOnly: true });
  await settle(() => assert(r.cards().length === 2, "two cards"));
  await click(labelled(r.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  const approve = labelled(drawerEl()!, "Approve")!;
  assert(approve.getAttribute("aria-disabled") === "true", "read-only turns Approve off");
  await click(approve);
  assert(sent() === 0, "and it sends nothing");
  await React.act(async () => { document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); });
  await settle(() => assert(!drawerEl(), "Escape closes"));
  await r.done();
}

console.log("staged decision checks passed");
}
void main();
