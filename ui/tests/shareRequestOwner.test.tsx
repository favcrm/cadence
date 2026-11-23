export {};
/**
 * AOS-711 (CAD-2705): the owner's side of a member's request for an owner-only
 * report, on the real To do rail and detail drawer, against the contract
 * fixtures agenticos-v2 copies (tests/fixtures/share-request):
 *
 *  - the staged `share_request` row is a To do card "<Member> asks for <View> · <period>";
 *  - the drawer says who, what, the reason, and that it shares a read-only PDF;
 *  - Share report and Decline go through /api/share-requests/:id/{share,decline}
 *    (the row's `input.request_id`), never the generic assist approve/deny;
 *  - 409 already_decided says who decided; 503 snapshot_unavailable keeps it pending and retryable;
 *  - a row that is not the contract's is not a decision.
 */
import type { Overview } from "../src/lib/types";
import type { ResourceState } from "../src/lib/cache";

declare function require(name: string): any;
declare const process: { cwd(): string; exit(code?: number): never };

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
const fixture = (name: string): any => JSON.parse(require("fs").readFileSync(`${process.cwd()}/tests/fixtures/share-request/${name}.json`, "utf8"));

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
const { parseStagedRow } = require("../src/features/decisions/stagedWrite") as typeof import("../src/features/decisions/stagedWrite");
const { onThreadReload } = require("../src/lib/masterChanged") as typeof import("../src/lib/masterChanged");

// ---- a fake board: the staged list and the share-request routes ----
const pendingRow = fixture("owner-item-pending");
const REQUEST = pendingRow.input.request_id as string;
let actions: unknown[] = [pendingRow];
const posts: { path: string; body: string | null; contentType: string | null }[] = [];
let answer: (verb: string) => { status: number; body: unknown } = () => ({ status: 200, body: fixture("decision-shared") });
let settled: ((verb: string) => unknown) | null = null;
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname === "/api/assist/actions") return json({ actions });
  const m = /^\/api\/share-requests\/([^/]+)\/(share|decline|cancel)$/.exec(url.pathname);
  if (m && init?.method === "POST") {
    const headers = new Headers(init.headers);
    posts.push({ path: url.pathname, body: init.body === undefined ? null : String(init.body), contentType: headers.get("content-type") });
    const reply = answer(m[2]);
    if (reply.status === 200 && settled) actions = [settled(m[2])];
    return json(reply.body, reply.status);
  }
  if (/^\/api\/assist\/actions\/[^/]+\/(approve|deny)$/.test(url.pathname)) throw new Error("the generic approve/deny must never see a share request");
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
/** A deciding click waits out the drawer's arming delay. */
async function click(element: Element | undefined | null, arm = false) {
  assert(element, "click target exists");
  if (arm) await sleep(850);
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 1 } as any)); });
  await flush();
}
const buttons = (scope: ParentNode) => Array.from(scope.querySelectorAll("button")) as HTMLButtonElement[];
const labelled = (scope: ParentNode, label: string) => buttons(scope).find((b) => b.textContent?.trim() === label);
const drawerEl = () => document.querySelector('.crm-drawer[data-drawer="staged"]') as HTMLElement | null;
const sent = (): number => posts.length;
const text = (el: Element | null) => el?.textContent?.replace(/[⁨⁩]/g, "").replace(/\s+/g, " ").trim() ?? "";

async function render() {
  resetTodoLocal();
  posts.length = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const overview = { status: "ok", inFlight: false, error: null, asOf: Date.now(), data: { needs_me: [] } } as unknown as ResourceState<Overview>;
  await React.act(async () => {
    root.render(React.createElement(NeedsRail, {
      overview, readOnly: false, onOpenIssue: () => {}, permissionsHref: "/settings/permissions",
      onAsk: () => {}, onOpenDecision: () => {}, onAskAgent: () => {}, decisions: true,
    }));
  });
  const cards = () => Array.from(host.querySelectorAll('[data-need="staged_write"]')) as HTMLElement[];
  await settle(() => assert(cards().length === 1, "the request is a To do card"));
  const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); homeDecisions.resetDecisions(); };
  return { host, cards, done };
}
const open = async (v: Awaited<ReturnType<typeof render>>) => {
  await click(v.cards()[0]!.querySelector(".todo-hit"));
  await settle(() => assert(drawerEl(), "the drawer opens"));
};
const setNote = async (value: string) => {
  const area = document.querySelector("#share-decline-note") as HTMLTextAreaElement;
  assert(area, "the note field");
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")!.set!.call(area, value);
    area.dispatchEvent(new win.Event("input", { bubbles: true }));
  });
};

// ---- the fixtures read as the contract says ----
{
  const pending = parseStagedRow(pendingRow);
  assert(pending?.share, "the pending item parses");
  assert(pending.status === "pending" && pending.share.requestId === REQUEST && pending.id !== REQUEST, "the row id and the request id are different things");
  assert(pending.share.requester === "Jamie" && pending.share.viewLabel === "Profit and loss" && pending.share.params?.compare === "prior_year", "who, what and params");
  assert(pending.share.reason === "Preparing the Q4 budget review on Thursday.", "the reason");
  const shared = parseStagedRow(fixture("owner-item-shared"));
  assert(shared?.status === "approved" && shared.decidedBy === "Chris" && shared.result?.code === "shared", "shared: approved by the owner who shared it");
  const declined = parseStagedRow(fixture("owner-item-declined"));
  assert(declined?.status === "denied" && declined.decisionReason === "Not final until the audit closes. Ask again next week.", "declined: the note");
  const bad = JSON.parse(JSON.stringify(pendingRow));
  bad.input.extra = "x";
  assert(parseStagedRow(bad) === null, "an input with an unknown key is not a decision");
  bad.input = { ...pendingRow.input, params: "from=2026-07-01&from=x&to=" };
  assert(parseStagedRow(bad) === null, "params the app would refuse are not a decision");
}

