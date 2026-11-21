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
import type * as StagedWriteModule from "../src/features/decisions/stagedWrite";

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
const ConversationDecisions = (require("../src/features/app-shell/chat/ConversationDecisions") as typeof import("../src/features/app-shell/chat/ConversationDecisions")).default;
const RecruitChat = (require("../src/features/agents/RecruitChat") as typeof import("../src/features/agents/RecruitChat")).default;
const { resetTodoLocal, useHomeCount } = require("../src/features/home/todoLocal") as typeof import("../src/features/home/todoLocal");
const { homeDecisions, useDecisions } = require("../src/features/decisions/useDecisions") as typeof import("../src/features/decisions/useDecisions");
const { parseStagedRow, parseStagedList, askedBy, sourceLine, outcomeOf, stagedDomain } = require("../src/features/decisions/stagedWrite") as typeof StagedWriteModule;
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
const scopedActions = new Map<string, Array<{ id: string; status: string; [key: string]: unknown }>>();
let listStatus = 200;
let listCalls = 0;
const listScopes: (string | null)[] = [];
let recruitStream: ReadableStreamDefaultController<Uint8Array> | null = null;
const posts: { path: string; body: any }[] = [];
let decideStatus: Record<string, number> = {};
let decideThrows: Record<string, boolean> = {};
let onDecide: ((id: string, verb: string) => void) | null = null;
let onScopedDecide: ((conversation: string, id: string, verb: string) => void) | null = null;
let hold: Promise<void> | null = null;
let listHold: Promise<void> | null = null;
/** The server records the decision, then the reply is lost. */
let lostReply: Record<string, boolean> = {};
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname === "/api/assist/actions") {
    listCalls++;
    await listHold;
    listScopes.push(url.searchParams.get("conversation"));
    const conversation = url.searchParams.get("conversation");
    return listStatus === 200 ? json({ actions: conversation === null ? actions : scopedActions.get(conversation) ?? [] }) : json({ error: { code: "x" } }, listStatus);
  }
  const m = /^\/api\/assist\/actions\/([^/]+)\/(approve|deny)$/.exec(url.pathname);
  if (m && init?.method === "POST") {
    posts.push({ path: url.pathname + url.search, body: JSON.parse(String(init.body ?? "{}")) });
    await hold;
    const record = () => {
      const conversation = url.searchParams.get("conversation");
      if (conversation === null) onDecide?.(m[1], m[2]);
      else onScopedDecide?.(conversation, m[1], m[2]);
    };
    if (lostReply[m[1]]) { record(); throw new TypeError("network"); }
    if (decideThrows[m[1]]) throw new TypeError("network");
    const status = decideStatus[m[1]] ?? 200;
    if (status === 200) record();
    return json(status === 200 ? { ok: true } : { error: { code: "refused" } }, status);
  }
  if (url.pathname === "/api/threads/master/conversations") return json({ conversation: { id: "recruit-conversation" } });
  if (url.pathname === "/api/threads/master/stream") {
    return new Response(new ReadableStream<Uint8Array>({ start(controller) { recruitStream = controller; } }), { headers: { "Content-Type": "text/event-stream" } });
  }
  if (url.pathname === "/api/threads/master") return json({ thread: null, entries: [] });
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
/** `detail` is the click count; a deciding click waits out the arming delay unless `quick`. */
async function click(element: Element | undefined | null, opts: { detail?: number; quick?: boolean } = {}) {
  assert(element, "click target exists");
  const label = element.textContent?.trim();
  if (!opts.quick && element.closest('[data-drawer="staged"]') && (label === "Approve" || label === "Deny")) await sleep(850);
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: opts.detail ?? 1 } as any)); });
  await flush();
}
const buttons = (scope: ParentNode) => Array.from(scope.querySelectorAll("button")) as HTMLButtonElement[];
const labelled = (scope: ParentNode, text: string) => buttons(scope).find((b) => b.textContent?.trim() === text);
const drawerEl = () => document.querySelector('.crm-drawer[data-drawer="staged"]') as HTMLElement | null;
const sent = (): number => posts.length;
// FSI/PDI isolates around a title inside a sentence are invisible; compare the words.
const text = (el: Element | null) => el?.textContent?.replace(/[\u2068\u2069]/g, "").replace(/\s+/g, " ").trim() ?? "";

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
          overview, readOnly: opts.readOnly ?? false, onOpenIssue: () => {}, permissionsHref: "/settings/permissions",
          onAsk: () => {}, onOpenDecision: () => {}, onAskAgent: () => {},
          decisions: opts.decisions ?? true,
        }),
      ),
    );
  });
  await settle(() => assert(listCalls > 0 || opts.decisions === false, "the list is read"));
  await flush();
  const cards = () => Array.from(host.querySelectorAll('[data-need="staged_write"]')) as HTMLElement[];
  const badge = () => host.querySelector("[data-badge]")!.textContent;
  const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); homeDecisions.resetDecisions(); };
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

