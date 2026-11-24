export {};
/**
 * AOS-711 (CAD-2705): the member's `share_request` chat card against the
 * contract fixtures agenticos-v2 copies (tests/fixtures/share-request):
 *
 *  - every state draws as the contract says: pending (with Cancel request),
 *    shared (by whom), declined (the owner's note as typed, or "<Owner> declined."),
 *    cancelled; an entry that is not exactly the contract's is a text line;
 *  - the file that follows a shared card is the ordinary artifact card;
 *  - Cancel request calls /cancel; a 409 says who decided; the chat is asked to read again;
 *  - a pending card that learns it was decided from the server shows it and asks the chat to reload.
 */
declare function require(name: string): any;
declare const process: { cwd(): string; exit(code?: number): never };

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
const fixture = (name: string): any => JSON.parse(require("fs").readFileSync(`${process.cwd()}/tests/fixtures/share-request/${name}.json`, "utf8"));

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "location", "sessionStorage"])
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
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { ShareRequestCard } = require("../src/features/home/ShareRequestCard") as typeof import("../src/features/home/ShareRequestCard");
const { parseShareRequestEntry } = require("../src/features/home/shareRequest") as typeof import("../src/features/home/shareRequest");
const { asEntry, threadItems } = require("../src/features/home/thread") as typeof import("../src/features/home/thread");
const { onThreadReload } = require("../src/lib/masterChanged") as typeof import("../src/lib/masterChanged");
const { WriteGate } = require("../src/features/auth/WriteGate") as typeof import("../src/features/auth/WriteGate");

const entryOf = (raw: Record<string, unknown>, seq = 1) => {
  const e = asEntry({ seq, ...raw });
  assert(e !== null, "entry");
  return e!;
};

// ---- strict parsing: the contract's strictObject ----
{
  for (const name of ["card-pending", "card-shared", "card-declined", "card-declined-no-note", "card-cancelled"]) {
    const model = parseShareRequestEntry(entryOf(fixture(name)));
    assert(model !== null && model.id === "sr_0123456789abcdef0123456789abcdef", `${name} parses`);
  }
  const base = fixture("card-shared");
  const bad: [string, (e: any) => void][] = [
    ["unknown payload key", (e) => { e.payload.html = "<b>x</b>"; }],
    ["unknown status", (e) => { e.payload.status = "approved"; }],
    ["bad request id", (e) => { e.payload.id = "sr_1"; }],
    ["operator role", (e) => { e.role = "operator"; }],
    ["control character in the note", (e) => { e.payload.note = "a\u0007b"; }],
    ["file with an extra key", (e) => { e.payload.file.url = "https://x.test/"; }],
    ["params the app would refuse", (e) => { e.payload.params = { Bad: "x" }; }],
    ["a different version", (e) => { e.payload.v = 2; }],
  ];
  for (const [what, mutate] of bad) {
    const copy = JSON.parse(JSON.stringify(base));
    mutate(copy);
    assert(parseShareRequestEntry(entryOf(copy)) === null, `rejects ${what}`);
  }
}

const html = () => {
  const host = document.createElement("div");
  return { host, root: createRoot(host) };
};
const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
async function settle(check: () => void) {
  for (let i = 0; i < 80; i++) {
    await flush();
    try { check(); return; } catch { /* keep polling */ }
    await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 12)); });
  }
  check();
}
const text = (el: Element | null) => el?.textContent?.replace(/\s+/g, " ").trim() ?? "";

// ---- the board: the request read and the cancel route ----
let reads: unknown[] = [];
const posts: string[] = [];
let cancelAnswer: { status: number; body: unknown } = { status: 200, body: { ok: true, id: "sr_0123456789abcdef0123456789abcdef", status: "cancelled" } };
let confirmAnswer: { status: number; body: unknown } | null = { status: 200, body: { ok: true, outcome: "sent" } };
let confirmInit: RequestInit | undefined;
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname.startsWith("/api/share-requests/offers/") && init?.method === "POST") {
    posts.push(url.pathname + url.search);
    confirmInit = init;
    if (confirmAnswer === null) throw new Error("connection lost");
    return json(confirmAnswer.body, confirmAnswer.status);
  }
  const m = /^\/api\/share-requests\/(sr_[0-9a-f]{32})(?:\/(cancel))?$/.exec(url.pathname);
  if (!m) return json({}, 404);
  if (m[2] === "cancel" && init?.method === "POST") {
    posts.push(url.pathname);
    return json(cancelAnswer.body, cancelAnswer.status);
  }
  const next = reads.shift();
  return next === undefined ? json({ ok: true, request: fixture("card-pending").payload }) : json({ ok: true, request: next });
}) as typeof fetch;