// ---- the card and the drawer ----
{
  actions = [pendingRow];
  settled = (verb) => fixture(verb === "share" ? "owner-item-shared" : "owner-item-declined");
  const v = await render();
  const c = v.cards()[0]!;
  assert(text(c.querySelector(".todo-title")) === "Jamie asks for Profit and loss · 1 Jul – 30 Sep 2026 · Prior year", `headline (got "${text(c.querySelector(".todo-title"))}")`);
  assert(text(c.querySelector(".rc-source")) === "Accounting", "the app is the source");
  assert(text(c.querySelector(".todo-meta")) === "Preparing the Q4 budget review on Thursday.", "the reason is the summary");
  await open(v);
  const d = drawerEl()!;
  const body = text(d);
  assert(body.includes("Share a report with Jamie?"), "title");
  assert(body.includes("Jamie · member") && body.includes("Accounting · Profit and loss") && body.includes("1 Jul – 30 Sep 2026 · Prior year"), `who and what with the options line (got "${body}")`);
  assert(body.includes("Preparing the Q4 budget review on Thursday."), "reason");
  assert(body.includes("Shares a read-only PDF of this report. Jamie gets no access to the books."), "what sharing does");
  assert(labelled(d, "Share report") && labelled(d, "Decline…"), "both decisions");
  assert(!labelled(d, "Approve") && !labelled(d, "Deny"), "no generic approve/deny");

  // 503: it could not be prepared. Still pending, said plainly, and Share can be tried again.
  answer = () => ({ status: 503, body: { ok: false, error: { code: "snapshot_unavailable", message: "The report could not be prepared." } } });
  await click(labelled(d, "Share report"), true);
  await settle(() => assert(/couldn't be prepared/.test(text(drawerEl())), "the 503 is explained"));
  assert(sent() === 1 && posts[0]!.path === `/api/share-requests/${REQUEST}/share` && posts[0]!.body === null, "one share call, to the request id, no body");
  assert(drawerEl() && /Share a report with Jamie\?/.test(text(drawerEl())), "the request is still there, pending");
  assert(!/aria-disabled/.test(labelled(drawerEl()!, "Share report")!.outerHTML), "Share can be tried again");

  // Retry succeeds: the list follows the request and the drawer leaves.
  answer = () => ({ status: 200, body: fixture("decision-shared") });
  let reloads = 0;
  const stop = onThreadReload(() => { reloads++; });
  await click(labelled(drawerEl()!, "Share report"), true);
  await settle(() => assert(!drawerEl(), "the drawer closes after the decision"));
  await settle(() => assert(v.cards().length === 0, "the row left To do"));
  assert(sent() === 2 && posts[1]!.path === `/api/share-requests/${REQUEST}/share`, "the retry is a second share call");
  assert(reloads >= 1, "the thread is asked to read again");
  assert(/Shared · Jamie · Profit and loss/.test(text(v.host)), `the rail says what happened (got "${text(v.host)}")`);
  stop();
  await v.done();
}

// ---- 409: first decision wins ----
{
  actions = [pendingRow];
  settled = null;
  const v = await render();
  await open(v);
  answer = () => ({ status: 409, body: fixture("refusal-already-decided") });
  actions = [fixture("owner-item-shared")];
  await click(labelled(drawerEl()!, "Share report"), true);
  await settle(() => assert(/Chris already shared this\./.test(text(drawerEl())), `who decided is shown (got "${text(drawerEl())}")`));
  assert(labelled(drawerEl()!, "Continue"), "Continue leaves the drawer");
  assert(!labelled(drawerEl()!, "Share report"), "no second decision is offered");
  await v.done();
}

// ---- decline: the note as typed; blank is none ----
{
  actions = [pendingRow];
  settled = () => fixture("owner-item-declined");
  answer = () => ({ status: 200, body: fixture("decision-declined") });
  const v = await render();
  await open(v);
  await click(labelled(drawerEl()!, "Decline…"));
  assert(/Decline Jamie's request/.test(text(drawerEl())) && /Note to Jamie \(optional\)/.test(text(drawerEl())), "the decline step names the member and the optional note");
  await setNote(fixture("decline-body").note);
  await click(labelled(drawerEl()!, "Decline request"), true);
  await settle(() => assert(!drawerEl(), "closed after declining"));
  assert(sent() === 1 && posts[0]!.path === `/api/share-requests/${REQUEST}/decline` && posts[0]!.contentType === "application/json", "a decline call");
  assert(JSON.stringify(JSON.parse(posts[0]!.body!)) === JSON.stringify(fixture("decline-body")), "the body is the fixture's, the note as typed");
  await v.done();

  actions = [pendingRow];
  const w = await render();
  await open(w);
  await click(labelled(drawerEl()!, "Decline…"));
  await setNote("   ");
  await click(labelled(drawerEl()!, "Decline request"), true);
  await settle(() => assert(!drawerEl(), "closed after declining without a note"));
  assert(sent() === 1 && posts[0]!.body === null, "a blank note sends no note");
  await w.done();
}

// ---- not an owner: the server refuses and the drawer says so ----
{
  actions = [pendingRow];
  settled = null;
  answer = () => ({ status: 403, body: { ok: false, error: { code: "owner_required", message: "Only an owner may share a report." } } });
  const v = await render();
  await open(v);
  await click(labelled(drawerEl()!, "Share report"), true);
  await settle(() => assert(/Only the workspace owner can decide this request\./.test(text(drawerEl())), "403 is explained"));
  await v.done();
}
console.log("shareRequestOwner ok");
}
main().then(() => process.exit(0), (error) => { console.error(error); process.exit(1); });