// AOS-660: fixture mirrored byte-for-byte from AgenticOS packages/contracts/fixtures.
{
  const fixture = JSON.parse(require("fs").readFileSync(
    `${require("process").cwd()}/../tests/fixtures/agenticos/assistant-action-proposers.json`, "utf8",
  )) as {
    personal: { actions: Array<{ id: string; status: string; proposer?: { scope: "personal"; label: string }; [key: string]: unknown }> };
    workspace: { actions: Array<{ id: string; status: string; proposer: { scope: "workspace"; label: string }; [key: string]: unknown }> };
  };
  const personal = fixture.personal.actions[0]!.proposer!;
  const workspace = fixture.workspace.actions[0]!.proposer;
  const personalRows = parseStagedList(fixture.personal);
  const workspaceRows = parseStagedList(fixture.workspace);
  assert(personalRows?.length === 2 && workspaceRows?.length === 1, "complete producer lists parse");
  for (const [row, proposer] of [[personalRows[0]!, personal], [workspaceRows[0]!, workspace]] as const) {
    const attribution = `${proposer.label} · ${proposer.scope === "workspace" ? "Workspace assistant" : "Personal assistant"}`;
    assert(row.proposer?.scope === proposer.scope && askedBy(row) === attribution, "scoped proposer parses");
    assert(sourceLine(row).includes(attribution), "rail uses the same attribution");
  }
  assert(personalRows[1]!.proposer === null && askedBy(personalRows[1]!) === null, "legacy producer row has unknown proposer");
  const issue = parseStagedRow({ ...issueRow("issue"), proposer: workspace })!;
  assert(askedBy(issue) === `${workspace.label} · Workspace assistant`, "issue proposal carries scoped attribution");
  const connected = parseStagedRow({
    id: "connected", tool: "connected_effect", status: "pending", createdAt: 1, proposer: personal,
    input: { slug: "gmail_send", toolkit: "gmail", account: "a@b.test", connection_id: "c", effect: "send", args: "{}" },
  })!;
  assert(askedBy(connected) === `${personal.label} · Personal assistant`, "connected effect carries scoped attribution");
  for (const proposer of [undefined, null, {}, { scope: "other", label: "Wrong" }, { scope: "personal", label: "  " }, { scope: "workspace", label: 42 }]) {
    const row = parseStagedRow(customer("unknown", { origin: null, proposer }))!;
    assert(row.proposer === null && askedBy(row) === null && sourceLine(row) === "CRM", "invalid or absent proposer is unknown");
  }
  const cli = parseStagedRow(customer("cli", { proposer: personal }))!;
  assert(cli.proposer === null && askedBy(cli) === "Atlas (agent)", "CLI origin wins over conflicting assistant metadata");
  const mcp = parseStagedRow(customer("mcp", { origin: { via: "mcp", kind: "agent", label: "MCP actor" }, proposer: workspace }))!;
  assert(askedBy(mcp) === "MCP actor (agent)", "MCP origin wins over conflicting assistant metadata");
  const malformedOrigin = parseStagedRow(customer("unknown-origin", { origin: {}, proposer: personal }))!;
  assert(askedBy(malformedOrigin) === null, "malformed origin does not expose a potentially different proposer");
  const homeCommand = {
    id: "bafc1fc2e2520bcd5f76443f4b6702ea",
    tool: "cadence_cli", status: "pending", createdAt: 1795239000000,
    verb: "issue_new", title: "Cadence: issue new", requested_by: "Home assistant",
    origin: { via: "home", kind: "agent", label: "Home assistant" },
  };
  const scopedCommand = parseStagedRow({ ...homeCommand, proposer: personal });
  assert(scopedCommand && askedBy(scopedCommand) === `${personal.label} · Personal assistant`, "keyed Home command shows the verified personal proposer");
  const legacyCommand = parseStagedRow(homeCommand);
  assert(legacyCommand && askedBy(legacyCommand) === "Home assistant", "legacy Home command keeps its generic source");
  for (const via of ["cli", "mcp"]) {
    const external = parseStagedRow({
      ...homeCommand, requested_by: "External agent", proposer: personal,
      origin: { via, kind: "agent", label: "External agent" },
    });
    assert(external && external.proposer === null && askedBy(external) === "External agent", "external command attribution wins over conflicting proposer data");
  }

  actions = fixture.personal.actions;
  const v = await render();
  await settle(() => assert(v.cards().length === 2, "personal and legacy cards render"));
  const personalCard = v.cards().find((card) => text(card.querySelector(".todo-title")).includes("Call the supplier"));
  const legacyCard = v.cards().find((card) => text(card.querySelector(".todo-title")).includes("Check the outstanding request"));
  assert(personalCard && legacyCard, "both issue proposals render");
  assert(text(personalCard.querySelector(".todo-meta")).includes(`${personal.label} · Personal assistant`), "personal proposer and scope on Home");
  assert(!text(legacyCard).includes(personal.label) && !text(legacyCard).includes(workspace.label), "legacy card does not guess a proposer");
  await click(labelled(legacyCard, "Review"));
  await settle(() => assert(drawerEl() && !text(drawerEl()!.querySelector(".dx-asked")).includes(personal.label), "legacy detail does not infer a proposer"));
  await click(labelled(drawerEl()!, "Not now"));
  await settle(() => assert(text(drawerEl()).includes("Call the supplier"), "scoped detail opens next"));
  assert(text(drawerEl()!.querySelector(".dx-asked")).includes(`${personal.label} · Personal assistant`), "detail shows personal proposer and scope");
  await click(labelled(drawerEl()!, "Overview"));
  assert(text(drawerEl()).includes(personal.label) && text(drawerEl()).includes("Call the supplier"), "overview shows proposer and issue preview");
  await v.done();
}