async function mount(raw: Record<string, unknown>, writeBlock: string | null = null) {
  const { host, root } = html();
  document.body.append(host);
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(WriteGate.Provider, { value: writeBlock }, React.createElement(ShareRequestCard, { entry: entryOf(raw) }))));
  });
  const card = () => host.querySelector('[data-kind="share_request"]') as HTMLElement | null;
  const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); };
  return { host, card, done };
}

// ---- each state ----
{
  reads = [];
  const pending = await mount(fixture("card-pending"));
  const c = pending.card()!;
  assert(c.getAttribute("data-share-state") === "pending", "pending state");
  assert(text(c).includes("Accounting · Profit and loss") && text(c).includes("1 Jul – 30 Sep 2026 · Prior year"), `what was asked, with the options (got "${text(c)}")`);
  assert(text(c).includes("“Preparing the Q4 budget review on Thursday.”"), "the member's reason");
  assert(text(c.querySelector("[data-share-outcome]")) === "Waiting for an owner. You'll hear back here.", "pending line");
  assert(c.querySelector("[data-share-cancel]"), "the requester can cancel a pending request");
  await pending.done();

  for (const [name, expect] of [
    ["card-shared", "Shared by Chris"],
    ["card-declined", "Chris declined: Not final until the audit closes. Ask again next week."],
    ["card-declined-no-note", "Chris declined."],
    ["card-cancelled", "You cancelled this request."],
  ] as const) {
    const v = await mount(fixture(name));
    assert(text(v.card()!.querySelector("[data-share-outcome]")) === expect, `${name}: "${expect}" (got "${text(v.card()!.querySelector("[data-share-outcome]"))}")`);
    assert(!v.card()!.querySelector("[data-share-cancel]"), `${name}: no Cancel once decided`);
    await v.done();
  }

  // An entry that is not the contract's is a text line, never a half card.
  const broken = fixture("card-pending");
  broken.payload.extra = true;
  const line = await mount(broken);
  assert(!line.card()!.getAttribute("data-share-state"), "no card");
  assert(text(line.host).includes("Asked an owner to share Accounting · Profit and loss. Waiting for an answer."), "the entry's own text shows");
  await line.done();
}

// ---- the file after a shared card is the ordinary artifact card ----
{
  const items = threadItems({ entries: [entryOf(fixture("card-shared"), 1), entryOf(fixture("artifact-card-after-shared"), 2)], pending: [], exists: true } as any);
  assert(items.map((i) => i.type).join() === "share,artifact", `a share card, then the artifact card (got ${items.map((i) => i.type).join()})`);
  const file = fixture("card-shared").payload.file;
  assert(items[1]!.type === "artifact" && (items[1] as any).entry.payload.id === file.id, "the artifact is the shared file");
}

