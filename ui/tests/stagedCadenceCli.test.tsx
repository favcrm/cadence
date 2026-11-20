export {};
/**
 * AOS-679 / CAD-2544: a hosted Cadence CLI write staged from Home
 * (`tool: "cadence_cli"`), read and shown for the owner's decision.
 *
 *  - the verb, summary, asker and every change render, a long body unclipped;
 *  - a `set` pair that contains `=` stays one whole entry;
 *  - bidi and zero-width characters are made visible, as text;
 *  - `changes_complete` that is not `true` is not approvable (Deny still is);
 *  - Approve and Deny use the existing action routes;
 *  - a malformed row, or an unknown tool, is dropped and the list survives.
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
const { resetTodoLocal } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
const { homeDecisions } = require("../src/features/decisions/useDecisions") as typeof import("../src/features/decisions/useDecisions");
const { parseStagedRow, parseStagedList, approvable } = require("../src/features/decisions/stagedWrite") as typeof import("../src/features/decisions/stagedWrite");

const BODY = "Line one of a long body. ".repeat(80) + "THE-END-MARKER";
const RLO = "‮";
const ZWSP = "​";
const change = (field: string, after: string) => ({ field, label: field, kind: "added", after });
const cliRow = (id: string, extra: Record<string, unknown> = {}) => {
  const args = { project: "ops", title: "Ship the gift pack", body: BODY, owner: "agents/atlas", set: ["priority=high", "note=a=b=c"] };
  return {
    id, tool: "cadence_cli", status: "pending", createdAt: Date.now() - 120_000, decidedAt: null,
    input: { verb: "issue_new", args: JSON.stringify(args) },
    title: "Cadence: issue new", summary: "ops: Ship the gift pack", verb: "issue_new",
    args: JSON.stringify(args), requested_by: "Home assistant",
    origin: { via: "home", kind: "agent", label: "Home assistant" },
    changes: [
      change("project", "ops"), change("title", "Ship the gift pack"), change("body", BODY), change("owner", "agents/atlas"),
      change("set", "priority=high"), change("set", "note=a=b=c"),
    ],
    unchanged_count: 0, changes_complete: true, result: null,
    ...extra,
  };
};

// ---- 1. parsing, directly ----
{
  const w = parseStagedRow(cliRow("c1"));
  assert(w !== null && w.kind === "cadence_cli", "a cadence_cli row parses into its own kind");
  assert(w.title === "Cadence: issue new" && w.cli?.verb === "issue_new" && w.cli.summary === "ops: Ship the gift pack" && w.cli.requestedBy === "Home assistant", "verb, summary, asker");
  assert(w.changes.length === 6 && w.changes.every((c) => c.kind === "added") && w.changesComplete && approvable(w), "all six changes, complete, approvable");
  assert(w.changes[2].after === BODY && w.changes[2].after!.endsWith("THE-END-MARKER"), "the long body is whole");
  assert(w.changes[5].field === "set" && w.changes[5].after === "note=a=b=c", "a set pair containing = stays whole");

  const v = parseStagedRow(cliRow("c2", { changes: [change("title", `pay${RLO}evil${ZWSP}x`)] }));
  assert(v !== null && v.changes[0].after === "pay[U+202E]evil[U+200B]x", `bidi and zero-width made visible (got ${v?.changes[0].after})`);

  for (const bad of [{ changes_complete: false }, { changes_complete: "true" }, { changes_complete: undefined }]) {
    const n = parseStagedRow(cliRow("c3", bad));
    assert(n !== null && !n.changesComplete && !approvable(n) && n.changes.length === 0 && n.cli?.summary === null, `${JSON.stringify(bad)} is not approvable and carries no summary`);
  }
  const malformed = [
    cliRow("m1", { verb: undefined, input: {} }),
    cliRow("m2", { changes: "nope" }),
    cliRow("m3", { changes: [{ field: 7, kind: "added" }] }),
    cliRow("m4", { status: "weird" }),
    { ...cliRow("m5"), tool: "cadence_cli_v2" },
    { ...cliRow("m6"), tool: "mystery_tool" },
  ];
  for (const m of malformed) assert(parseStagedRow(m) === null, `${m.id} is dropped`);
  const list = parseStagedList({ actions: [...malformed, cliRow("ok")] });
  assert(list !== null && list.length === 1 && list[0].id === "ok", "a malformed row never crashes or hides the list");
}

// ---- a fake board ----
let actions: unknown[] = [];
let listCalls = 0;
const posts: string[] = [];
const seen = (): string[] => posts.slice();
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname === "/api/assist/actions") {
    listCalls++;
    return json({ actions });
  }
  const m = /^\/api\/assist\/actions\/([^/]+)\/(approve|deny)$/.exec(url.pathname);
  if (m && init?.method === "POST") {
    posts.push(url.pathname);
    actions = (actions as Record<string, unknown>[]).map((a) => (a.id === m[1] ? { ...a, status: m[2] === "approve" ? "approved" : "denied", decidedAt: Date.now(), result: m[2] === "approve" ? { ok: false, code: "not_found" } : null } : a));
    return json({ ok: true });
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
const sleep = (ms: number) => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, ms)); });
async function click(element: Element | undefined | null) {
  assert(element, "click target exists");
  const label = element.textContent?.trim();
  if (element.closest('[data-drawer="staged"]') && (label === "Approve" || label === "Deny")) await sleep(850);
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 1 } as any)); });
  await flush();
}
const buttons = (scope: ParentNode) => Array.from(scope.querySelectorAll("button")) as HTMLButtonElement[];
const labelled = (scope: ParentNode, text: string) => buttons(scope).find((b) => b.textContent?.trim() === text);
const drawerEl = () => document.querySelector('.crm-drawer[data-drawer="staged"]') as HTMLElement | null;
const text = (el: Element | null) => el?.textContent?.replace(/[⁨⁩]/g, "").replace(/\s+/g, " ").trim() ?? "";

async function render() {
  resetTodoLocal();
  posts.length = 0;
  listCalls = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const overview = { status: "ok", inFlight: false, error: null, asOf: Date.now(), data: { needs_me: [] } } as unknown as ResourceState<Overview>;
  await React.act(async () => {
    root.render(
      React.createElement(NeedsRail, {
        overview, readOnly: false, onOpenIssue: () => {}, overviewHref: "/overview", permissionsHref: "/settings/permissions",
        onAsk: () => {}, onOpenDecision: () => {}, collapsed: false, onToggleCollapse: () => {}, onAskAgent: () => {}, decisions: true,
      }),
    );
  });
  await settle(() => assert(listCalls > 0, "the list is read"));
  const cards = () => Array.from(host.querySelectorAll('[data-need="staged_write"]')) as HTMLElement[];
  const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); homeDecisions.resetDecisions(); };
  return { host, cards, done };
}

// ---- 2. a full row in Home: every change, whole, as text; approve uses the existing route ----
{
  actions = [cliRow("c1"), { ...cliRow("bad"), changes: "nope" }, { ...cliRow("odd"), tool: "mystery_tool" }];
  const v = await render();
  await settle(() => assert(v.cards().length === 1, "only the renderable cadence_cli row is a card"));
  assert(text(v.cards()[0].querySelector(".todo-title")) === "Cadence: issue new", "the verb is the title");
  assert(/Home assistant/.test(text(v.cards()[0].querySelector(".todo-meta"))), "the asker is on the card");
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "the drawer opens"));
  const d = drawerEl()!;
  const body = text(d);
  const rows = Array.from(d.querySelectorAll(".dx-row")).map((r) => `${text(r.querySelector("dt"))}: ${text(r.querySelector("dd"))}`);
  assert(rows.length === 6, `all six changes render (${rows.length})`);
  assert(rows.includes("set: priority=high") && rows.includes("set: note=a=b=c"), "each set pair is its own whole row");
  assert(rows.includes("owner: agents/atlas"), "the owner field is listed");
  assert(rows.some((r) => r.endsWith("THE-END-MARKER")) && !body.includes("…"), "the long body is not clipped");
  assert(body.includes("Summary: ops: Ship the gift pack") && /Asked by Home assistant/.test(body), "summary and asker");
  assert(labelled(d, "Approve")?.getAttribute("aria-disabled") !== "true", "Approve is on");
  await click(labelled(d, "Approve"));
  assert(seen().join() === "/api/assist/actions/c1/approve", `approve uses the existing route (${seen().join(",")})`);
  await settle(() => assert(listCalls > 1, "the list is re-read"));
  await v.done();
}

// ---- 3. bidi and zero-width characters are visible in the rendered row ----
{
  actions = [cliRow("c1", { changes: [change("title", `pay${RLO}evil${ZWSP}x`)] })];
  const v = await render();
  await settle(() => assert(v.cards().length === 1, "a card"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "the drawer opens"));
  const rows = Array.from(drawerEl()!.querySelectorAll(".dx-row")).map((r) => text(r.querySelector("dd")));
  assert(rows[0] === "pay[U+202E]evil[U+200B]x", `shown as text (${rows[0]})`);
  assert(!drawerEl()!.textContent!.includes(RLO) && !drawerEl()!.textContent!.includes(ZWSP), "no raw control character reaches the page");
  await v.done();
}

// ---- 4. an incomplete row is not reviewable: no summary, Approve off, Deny still runs ----
{
  actions = [cliRow("c1", { changes_complete: false })];
  const v = await render();
  await settle(() => assert(v.cards().length === 1, "a card"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "the drawer opens"));
  const d = drawerEl()!;
  assert(text(d).includes("cannot be reviewed here"), "a clear note");
  assert(!text(d).includes("Ship the gift pack") || d.querySelector("pre"), "no summary; only the raw call may carry the text");
  assert(!text(d).includes("Summary:") && d.querySelectorAll(".dx-row").length === 0, "no summary and no partial change list");
  assert(labelled(d, "Approve")?.getAttribute("aria-disabled") === "true", "Approve is disabled");
  await click(labelled(d, "Approve"));
  assert(seen().length === 0, "a click on the disabled Approve posts nothing");
  await click(labelled(d, "Deny"));
  assert(seen().join() === "/api/assist/actions/c1/deny", "Deny uses the existing route");
  await v.done();
}

console.log("staged cadence_cli checks passed");
}
void main();