// Recruit must use its server-proven conversation, never Home's unqualified queue.
{
  const fixture = JSON.parse(require("fs").readFileSync(
    `${require("process").cwd()}/../tests/fixtures/agenticos/assistant-action-proposers.json`, "utf8",
  )) as { personal: { actions: Array<{ id: string; proposer: { scope: "personal"; label: string } }> }; workspace: { actions: Array<{ id: string; proposer: { scope: "workspace"; label: string } }> } };
  const personal = fixture.personal.actions[0]!;
  const workspace = fixture.workspace.actions[0]!;
  const sharedLabel = personal.proposer.label;
  actions = [customer(personal.id, { origin: null, proposer: personal.proposer, title: "Home only" })];
  scopedActions.set("recruit-conversation", [customer(workspace.id, {
    origin: null, proposer: { ...workspace.proposer, label: sharedLabel }, title: "Recruit only",
  })]);
  listScopes.length = 0;
  const home = await render();
  await settle(() => assert(text(home.cards()[0]).includes("Home only"), "Home loads its own action"));
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const show = (showDecisions: boolean) => React.createElement(RecruitChat, { hireBlock: null, onBack: () => {}, showDecisions });
  await React.act(async () => { root.render(show(false)); });
  await settle(() => assert(host.querySelector("[data-chat-panel]"), "server-proven Recruit chat opens"));
  assert(!listScopes.includes("recruit-conversation"), "old native route does not request hosted decisions");
  await React.act(async () => { root.render(show(true)); });
  await settle(() => assert(text(host).includes("Recruit only"), "Recruit loads its scoped action"));
  assert(listScopes.includes("recruit-conversation") && !text(host).includes("Home only"), "Recruit uses only its conversation");
  assert(text(home.cards()[0]).includes(`${sharedLabel} · Personal assistant`), "Home identifies personal scope");
  assert(text(host.querySelector("[data-chat-staged]")).includes(`${sharedLabel} · Workspace assistant`), "Recruit identifies workspace scope despite identical names");
  await click(labelled(host.querySelector("[data-chat-staged]")!, "Review"));
  await settle(() => assert(text(drawerEl()?.querySelector(".dx-asked") ?? null).includes(`${sharedLabel} · Workspace assistant`), "Recruit review identifies workspace proposer"));
  await click(labelled(drawerEl()!, "Not now"));
  const pendingShare = {
    id: "ab0123456789abcdef0123456789abcd", tool: "artifact_share", status: "pending", createdAt: Date.now() - AGO,
    input: { id: "art-Brief", version: 2, title: "Hiring brief" },
    proposer: workspace.proposer,
  };
  scopedActions.set("recruit-conversation", [
    customer("live-recruit", { origin: null, proposer: workspace.proposer, title: "Live Recruit request" }),
    pendingShare,
    { ...pendingShare, id: "cd0123456789abcdef0123456789abcd", input: { ...pendingShare.input, title: "Malformed artifact", version: 0 } },
    { ...pendingShare, id: "bad-action-id", input: { ...pendingShare.input, title: "Unreviewable share" } },
  ]);
  await settle(() => assert(recruitStream, "Recruit stream opened"));
  const snapshot = {
    session: { id: "recruit-conversation" },
    actor_identity: { scope: "workspace", conversation_id: "recruit-conversation", identity: { name: sharedLabel, tone: null, look: "robot", revision: 1 } },
  };
  await React.act(async () => {
    recruitStream!.enqueue(new TextEncoder().encode(`event: master\ndata: ${JSON.stringify(snapshot)}\n\n`));
  });
  await settle(() => assert(text(host).includes("Live Recruit request"), "live Recruit snapshot refreshes its decisions"));
  assert(text(home.cards()[0]).includes("Home only"), "Recruit refresh leaves Home distinct");
  assert(!text(host).includes("Hiring brief") && !text(host).includes("Malformed artifact") && !text(host).includes("Unreviewable share"), "Recruit omits artifact shares, including otherwise well-formed entries");
  const live = Array.from(host.querySelectorAll("[data-chat-staged]")).find((card) => text(card).includes("Live Recruit request"));
  assert(live, "the supported action remains available after refresh");
  await click(labelled(live, "Review"));
  await settle(() => assert(text(drawerEl()).includes("Live Recruit request"), "the refreshed supported action opens for review"));
  await React.act(async () => { root.unmount(); });
  host.remove();
  await home.done();
  scopedActions.clear();
  recruitStream = null;
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
  await click(v.host.querySelector<HTMLButtonElement>(".needbtn")!);
  const rail = v.host.querySelector(".needsdrawer")!;
  await click(labelled(rail, "Updates"));
  const line = rail.querySelector("[data-fail]")!;
  assert(text(line).includes("Did not apply") && text(line).includes("the record changed after it was staged"), "the failed line says why");
  assert(!line.querySelector("pre"), "the code is under raw");
  await click(labelled(line, "Show raw"));
  assert(line.querySelector("pre")?.textContent?.includes("revision_conflict"), "code under raw");
  assert(!v.host.querySelector(".dx-dot"), "viewing Updates clears the dot");
  await click(buttons(rail).find((b) => b.textContent?.startsWith("To do")));
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

// ---- 10. new rows appear without a reload; a failed re-read keeps the list ----
{
  actions = [customer("a1")];
  const v = await render();
  await settle(() => assert(v.cards().length === 1 && v.badge() === "1", "one waiting"));
  actions = [...actions, customer("a2", { title: "Update Harbour Tea" })];
  await React.act(async () => { win.dispatchEvent(new win.Event("focus")); });
  await settle(() => assert(v.cards().length === 2 && v.badge() === "2", "a row added on the server appears on focus, count and badge too"));
  actions = [...actions, customer("a3", { title: "Update Third" })];
  await React.act(async () => { win.document.dispatchEvent(new win.Event("visibilitychange")); });
  await settle(() => assert(v.cards().length === 3 && v.badge() === "3", "and on visibility"));
  listStatus = 500;
  await React.act(async () => { win.dispatchEvent(new win.Event("focus")); });
  await flush(); await flush();
  assert(v.cards().length === 3 && v.badge() === "3" && !/Nothing needs you/.test(text(v.host)), "a failed re-read keeps the known list");
  listStatus = 200;
  await v.done();

  // A first read that fails is not "nothing".
  listStatus = 500;
  actions = [];
  const w = await render();
  await flush(); await flush();
  assert(!/Nothing needs you/.test(text(w.host)) && w.host.querySelector('[role="alert"]'), "an unread list is reported as unavailable, not empty");
  listStatus = 200;
  await w.done();
}

// ---- 11. a double-click cannot approve the next row; a refresh cannot retarget Approve ----
{
  actions = [customer("a1"), customer("a2", { title: "Update Harbour Tea" })];
  onDecide = (id) => {
    actions = actions.map((a: any) => a.id === id ? { ...a, status: "approved", decidedAt: Date.now(), result: { ok: true } } : a);
  };
  const v = await render();
  await settle(() => assert(v.cards().length === 2, "two cards"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  const approve = labelled(drawerEl()!, "Approve")!;
  await sleep(850);
  // The exact probe: the first click decides a1 and the list is re-read before the second lands.
  await click(approve, { quick: true, detail: 1 });
  await settle(() => assert(text(drawerEl()!.querySelector(".crm-drawer-title")) === "Update Harbour Tea", "advanced"));
  await click(labelled(drawerEl()!, "Approve"), { quick: true, detail: 2 });
  await click(labelled(drawerEl()!, "Approve"), { quick: true, detail: 1 });
  assert(sent() === 1 && posts[0].path.endsWith("/a1/approve"), `the second click did not approve a2 (${posts.map((p) => p.path).join(",")})`);
  onDecide = null;
  await v.done();

  // A refresh while open that puts a new row first cannot change what Approve acts on.
  actions = [customer("a1"), customer("a2", { title: "Update Harbour Tea" })];
  const w = await render();
  await settle(() => assert(w.cards().length === 2, "two cards"));
  await click(labelled(w.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  actions = [customer("a0", { title: "Newest", createdAt: Date.now() - 10 * AGO }), ...actions];
  await React.act(async () => { win.dispatchEvent(new win.Event("focus")); });
  await settle(() => assert(w.cards().length === 3, "the refresh arrived"));
  assert(text(drawerEl()!.querySelector(".crm-drawer-title")) === "Update Marlow Books", "the drawer still shows a1");
  await click(labelled(drawerEl()!, "Approve"));
  assert(sent() === 1 && posts[0].path.endsWith("/a1/approve"), "Approve acts on the displayed row");
  await w.done();
}

// ---- 12. approved without a result is not success ----
{
  const row = customer("a1", { status: "approved", decidedAt: Date.now(), result: null });
  assert(parseStagedRow(row) !== null, "it parses");
  actions = [row, customer("a2", { status: "approved", decidedAt: Date.now(), result: { ok: true } })];
  const v = await render();
  await settle(() => assert(listCalls > 0, "read"));
  await click(labelled(v.host, "Updates"));
  const lines = Array.from(v.host.querySelectorAll("[data-todo-decisions] li")).map((l) => text(l));
  assert(lines.length === 2 && lines.filter((l) => l.includes("Applied")).length === 1 && lines.some((l) => l.includes("Result not confirmed")), `only ok === true reads Applied (${lines.join("|")})`);
  await v.done();
  actions = [customer("a1")];
  onDecide = (id) => { actions = actions.map((a: any) => a.id === id ? { ...a, status: "approved", decidedAt: Date.now() } : a); };
  const w = await render();
  await settle(() => assert(w.cards().length === 1, "one"));
  await click(labelled(w.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  await click(labelled(drawerEl()!, "Approve"));
  await settle(() => assert(!drawerEl(), "closed"));
  assert(/could not be confirmed/.test(text(w.host)) && !/Applied/.test(text(w.host.querySelector(".todo-note"))), "the rail does not claim it applied");
  onDecide = null;
  await w.done();
}

// ---- 13. narrow: focus is not left on the button behind the scrim ----
{
  actions = [customer("a1")];
  const v = await render();
  await settle(() => assert(v.cards().length === 1, "one card"));
  const needBtn = v.host.querySelector(".needbtn") as HTMLElement;
  await click(needBtn);
  const slide = v.host.querySelector(".needsdrawer") as HTMLElement;
  assert(slide, "the slide-over opens");
  await click(labelled(slide.querySelector('[data-need="staged_write"]')!, "Review"));
  await settle(() => assert(drawerEl(), "the detail opens"));
  await tick();
  assert(document.activeElement !== needBtn, "focus is not put on the button behind the scrim");
  await click(labelled(drawerEl()!, "Not now"));
  await settle(() => assert(!drawerEl(), "closed"));
  await tick(); await tick();
  assert(document.activeElement === needBtn, "narrow: focus returns to the visible floating button");
  await v.done();
  win.innerWidth = 1400;
  actions = [customer("a1")];
  const w = await render();
  await settle(() => assert(w.cards().length === 1, "one"));
  await click(labelled(w.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  await click(labelled(drawerEl()!, "Not now"));
  await settle(() => assert(!drawerEl(), "closed"));
  await tick(); await tick();
  assert(document.activeElement?.closest(".needsrail") !== null && document.activeElement !== w.host.querySelector(".needbtn"), "wide: focus returns into the rail column");
  win.innerWidth = 1024;
  await w.done();
}

// ---- 14. members are never asked; the note names who asked; a removed kind degrades plainly ----
{
  const { ownerDecisionsOn } = require("../src/features/decisions/useDecisions") as typeof import("../src/features/decisions/useDecisions");
  const owner = { signed_in: true, hosted: true, session: { user: { role: "owner" } } };
  assert(ownerDecisionsOn(owner), "an owner on a hosted board is asked");
  assert(!ownerDecisionsOn({ ...owner, session: { user: { role: "member" } } }), "a member is not");
  assert(!ownerDecisionsOn({ ...owner, signed_in: false }) && !ownerDecisionsOn({ ...owner, hosted: false }) && !ownerDecisionsOn(null), "nor an unsigned or unhosted viewer");
  const m = await render({ decisions: ownerDecisionsOn({ ...owner, session: { user: { role: "member" } } }) });
  assert(listCalls === 0 && m.cards().length === 0 && m.badge() === "0", "a member makes no query; badge and rail agree");
  await m.done();

  actions = [customer("a1", { origin: { via: "cli", kind: "person", label: "Priya Nair" } })];
  const v = await render();
  await settle(() => assert(v.cards().length === 1, "one"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  assert(text(drawerEl()).includes("Priya Nair's note · not verified") && !text(drawerEl()).includes("The assistant's note"), "the note is labelled by who asked");
  await v.done();

  const odd = parseStagedRow(customer("z", { changes: [{ field: "notes", label: "Notes", kind: "removed", before: "x" }] }));
  assert(odd !== null && odd.changes[0].kind === "removed", "a removed kind is tolerated");
}

// ---- 15. a lost reply: unsure wins over "decided elsewhere"; a poll cannot fake a failure ----
{
  const decided = (result: unknown) => (id: string) => {
    actions = actions.map((a: any) => a.id === id ? { ...a, status: "approved", decidedAt: Date.now(), result } : a);
  };
  for (const [name, record, expect] of [
    ["applied", decided({ ok: true }), "Applied"],
    ["failed", decided({ ok: false, code: "revision_conflict" }), "did not apply: the record changed after it was staged"],
    ["pending", () => {}, "still waiting"],
  ] as const) {
    actions = [customer("a1")];
    onDecide = record;
    lostReply = { a1: true };
    const v = await render();
    await settle(() => assert(v.cards().length === 1, "one card"));
    await click(labelled(v.cards()[0], "Review"));
    await settle(() => assert(drawerEl(), "open"));
    await click(labelled(drawerEl()!, "Approve"));
    await settle(() => assert(labelled(drawerEl()!, "Check again"), `${name}: unsure after a lost reply`));
    lostReply = {};
    // A poll lands and shows the row decided: still unsure, not "decided elsewhere".
    await React.act(async () => { win.dispatchEvent(new win.Event("focus")); });
    await settle(() => assert(listCalls >= 2, "the poll read"));
    assert(labelled(drawerEl()!, "Check again") && !/decided elsewhere/.test(text(drawerEl())), `${name}: Check again stays; no false "elsewhere"`);
    const n = sent();
    await click(labelled(drawerEl()!, "Check again"));
    if (name === "pending") {
      await settle(() => assert(labelled(drawerEl()!, "Approve") && text(drawerEl()).includes(expect), "pending: decide again is allowed"));
    } else {
      await settle(() => assert(!drawerEl(), `${name}: the queue ends`));
      assert(text(v.host.querySelector(".todo-note")).includes(expect), `${name}: the row's actual outcome is reported (got "${text(v.host.querySelector(".todo-note"))}")`);
    }
    assert(sent() === n, "Check again never resubmits");
    onDecide = null;
    await v.done();
  }

  // A poll that starts while the decision settles is skipped: no false "could not confirm".
  actions = [customer("a1")];
  onDecide = decided({ ok: true });
  const w = await render();
  await settle(() => assert(w.cards().length === 1, "one card"));
  await click(labelled(w.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  let release!: () => void;
  listHold = new Promise<void>((r) => { release = r; });
  await click(labelled(drawerEl()!, "Approve"));
  await flush();
  const reads = listCalls;
  await React.act(async () => { win.dispatchEvent(new win.Event("focus")); });
  await flush();
  assert(listCalls === reads, "no background read while a decision settles");
  release(); listHold = null;
  await settle(() => assert(!drawerEl(), "settled and closed"));
  assert(/Applied/.test(text(w.host.querySelector(".todo-note"))) && !/could not be confirmed/.test(text(w.host)), "reported as applied, not unconfirmed");
  onDecide = null;

  // A hidden tab is not polled.
  Object.defineProperty(win.document, "visibilityState", { value: "hidden", configurable: true });
  const before = listCalls;
  await React.act(async () => { win.dispatchEvent(new win.Event("focus")); });
  await flush();
  assert(listCalls === before, "hidden: no read");
  Object.defineProperty(win.document, "visibilityState", { value: "visible", configurable: true });
  await w.done();
}

// ---- 16. API text is isolated; creates fold their defaults; Empty vs (cleared); effects apart; arming ----
{
  const RLO = "\u202E";
  const evil = (w: string) => `${RLO}${w}`;
  actions = [customer("a1", {
    title: evil("title"), reason: evil("reason"), origin: { via: "cli", kind: "person", label: evil("origin") },
    consent: { channel: evil("consent") },
    changes: [{ field: "stage", label: evil("label"), kind: "changed", before: evil("before"), after: evil("after") }],
    effects: [evil("effect one"), "Nothing is sent to anyone"],
  })];
  const v = await render();
  await settle(() => assert(v.cards().length === 1, "one card"));
  const card = v.cards()[0];
  const bare = (root: Element) => Array.from(root.querySelectorAll("*")).filter((e) => Array.from(e.childNodes).some((n) => n.nodeType === 3 && (n.textContent ?? "").includes(RLO)) && !e.closest("bdi"));
  assert(bare(card).length === 0, `the card isolates API text (${bare(card).map((e) => e.className).join(",")})`);
  await click(labelled(card, "Review"));
  await settle(() => assert(drawerEl(), "open"));
  const unwrapped = bare(drawerEl()!);
  assert(unwrapped.length === 0, `every API string in the drawer is isolated (${unwrapped.map((e) => e.tagName + "." + e.className).join(",")})`);
  for (const w of ["title", "reason", "origin", "consent", "label", "before", "after", "effect one"]) assert(drawerEl()!.textContent!.includes(evil(w)), `${w} is shown`);
  assert(text(drawerEl()!.querySelector(".dx-eff")).includes("effect one · Nothing is sent to anyone"), "effects are separated");
  await v.done();

  const createRow = (id: string, args: string) => customer(id, {
    title: "Add E2E Synthetic Customer", target: null, unchanged_count: 0, reason: null, consent: null, effects: ["Runs as you"],
    args,
    changes: [
      { field: "display_name", label: "Display name", kind: "added", after: "E2E Synthetic Customer" },
      { field: "email", label: "Email", kind: "added", after: "e2e@example.invalid" },
      ...["phone", "notes", "tags", "stage", "owner", "source"].map((f) => ({ field: f, label: f, kind: "added", after: f === "notes" ? null : "x" })),
    ],
  });
  actions = [createRow("c1", JSON.stringify({ profile: { display_name: "E2E Synthetic Customer", email: "e2e@example.invalid" }, record_id: "rec-1" }))];
  const w = await render();
  await settle(() => assert(w.cards().length === 1, "one card"));
  await click(labelled(w.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  const rowsOf = () => Array.from(drawerEl()!.querySelectorAll(".dx-row"));
  assert(rowsOf().length === 2, `a create lists what was sent (${rowsOf().length})`);
  const more = labelled(drawerEl()!, "6 defaults also set");
  assert(more && more.getAttribute("aria-expanded") === "false", "defaults are one closed disclosure");
  const dlg = drawerEl();
  await click(more);
  assert(rowsOf().length === 8 && drawerEl() === dlg, "expanding shows all of them in place");
  const notes = rowsOf().map((r) => text(r)).find((r) => r.startsWith("notes"))!;
  assert(notes.includes("Empty") && !notes.includes("(cleared)"), `an added null reads Empty (${notes})`);
  await w.done();

  actions = [createRow("c2", "not json")];
  const x = await render();
  await settle(() => assert(x.cards().length === 1, "one card"));
  await click(labelled(x.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  assert(drawerEl()!.querySelectorAll(".dx-row").length === 8 && !labelled(drawerEl()!, "6 defaults also set"), "unparseable args show the flat list");
  await x.done();

  // A just-shown item is not armed yet.
  actions = [customer("a1"), customer("a2", { title: "Update Harbour Tea" })];
  const y = await render();
  await settle(() => assert(y.cards().length === 2, "two"));
  await click(labelled(y.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  await sleep(300);
  await click(labelled(drawerEl()!, "Approve"), { quick: true });
  assert(sent() === 0, "inside the arming window a click does nothing");
  await y.done();
}

// ---- 17. a blank field name is data; a blind update's null is cleared; an embedded PDI cannot close an isolate ----
{
  const row = parseStagedRow(customer("b", { changes: [{ field: "", label: "", kind: "changed", before: "a", after: "b" }, { field: "  ", label: "  ", kind: "added", after: "c" }] }));
  assert(row !== null && row.changes.length === 2, "blank names do not drop the row");
  actions = [customer("a1", {
    changes: [{ field: "", label: "", kind: "changed", before: "old", after: "new" }, { field: "phone", label: "Phone", kind: "added", after: null }],
    changes_complete: false,
  })];
  const v = await render();
  await settle(() => assert(v.cards().length === 1 && v.badge() === "1", "the row still shows, counts, and Nothing needs you is untrue"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  const rows = Array.from(drawerEl()!.querySelectorAll(".dx-row")).map((r) => `${text(r.querySelector("dt"))}: ${text(r.querySelector("dd"))}`);
  assert(rows.some((r) => r.startsWith("(unnamed field)") && r.includes("new")), `a blank name renders as data (${rows.join("|")})`);
  assert(rows.some((r) => r.startsWith("Phone") && r.includes("(cleared)") && !r.includes("Empty")), `a blind update's null reads (cleared) (${rows.join("|")})`);
  await v.done();

  const PDI = "\u2069";
  const bad = (w: string) => `${w}${PDI}x`;
  actions = [customer("a1", {
    title: bad("title"), reason: bad("reason"), origin: { via: "cli", kind: "person", label: bad("origin") },
    consent: { channel: bad("consent") },
    changes: [{ field: "stage", label: bad("label"), kind: "changed", before: bad("before"), after: bad("after") }],
    effects: [bad("effect")],
  })];
  const w = await render();
  await settle(() => assert(w.cards().length === 1, "one card"));
  const inBdi = (root: Element) => Array.from(root.querySelectorAll("bdi")).map((b) => b.textContent ?? "").join("");
  assert(!/[\u2066-\u2069]/.test(inBdi(w.cards()[0])), "the card strips isolate controls from API text");
  await click(labelled(w.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  assert(!/[\u2066-\u2069]/.test(inBdi(drawerEl()!)), "so does the drawer");
  assert(drawerEl()!.textContent!.includes("titlex") && drawerEl()!.textContent!.includes("afterx"), "the words are still shown");
  const { noticeText } = require("../src/features/decisions/StagedWriteDetail") as typeof import("../src/features/decisions/StagedWriteDetail");
  const sentence = noticeText({ outcome: "applied", title: bad("t") }, (e: string) => e);
  assert(sentence.split(PDI).length === 2 && sentence.endsWith(`${PDI}`), "in a sentence only our own terminator remains");
  await w.done();
}

// A staged action stays with its conversation, including across a late read
// and when another conversation happens to use the same action id.
{
  actions = [customer("home", { title: "Home request" })];
  scopedActions.set("conversation-a", [customer("same", { title: "Draft A" })]);
  scopedActions.set("conversation-b", [customer("same", { title: "Draft B" })]);
  onScopedDecide = (conversation, id, verb) => {
    scopedActions.set(conversation, (scopedActions.get(conversation) ?? []).map((row) =>
      row.id === id ? { ...row, status: verb === "approve" ? "approved" : "denied", decidedAt: Date.now(), result: verb === "approve" ? { ok: true } : null } : row,
    ));
  };
  posts.length = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const pending = (_conversation: string, _waiting: boolean) => {};
  const show = (conversation: string) => React.createElement(React.Fragment, null,
    React.createElement(Badge, { needs: [], owner: true }),
    React.createElement(ConversationDecisions, { key: conversation, conversation, revision: 0, onPendingChange: pending, onReview: () => {} }),
  );
  await React.act(async () => { root.render(show("conversation-a")); });
  await settle(() => assert(text(host).includes("Draft A") && text(host.querySelector("[data-badge]")) === "1", "the conversation and Home show their own pending actions"));
  await click(labelled(host.querySelector("[data-chat-staged]")!, "Review"));
  await settle(() => assert(drawerEl() && text(drawerEl()).includes("Draft A"), "review opens the selected action"));

  let releaseRead!: () => void;
  listHold = new Promise<void>((resolve) => { releaseRead = resolve; });
  const reads = listCalls;
  await React.act(async () => { win.dispatchEvent(new win.Event("focus")); });
  await settle(() => assert(listCalls > reads, "a refresh is in flight"));
  await React.act(async () => { root.render(show("conversation-b")); });
  assert(!drawerEl(), "switching conversation closes the old review");
  releaseRead(); listHold = null;
  await settle(() => assert(text(host).includes("Draft B"), "the new conversation loads"));
  assert(!text(host).includes("Draft A") && text(host.querySelector("[data-badge]")) === "1", "the late read cannot replace the new conversation or Home");

  await click(labelled(host.querySelector("[data-chat-staged]")!, "Review"));
  await settle(() => assert(drawerEl() && text(drawerEl()).includes("Draft B"), "the new review shows the correct subject"));
  const clock = Date.now;
  Date.now = () => clock() + 1_000;
  try { await click(labelled(drawerEl()!, "Approve"), { quick: true }); }
  finally { Date.now = clock; }
  await settle(() => assert(!drawerEl() && text(host).includes("Applied"), "the recorded outcome is shown in the conversation"));
  assert(posts.length === 1 && posts.map((post) => post.path).join("") === "/api/assist/actions/same/approve?conversation=conversation-b", "the decision carries the selected conversation");
  assert(scopedActions.get("conversation-a")?.[0]?.status === "pending", "the other conversation remains pending");
  assert(typeof actions[0] === "object" && actions[0] !== null && "status" in actions[0] && actions[0].status === "pending", "the Home action remains pending");
  await React.act(async () => { root.unmount(); });
  host.remove();
  homeDecisions.resetDecisions();
  scopedActions.clear();
  onScopedDecide = null;
}

// An unresolved request survives switching away and back to its conversation.
{
  scopedActions.set("waiting-a", [customer("flight", { title: "Pending flight" })]);
  scopedActions.set("waiting-b", [customer("other", { title: "Other conversation" })]);
  onScopedDecide = (conversation, id) => {
    scopedActions.set(conversation, (scopedActions.get(conversation) ?? []).map((row) =>
      row.id === id ? { ...row, status: "approved", decidedAt: Date.now(), result: { ok: true } } : row,
    ));
  };
  posts.length = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const show = (conversation: string) => React.createElement(ConversationDecisions, {
    key: conversation, conversation, revision: 0, onPendingChange: () => {}, onReview: () => {},
  });
  const select = async (conversation: string) => {
    await React.act(async () => { root.render(show(conversation)); });
    await settle(() => assert(text(host).includes(conversation === "waiting-a" ? "Pending flight" : "Other conversation"), "selected conversation loaded"));
  };
  await select("waiting-a");
  await click(labelled(host.querySelector("[data-chat-staged]")!, "Review"));
  await settle(() => assert(drawerEl(), "flight opens"));
  let finish!: () => void;
  hold = new Promise<void>((resolve) => { finish = resolve; });
  const clock = Date.now;
  Date.now = () => clock() + 1_000;
  try { await click(labelled(drawerEl()!, "Approve"), { quick: true }); }
  finally { Date.now = clock; }
  assert(posts.length === 1, "one approval is in flight");
  await select("waiting-b");
  await select("waiting-a");
  await click(labelled(host.querySelector("[data-chat-staged]")!, "Review"));
  await settle(() => assert(drawerEl(), "review reopens while the request is in flight"));
  assert(labelled(drawerEl()!, "Deny")?.getAttribute("aria-disabled") === "true", "the in-flight approval prevents a conflicting denial after switching back");
  Date.now = () => clock() + 1_000;
  try { await click(labelled(drawerEl()!, "Deny"), { quick: true }); }
  finally { Date.now = clock; }
  assert(posts.length === 1, "a conflicting click does not submit");
  finish(); hold = null;
  await settle(() => assert(scopedActions.get("waiting-a")?.[0]?.status === "approved", "the original approval settled"));
  await settle(() => assert(text(drawerEl()).includes("Applied"), "the reopened review shows the result"));
  assert(!text(drawerEl()).includes("decided elsewhere"), "our own completed decision is not attributed elsewhere");
  assert(posts.length === 1, "switching conversations did not submit another decision");
  await React.act(async () => { root.unmount(); });
  host.remove();
  scopedActions.clear();
  onScopedDecide = null;
}

// A lost reply while away remains uncertain until the owner checks, not resubmits.
{
  scopedActions.set("uncertain-a", [customer("uncertain", { title: "Uncertain flight" })]);
  scopedActions.set("uncertain-b", [customer("other", { title: "Other conversation" })]);
  decideThrows.uncertain = true;
  posts.length = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const show = (conversation: string) => React.createElement(ConversationDecisions, {
    key: conversation, conversation, revision: 0, onPendingChange: () => {}, onReview: () => {},
  });
  await React.act(async () => { root.render(show("uncertain-a")); });
  await settle(() => assert(text(host).includes("Uncertain flight"), "uncertain action loaded"));
  await click(labelled(host.querySelector("[data-chat-staged]")!, "Review"));
  let finish!: () => void;
  hold = new Promise<void>((resolve) => { finish = resolve; });
  const clock = Date.now;
  Date.now = () => clock() + 1_000;
  try { await click(labelled(drawerEl()!, "Approve"), { quick: true }); }
  finally { Date.now = clock; }
  await React.act(async () => { root.render(show("uncertain-b")); });
  finish(); hold = null;
  await flush();
  await React.act(async () => { root.render(show("uncertain-a")); });
  await settle(() => assert(text(host).includes("Uncertain flight"), "uncertain action reloaded"));
  await click(labelled(host.querySelector("[data-chat-staged]")!, "Review"));
  await settle(() => assert(labelled(drawerEl()!, "Check again"), "the lost reply still requires checking"));
  assert(!labelled(drawerEl()!, "Approve") && posts.length === 1, "the owner cannot resubmit an uncertain decision");
  decideThrows = {};
  await click(labelled(drawerEl()!, "Check again"));
  await settle(() => assert(labelled(drawerEl()!, "Approve"), "a check showing it still pending permits a new decision"));
  assert(posts.length === 1, "checking never resubmits");
  await React.act(async () => { root.unmount(); });
  host.remove();
  scopedActions.clear();
}

// A private-session change clears retained attempts even while their view is absent.
{
  const { resetPrivateSession, allowPrivateReads } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  scopedActions.set("session-a", [customer("private", { title: "Previous session request" })]);
  scopedActions.set("session-b", [customer("other", { title: "Other conversation" })]);
  decideThrows.private = true;
  posts.length = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const show = (conversation: string) => React.createElement(ConversationDecisions, {
    key: conversation, conversation, revision: 0, onPendingChange: () => {}, onReview: () => {},
  });
  await React.act(async () => { root.render(show("session-a")); });
  await settle(() => assert(text(host).includes("Previous session request"), "previous session action loaded"));
  await click(labelled(host.querySelector("[data-chat-staged]")!, "Review"));
  let finish!: () => void;
  hold = new Promise<void>((resolve) => { finish = resolve; });
  const clock = Date.now;
  Date.now = () => clock() + 1_000;
  try { await click(labelled(drawerEl()!, "Approve"), { quick: true }); }
  finally { Date.now = clock; }
  await React.act(async () => { root.render(show("session-b")); });
  await React.act(async () => { resetPrivateSession(); allowPrivateReads(); });
  scopedActions.set("session-a", [customer("private", { title: "New session request" })]);
  finish(); hold = null;
  await flush();
  await React.act(async () => { root.render(show("session-a")); });
  await settle(() => assert(text(host).includes("New session request"), "new session receives its own action"));
  assert(!text(host).includes("Previous session request"), "no private row from the previous session is retained");
  await click(labelled(host.querySelector("[data-chat-staged]")!, "Review"));
  assert(labelled(drawerEl()!, "Approve")?.getAttribute("aria-disabled") !== "true", "the previous session's attempt does not block the new one");
  assert(!labelled(drawerEl()!, "Check again"), "the previous session's uncertainty is not shown");
  decideThrows = {};
  await React.act(async () => { root.unmount(); });
  host.remove();
  scopedActions.clear();
}

// ---- 18. CAD-2303: 4-tab drawer, cross-domain chip, optional deny note, and multi-tab bidi isolation ----
{
  const RLO = "\u202E";
  const PDI = "\u2069";
  const crossRows = {
    marketing: customer("m1", { title: "Send Q4 campaign newsletter", app: "Mailchimp", target: { kind: "campaign", label: "Q4 Launch", id: "cmp_1" } }),
    design: customer("d1", { title: "Publish Figma brand token set", app: "Figma", target: { kind: "brand", label: "Core Palette", id: "des_1" } }),
    business: customer("b1", { title: "Update Marlow Books", app: "CRM" }),
    operations: customer("o1", { title: "Approve vendor invoice schedule", app: "Ops", target: { kind: "vendor", label: "Acme Freight", id: "vnd_1" } }),
    engineering: {
      id: "e1", tool: "board_create_issue", status: "pending", createdAt: Date.now() - AGO, decidedAt: null,
      input: { title: "Deploy GitHub repo worker release", project: "ENG", priority: "high", body: "Ready for release." },
    },
  };
  assert(stagedDomain(parseStagedRow(crossRows.marketing)!) === "marketing", "marketing domain");
  assert(stagedDomain(parseStagedRow(crossRows.design)!) === "design", "design domain");
  assert(stagedDomain(parseStagedRow(crossRows.business)!) === "business", "business domain");
  assert(stagedDomain(parseStagedRow(crossRows.operations)!) === "operations", "operations domain");
  assert(stagedDomain(parseStagedRow(crossRows.engineering)!) === "engineering", "engineering domain");

  const parsedWithReason = parseStagedRow(customer("r1", { status: "denied", decidedAt: Date.now(), decision_reason: "Budget freeze until Q2" }));
  assert(parsedWithReason?.decisionReason === "Budget freeze until Q2", "decision_reason is parsed onto StagedWrite");

  const evil = (s: string) => `${RLO}${s}${PDI}x`;
  actions = [customer("a1", {
    title: evil("Launch promo campaign"),
    app: "Mailchimp",
    target: { kind: "campaign", label: evil("VIP List"), id: evil("cmp_77") },
    origin: { via: "cli", kind: "agent", label: evil("Atlas") },
    reason: evil("Ready for send"),
    consent: { channel: evil("email") },
    effects: [evil("Sends once")],
  })];
  onDecide = (id, verb) => {
    actions = actions.map((a: any) =>
      a.id === id ? { ...a, status: verb === "approve" ? "approved" : "denied", decidedAt: Date.now(), decision_reason: "Hold until pricing sign-off" } : a,
    );
  };
  const v = await render();
  await settle(() => assert(v.cards().length === 1, "one card"));
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "drawer opens"));
  const d = drawerEl()!;
  assert(text(d.querySelector(".dx-domain-chip")) === "Marketing", "domain chip renders Marketing");
  const tabLabels = Array.from(d.querySelectorAll('[role="tab"]')).map((el) => text(el));
  assert(tabLabels.join("|") === "Approvals|Overview|Evidence|Audit", `4 tabs render (${tabLabels.join("|")})`);

  const bare = (root: Element) => Array.from(root.querySelectorAll("*")).filter((e) => Array.from(e.childNodes).some((n) => n.nodeType === 3 && (n.textContent ?? "").includes(RLO)) && !e.closest("bdi"));
  const inBdi = (root: Element) => Array.from(root.querySelectorAll("bdi")).map((b) => b.textContent ?? "").join("");

  // Check all 4 tabs before any Show raw is opened: "revision" must not appear anywhere.
  assert(!/revision/i.test(text(d)), "Approvals hides 'revision' outside Show raw");
  // Approvals Show raw must also isolate RLO text inside bdi.
  await click(labelled(d, "Show raw"));
  assert(d.querySelector("pre")?.textContent?.includes('"expected_revision": 7'), "Approvals Show raw renders payload");
  assert(bare(d).length === 0, "Approvals Show raw isolates RLO text inside bdi");
  assert(!/[\u2066-\u2069]/.test(inBdi(d)), "Approvals Show raw strips PDI inside bdi");
  await click(labelled(d, "Hide raw"));

  // Switch tabs within the same drawer node and check content, no-'revision' guard, and bidi isolation on every tab.
  for (const name of ["Overview", "Evidence", "Audit"] as const) {
    await click(labelled(d, name));
    assert(drawerEl() === d, `${name} tab reuses the same drawer node`);
    assert(bare(d).length === 0, `${name} isolates RLO text inside bdi`);
    assert(!/[\u2066-\u2069]/.test(inBdi(d)), `${name} strips PDI inside bdi`);
    assert(!/revision/i.test(text(d)), `${name} hides 'revision' outside Show raw`);
    if (name === "Overview") assert(text(d).includes("What this request does") && text(d).includes("Marketing"), "Overview shows domain facts");
    if (name === "Evidence") {
      assert(d.querySelector("pre") === null, "Evidence hides raw payload behind Show raw by default");
      await click(labelled(d, "Show raw"));
      assert(d.querySelector("pre")?.textContent?.includes('"expected_revision": 7'), "Evidence Show raw renders payload");
      assert(bare(d).length === 0, "Evidence Show raw isolates RLO text inside bdi");
      assert(!/[\u2066-\u2069]/.test(inBdi(d)), "Evidence Show raw strips PDI inside bdi");
      await click(labelled(d, "Hide raw"));
      assert(d.querySelector("pre") === null, "Evidence Hide raw collapses payload");
    }
    if (name === "Audit") assert(text(d).includes("Workspace owner only") && text(d).includes("a1"), "Audit shows gate and id");
  }

  await click(labelled(d, "Approvals"));
  await click(labelled(d, "Add deny note (optional)"));
  const draftBox = d.querySelector(".dx-deny-box textarea") as HTMLTextAreaElement | null;
  assert(draftBox !== null, "deny note textarea expands");
  const setTextarea = async (el: HTMLTextAreaElement, val: string) => {
    await React.act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
      setter?.call(el, val);
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    });
  };
  await setTextarea(draftBox, "Draft note");
  await click(labelled(d, "Overview"));
  await click(labelled(d, "Approvals"));
  const keptBox = d.querySelector(".dx-deny-box textarea") as HTMLTextAreaElement | null;
  assert(keptBox?.value === "Draft note", "deny note disclosure & text survive tab switch");
  await click(labelled(d, "Hide deny note"));
  assert(d.querySelector(".dx-deny-box") === null, "Hide deny note collapses textarea");
  await click(labelled(d, "Add deny note (optional)"));
  const box = d.querySelector(".dx-deny-box textarea") as HTMLTextAreaElement | null;
  assert(box !== null && box.value === "", "Hide deny note cleared draft text before re-opening");
  await setTextarea(box, "  Hold until pricing sign-off  ");
  await click(labelled(d, "Deny"));
  await settle(() => assert(!drawerEl(), "drawer closes after deny"));
  const denyPost = posts.find((p) => p.path === "/api/assist/actions/a1/deny");
  assert(denyPost && posts.length === 1 && denyPost.body.reason === "Hold until pricing sign-off", `deny sends optional trimmed reason (${JSON.stringify(denyPost?.body)})`);
  onDecide = null;
  await v.done();

  // Audit tab also hides raw failure code ("revision_conflict") behind Show raw.
  actions = [customer("a1")];
  const f = await render();
  await settle(() => assert(f.cards().length === 1, "one card for Audit receipt check"));
  await click(labelled(f.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "drawer opens for Audit receipt check"));
  decideStatus.a1 = 409;
  actions = [customer("a1", { status: "approved", decidedAt: Date.now(), result: { ok: false, code: "revision_conflict" } })];
  await click(labelled(drawerEl()!, "Approve"));
  await settle(() => assert(text(drawerEl()!).includes("already decided elsewhere"), "decided elsewhere keeps drawer open"));
  await click(labelled(drawerEl()!, "Audit"));
  assert(text(drawerEl()!).includes("Execution receipt") && text(drawerEl()!).includes("Did not apply: the record changed after it was staged"), "Audit renders plain failure reason");
  assert(!/revision/i.test(text(drawerEl()!)), "Audit hides raw failure code containing 'revision' outside Show raw");
  await click(labelled(drawerEl()!, "Show raw"));
  assert(drawerEl()!.querySelector("pre")?.textContent?.includes("revision_conflict"), "Audit Show raw discloses failure code");
  decideStatus.a1 = 200;
  await f.done();
}

// ---- 18. CAD-2406: a connected draft or send is decidable; args are plain text; every outcome is honest ----
{
  const ARGS = { to: "<b>Ana</b>@example.com", subject: "Quote", body: "Line one\n<img src=x onerror=alert(1)>", cc: null, n: 3, opts: { a: [1] } };
  const effect = (id: string, extra: Record<string, unknown> = {}, input: Record<string, unknown> = {}) => ({
    id, tool: "connected_effect", status: "pending", createdAt: Date.now() - AGO, decidedAt: null,
    origin: { via: "mcp", kind: "agent", label: "Atlas" },
    input: { slug: "gmail_send_email", toolkit: "gmail", effect: "send", connection_id: "ca_1", account: "ops@example.com", args: JSON.stringify(ARGS), ...input },
    ...extra,
  });
  const good = parseStagedRow(effect("e1"));
  assert(good !== null && good.kind === "connected_effect" && good.connected?.effect === "send", "a good row parses");
  assert(good.changes.length === 6 && good.changes.find((c) => c.field === "opts")?.after === '{"a":[1]}', "every argument is kept, nested ones as JSON text");
  for (const [why, row] of [
    ["no input", { ...effect("e"), input: undefined }],
    ["args not JSON", effect("e", {}, { args: "{nope" })],
    ["args not an object", effect("e", {}, { args: "[1]" })],
    ["args not text", effect("e", {}, { args: { to: "x" } })],
    ["unknown effect", effect("e", {}, { effect: "delete" })],
    ["no account", effect("e", {}, { account: " " })],
    ["no slug", effect("e", {}, { slug: undefined })],
    ["no connection", effect("e", {}, { connection_id: undefined })],
  ] as const) assert(parseStagedRow(row) === null, `${why}: the row is dropped`);

  actions = [effect("e1"), effect("e2", {}, { effect: "draft", slug: "gmail_create_email_draft" }), effect("bad", {}, { args: "{" })];
  onDecide = (id, verb) => {
    actions = actions.map((a: any) => a.id !== id ? a : {
      ...a, status: verb === "approve" ? "approved" : "denied", decidedAt: Date.now(),
      result: verb === "approve" ? (id === "e1" ? { ok: true } : { ok: false, code: "uncertain" }) : null,
    });
  };
  const v = await render();
  await settle(() => assert(v.cards().length === 2, "two cards; the unparseable row is not one"));
  assert(text(v.cards()[0].querySelector(".todo-title")) === "Send · gmail_send_email", `verb and tool (got ${text(v.cards()[0].querySelector(".todo-title"))})`);
  assert(text(v.cards()[1].querySelector(".todo-title")) === "Draft · gmail_create_email_draft", "the draft verb");
  assert(text(v.cards()[0].querySelector(".todo-meta")).startsWith("ops@example.com (gmail) · Atlas (agent)"), "account, toolkit, asker");
  await click(labelled(v.cards()[0], "Review"));
  await settle(() => assert(drawerEl(), "open"));
  const d = drawerEl()!;
  const rows = Array.from(d.querySelectorAll(".dx-row")).map((r) => `${text(r.querySelector("dt"))}: ${text(r.querySelector("dd"))}`);
  assert(rows.includes("to: <b>Ana</b>@example.com") && rows.some((r) => r.startsWith("body: Line one") && r.includes("<img src=x onerror=alert(1)>")), `values are text (${rows.join("|")})`);
  assert(!d.querySelector("b, img"), "no argument became markup");
  assert(rows.includes("cc: Empty") && rows.includes("n: 3"), "null and numbers read as data");
  assert(text(d.querySelector(".crm-drawer-sub")).includes("ops@example.com (gmail)") && /Asked by Atlas \(agent\), via mcp/.test(text(d)), "account, toolkit and asker in the detail");
  assert(text(d).includes("Nothing has been sent yet"), "pending says nothing was sent");
  await click(labelled(d, "Approve"));
  await settle(() => assert(text(drawerEl()!.querySelector(".crm-drawer-title")) === "Draft · gmail_create_email_draft", "advanced"));
  assert(text(drawerEl()!).includes("Sent · gmail_send_email"), "ok:true reads Sent");
  await click(labelled(drawerEl()!, "Approve"));
  await settle(() => assert(!drawerEl(), "closed"));
  assert(/unconfirmed/.test(text(v.host)) && /may have acted/.test(text(v.host)) && !/Drafted/.test(text(v.host)), `uncertain is not success (${text(v.host)})`);
  await click(labelled(v.host, "Updates"));
  const lines = Array.from(v.host.querySelectorAll("[data-todo-decisions] li")).map((l) => text(l));
  assert(lines.some((l) => l.includes("Sent") && l.includes("gmail_send_email")) && lines.some((l) => l.includes("Result not confirmed") && l.includes("do not assume it did not")), `Updates (${lines.join("|")})`);
  onDecide = null;
  await v.done();

  // The server leaves an uncertain send pending, carrying the result.
  const stuck = parseStagedRow(effect("u1", { result: { ok: false, code: "uncertain", retry: true } }));
  assert(stuck !== null && outcomeOf(stuck) === "pending" && stuck.result?.code === "uncertain", "a pending row keeps its uncertain result");
  const denied = parseStagedRow(effect("u2", { status: "denied", decidedAt: Date.now(), result: { ok: false, code: "uncertain", retry: true } }));
  assert(denied !== null && outcomeOf(denied) === "unconfirmed", "denied after an uncertain attempt is not 'nothing ran'");
  const lost = parseStagedRow(effect("u6", { status: "denied", decidedAt: Date.now(), result: { ok: false, code: "capability_unavailable", retry: true } }));
  assert(lost !== null && outcomeOf(lost) === "unconfirmed", "denied after any unsettled (retry) attempt is not 'nothing ran'");
  actions = [effect("u1", { result: { ok: false, code: "uncertain", retry: true } })];
  {
    const w = await render();
    await settle(() => assert(w.cards().length === 1, "still a card"));
    await click(labelled(w.cards()[0], "Review"));
    await settle(() => assert(drawerEl(), "open"));
    const body = text(drawerEl());
    assert(!body.includes("Nothing has been sent yet") && body.includes("may have acted"), `a pending uncertain row never says nothing was sent (${body.slice(0, 300)})`);
    await click(labelled(drawerEl()!, "Audit"));
    assert(text(drawerEl()!).includes("Result not confirmed: the provider may have acted") && !text(drawerEl()!).includes("Did not apply"), `the Audit receipt of a pending uncertain row (${text(drawerEl()!).slice(0, 400)})`);
    await w.done();
  }
  {
    actions = [effect("u3", { status: "denied", decidedAt: Date.now(), result: { ok: false, code: "uncertain", retry: true } }), effect("u4")];
    const w = await render();
    await settle(() => assert(w.cards().length === 1, "the denied row is not waiting"));
    await w.done();
  }
  // The real approve: the reply is fine, the row stays pending carrying the uncertain result.
  actions = [effect("u5")];
  onDecide = (id) => { actions = actions.map((a: any) => a.id === id ? { ...a, result: { ok: false, code: "uncertain", retry: true } } : a); };
  {
    const w = await render();
    await settle(() => assert(w.cards().length === 1, "one card"));
    await click(labelled(w.cards()[0], "Review"));
    await settle(() => assert(drawerEl(), "open"));
    await click(labelled(drawerEl()!, "Approve"));
    await settle(() => assert(labelled(drawerEl()!, "Check again"), "unsure after a pending reply"));
    assert(sent() === 1, "one submit");
    await click(labelled(drawerEl()!, "Check again"));
    const n = text(drawerEl()!);
    assert(sent() === 1 && n.includes("did not finish") && n.includes("may have acted") && !n.includes("Nothing was recorded"), `Check again does not claim nothing was recorded (${n.slice(0, 400)})`);
    await w.done();
  }
  onDecide = null;

  for (const [code, words] of [["expired", "expired"], ["insufficient_funds", "not enough funds"], ["toolkit_disabled", "turned off"], ["mystery", "refused (mystery)"]] as const) {
    actions = [effect("f1", { status: "approved", decidedAt: Date.now(), result: { ok: false, code } })];
    const w = await render();
    await settle(() => assert(listCalls > 0, "read"));
    await click(labelled(w.host, "Updates"));
    assert(text(w.host.querySelector("[data-fail]")).includes(words), `${code} reads in plain words`);
    await w.done();
  }
}

console.log("staged decision checks passed");
}
void main();