// ---- Cancel request ----
{
  reads = [];
  posts.length = 0;
  let reloads = 0;
  const stop = onThreadReload(() => { reloads++; });
  const v = await mount(fixture("card-pending"));
  await React.act(async () => { (v.card()!.querySelector("[data-share-cancel]") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle(() => assert(v.card()!.getAttribute("data-share-state") === "cancelled", "the card reads cancelled"));
  assert(posts.length === 1 && posts[0] === "/api/share-requests/sr_0123456789abcdef0123456789abcdef/cancel", "one cancel call");
  assert(reloads >= 1, "the chat is asked to read the thread again");
  await v.done();

  // Someone decided first: 409 names who.
  cancelAnswer = { status: 409, body: fixture("refusal-already-decided") };
  reads = [];
  const w = await mount(fixture("card-pending"));
  // The card's own poll saw a pending request; by the time Cancel is pressed an owner has shared it.
  reads = [fixture("card-shared").payload];
  await React.act(async () => { (w.card()!.querySelector("[data-share-cancel]") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle(() => assert(/Chris already shared this\./.test(text(w.card())), `409 says who decided (got "${text(w.card())}")`));
  await settle(() => assert(w.card()!.getAttribute("data-share-state") === "shared", "and the card follows the request"));
  await w.done();
  stop();
}

// ---- a pending card learns it was decided ----
{
  cancelAnswer = { status: 200, body: {} };
  reads = [fixture("card-declined").payload];
  let reloads = 0;
  const stop = onThreadReload(() => { reloads++; });
  const v = await mount(fixture("card-pending"));
  await settle(() => assert(v.card()!.getAttribute("data-share-state") === "declined", "the decision shows without a reload"));
  assert(/Chris declined: Not final until the audit closes/.test(text(v.card())), "with the note");
  assert(reloads >= 1, "and the chat is asked to read again, so a shared file's card follows");
  await v.done();
  stop();
}

// ---- confirmation offers preserve the exact report and submit only once ----
{
  let reloads = 0;
  const stop = onThreadReload(() => { reloads++; });
  const raw = fixture("card-offered");
  raw.payload.params["custom_filter"] = "<literal & value>";
  const v = await mount(raw);
  assert(text(v.card()).includes('custom filter: "<literal & value>"'), "arbitrary filter and literal value shown");
  assert(text(v.card()).includes("1 Jul – 30 Sep 2026") && text(v.card()).includes("Monthly · Prior year") &&
    text(v.card()).includes("November planning meeting"), "exact options and reason shown");
  assert(!posts.some((p) => p.includes("/offers/")), "opening never sends");
  const confirmationPath = "/api/share-requests/offers/sro_0123456789abcdef0123456789abcdef/confirm?conversation=tc_12345678-1234-4123-8123-123456789abc";
  confirmAnswer = { status: 200, body: { ok: true, outcome: "sent" } };
  await React.act(async () => { (v.card()!.querySelector("[data-share-confirm]") as HTMLElement).click(); });
  await settle(() => assert(text(v.card()).includes("Waiting for an owner"), "accepted request waits for owner"));
  assert(posts.filter((p) => p === confirmationPath).length === 1, "one confirmation for this conversation");
  assert(confirmInit?.credentials === "same-origin" && confirmInit?.body === undefined, "authenticated POST without payload");
  assert(reloads > 0, "confirmation reloads thread");
  assert((v.card()!.querySelector("[data-share-confirm]") as HTMLButtonElement).disabled, "accepted offer cannot be sent twice");
  await v.done();

  for (const [answer, expected] of [
    [{ status: 200, body: { ok: true, outcome: "already_requested" } }, "already pending"],
    [{ status: 404, body: {} }, "no longer available"],
    [{ status: 409, body: {} }, "no longer active"],
    [{ status: 410, body: {} }, "expired"],
    [{ status: 503, body: { error: { code: "owners_unavailable" } } }, "owners are unavailable"],
    [{ status: 503, body: { error: { code: "confirmation_unconfirmed" } } }, "result is unknown"],
    [null, "result is unknown"],
  ] as const) {
    confirmAnswer = answer;
    const w = await mount(fixture("card-offered"));
    const before: number = posts.length;
    await React.act(async () => { (w.card()!.querySelector("[data-share-confirm]") as HTMLElement).click(); });
    await settle(() => assert(text(w.card()).includes(expected), `confirmation result: ${expected}`));
    assert((w.card()!.querySelector("[data-share-confirm]") as HTMLButtonElement).disabled === (expected !== "result is unknown"),
      "only an unconfirmed result can be checked with the same offer");
    assert(posts.length === before + 1, "one POST per confirmation");
    await w.done();
  }
  for (const status of ["expired", "failed", "already_requested"]) {
    const offer = fixture("card-offered");
    offer.payload.status = status;
    const w = await mount(offer);
    assert((w.card()!.querySelector("[data-share-confirm]") as HTMLButtonElement).disabled, `${status} is not confirmable`);
    await w.done();
  }
  const ambiguous = fixture("card-offered");
  ambiguous.payload.status = "submitting";
  const checking = await mount(ambiguous);
  const checkButton = checking.card()!.querySelector<HTMLButtonElement>("[data-share-confirm]");
  assert(checkButton && !checkButton.disabled && text(checkButton) === "Check request", "a saved incomplete attempt can be checked");
  confirmAnswer = { status: 503, body: { error: { code: "owners_unavailable" } } };
  await React.act(async () => { checkButton.click(); });
  await settle(() => assert(!checkButton.disabled && text(checkButton) === "Check request", "temporary owner lookup failure keeps the same offer actionable"));
  confirmAnswer = { status: 200, body: { ok: true, outcome: "sent" } };
  await React.act(async () => { checkButton.click(); });
  await settle(() => assert(text(checking.card()).includes("Waiting for an owner"), "checking waits for an owner when the same request was sent"));
  assert(posts.at(-1) === confirmationPath, "checking uses the original offer");
  await checking.done();
  const locked = await mount(fixture("card-offered"), "Sign in to write.");
  assert(!locked.card()!.querySelector("[data-share-confirm]") && text(locked.card()).includes("Sign in to write."), "write gate prevents confirmation");
  await locked.done();
  stop();
}
console.log("shareRequestCard ok");
}
main().then(() => process.exit(0), (error) => { console.error(error); process.exit(1); });
