import "fake-indexeddb/auto";
import { streamFixture } from "./streamFixture";
/** CAD-1098 S3: per-app assistant conversations in the shell chat. */
import type * as SeenModule from "../src/features/app-shell/chat/unreadSeen";
import type * as ConversationModule from "../src/features/app-shell/chat/Conversation";
import type { MasterState } from "../src/lib/types";
declare function require(name: string): any;
declare const process: { cwd(): string };
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(a: unknown, e: unknown, why: string) {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
Object.defineProperty(win.document, "visibilityState", { value: "visible", configurable: true });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
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
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const AppShellComponent = require("../src/features/app-shell/AppShell").default;
const AppShell = (props: React.ComponentProps<typeof AppShellComponent>) =>
  React.createElement(LocaleProvider, null,
    React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(AppShellComponent, props) }));
const { resources } = require("../src/lib/resources") as typeof import("../src/lib/resources");
const { parseSlash, autoSelectCampaignConversation, conversationThread, conversationList } = require("../src/features/app-shell/conversationClient") as typeof import("../src/features/app-shell/conversationClient");
const { readSeen } = require("../src/features/app-shell/chat/unreadSeen") as typeof SeenModule;

equal([parseSlash("/new"), parseSlash(" /CLEAR "), parseSlash("/new now"), parseSlash("hello")], ["new", "new", null, null], "only the bare commands are commands");

const inst = (id: string, name: string) => ({ install_id: id, title: name, name, version: "0.1.0", digest: "d", catalog_generation: "g", approved: true, approval: { state: "approved", epoch: 1 }, storage_kind: "workspace", files: [], capabilities: null, connection_slots: [] });
let crmContext = "ctx-a";
const ctxs = (id: string) => ({ contexts: [{ id: id === "install-crm" ? crmContext : "ctx-a", install_id: id, revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } }] });
const ent = (seq: number, role: string, kind: string, text: string, message: string | null) => ({ seq, role, kind, message, text, created: "2026-10-03T00:00:00Z", payload: null });
const conv = (id: string, over: object = {}) => ({ id, title: null, subject: null, is_general: false, ...over });
const convs: Record<string, any[]> = {
  "install-crm": [conv("c-gen", { is_general: true }), conv("c-camp", { subject: "campaign:cmp-1", title: "Spring launch" })],
  "install-other": [conv("o-gen", { is_general: true })],
  "install-social": [conv("s-gen", { is_general: true }), conv("s-full", { title: "Has messages" })],
};
const threads: Record<string, any[]> = {
  "c-gen": [ent(1, "operator", "message", "general question", "m-gen"), ent(2, "agent", "turn_result", "general answer", "m-gen")],
  "c-camp": [
    ent(1, "operator", "message", "campaign brief", "m-camp"),
    { ...ent(2, "system", "message", "Permission requested (permission-old, risk low): old command", "old-decision"), payload: { source: "permission" } },
  ],
  "o-gen": [],
  "s-gen": [],
  "s-full": [ent(1, "operator", "message", "earlier question", "m-s")],
};
let turn: any = null;
// CAD-1700: the master queue in delivery order, and the board commands posted.
let queue: { message: string; conversation: string | null }[] = [];
let operations: unknown[] = [];
let controlledTranscript = false;
let holdReconnect = false;
let disconnectTranscript: (() => void) | null = null;
let heldOperations: Array<(response: Response) => void> | null = null;
const commandPosts: unknown[] = [];
// Set inside a check to hold one send in flight (pending state "sending").
let deferSend: ((resolve: (r: Response) => void) => void) | null = null;
const messagePosts: any[] = [];
const createPosts: any[] = [];
let created = 0;
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
let hostedIdentity = false;
let identityUnavailable = false;
const masterSnapshot = (conversation: string): MasterState => ({
  alias: "master",
  identity: { name: null, tone: null, look: "robot", revision: "fixture" },
  provider: "p", endpoint_kind: "k", live: true,
  turn: hostedIdentity && conversation === "home-thread" ? null : turn,
  queue: hostedIdentity && conversation === "home-thread" ? [] : queue,
  ...(hostedIdentity ? { actor_identity: {
    scope: "personal", conversation_id: conversation,
    identity: { name: "Poppy", tone: "Warm", look: "designer", revision: 1 },
  } } : {}),
});
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  const method = init?.method ?? "GET";
  if (url.pathname.endsWith("/stream")) {
    if (!controlledTranscript || url.pathname !== "/api/threads/master/stream")
      return streamFixture(url.href, masterSnapshot(url.searchParams.get("conversation") ?? "home-thread"), init?.signal);
    if (holdReconnect) return new Response(null, { status: 503 });
    let closed = false;
    const body = new ReadableStream<Uint8Array>({ start(controller) {
      controller.enqueue(new TextEncoder().encode(`event: master\ndata: ${JSON.stringify(masterSnapshot(url.searchParams.get("conversation") ?? "home-thread"))}\n\n`));
      const close = () => { if (!closed) { closed = true; controller.close(); } };
      disconnectTranscript = close;
      init?.signal?.addEventListener("abort", close, { once: true });
    }, cancel() { closed = true; } });
    return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
  }
  // The CRM package's own descriptor, served pinned to the install's digest.
  if (url.pathname === "/api/app-installations/install-crm/chat-descriptor") {
    return json({ descriptor: JSON.parse(require("fs").readFileSync(require("path").join(process.cwd(), "..", "tests", "fixtures", "apps", "crm", "app-chat.json"), "utf8")), digest: "d", app: "crm" });
  }
  if (url.pathname === "/api/app-installations/install-social/chat-descriptor") {
    return json({ descriptor: { contract: "app-chat/v1", app: "social-content", contexts: [{ id: "main", label: "Social Content", prompts: ["Fetch posts"] }], attachments: [], directives: [], subjects: [] }, digest: "d", app: "social-content" });
  }
  if (url.pathname.endsWith("/chat-descriptor")) return json({ error: "no chat descriptor" }, 404);
  const m = url.pathname.match(/^\/api\/app-installations\/([^/]+)(\/contexts|\/conversations)?$/);
  if (m && m[2] === "/conversations") {
    if (method === "POST") {
      createPosts.push({ install: m[1], body: JSON.parse(String(init!.body)) });
      const c = conv(`new-${++created}`);
      convs[m[1]].push(c);
      threads[c.id] = [];
      return json({ conversation: c });
    }
    return json({ conversations: convs[m[1]] });
  }
  if (m && m[2] === "/contexts") return json(ctxs(m[1]));
  if (m && m[1] === "install-social") return json({ ...inst(m[1], "social-content"), executable: true, files: ["app.md", "screens/main/screens.json"] });
  if (m) return json(inst(m[1], m[1] === "install-crm" ? "crm" : "reports"));
  if (url.pathname === "/api/threads/master/messages") {
    messagePosts.push(JSON.parse(String(init!.body)));
    if (deferSend) return new Promise<Response>((r) => deferSend!(r));
    return json({ message: "x", state: "queued", duplicate: false });
  }
  if (url.pathname === "/api/threads/master") {
    const id = url.searchParams.get("conversation") ?? "";
    return json({ thread: { id: id || "home-thread", alias: "master" }, entries: threads[id] ?? [], more_before: false });
  }
  if (url.pathname === "/api/master/state") return identityUnavailable
    ? json({ error: "Identity unavailable" }, 503)
    : json(masterSnapshot(url.searchParams.get("conversation") ?? "home-thread"));
  if (url.pathname === "/api/master/command") {
    commandPosts.push(JSON.parse(String(init!.body)));
    if (hostedIdentity) { turn = null; queue = []; }
    return json({ command: "x", ok: true, result: {} });
  }
  if (url.pathname === "/api/crm-send/list") return json({ sends: [] });
  if (url.pathname === "/api/crm-send/origin") return json({ unsubscribe_origin: null, stored: false });
  if (url.pathname === "/api/master/permissions") return json({ rules: [], requests: [{ id: "permission-old", status: "allowed_once", command: "old command", cwd: null, reason: "Previously decided", risk: "low", prefix: [] }] });
  if (url.pathname.endsWith("/assistant/actions")) return json({ actions: [] });
  if (url.pathname.endsWith("/assistant/operations"))
    return heldOperations === null ? json({ operations }) : new Promise<Response>((resolve) => heldOperations!.push(resolve));
  if (url.pathname.endsWith("/assistant/permissions")) return json({ permissions: [] });
  return json({ records: [], truncated: false, next_cursor: null, segments: [], exclusions: [], suppressions: [], contents: [], proposals: [], bindings: [] });
}) as typeof fetch;

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });
const settle = async () => { for (let i = 0; i < 8; i++) await flush(); };
async function mount(installId: string, viewer = { operator: true, readOnly: false }, context = "ctx-a") {
  history.pushState(null, "", `/app-installations/${installId}?ctx=${context}`);
  await React.act(async () => {
    win.dispatchEvent(new win.PopStateEvent("popstate"));
    root.render(React.createElement(AppShell, { installId, viewer }));
  });
  await settle();
}
// CAD-1700: the header names the open conversation; the Conversations menu
// (history icon) lists them, the current one checked.
const histBtn = () => host.querySelector('[aria-label="Conversations"]') as HTMLButtonElement;
const rows = () => Array.from(host.querySelectorAll('[data-chat-history] [role="menuitemradio"]')) as HTMLButtonElement[];
async function openHistory() {
  if (!host.querySelector("[data-chat-history]")) {
    await React.act(async () => { histBtn().dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  }
}
const rowName = (r: Element) => r.querySelector(".app-chat-history-name")?.textContent ?? null;
const optionLabels = async () => { await openHistory(); return rows().map(rowName); };
const openLabel = () => host.querySelector("[data-chat-conv-label]")?.textContent?.replace(/^·\s*/, "") ?? null;
const pane = () => (host.querySelector("[data-chat-pane]") as HTMLElement).textContent ?? "";
const box = () => host.querySelector("#app-shell-chat-box") as HTMLTextAreaElement;
const newButton = () => host.querySelector("[data-chat-new]") as HTMLButtonElement;
async function pick(label: string) {
  await openHistory();
  const row = rows().find((r) => rowName(r) === label);
  assert(row, `history lists ${label}`);
  await React.act(async () => { row!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
}
async function type(text: string, key = "Enter") {
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(box(), text);
    box().dispatchEvent(new Event("input", { bubbles: true }));
  });
  await React.act(async () => { box().dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true })); });
  await settle();
}

await mount("install-crm");

// Picker: General, the campaign conversation, and + New; only the selected conversation's entries show.
equal(await optionLabels(), ["General", "Spring launch"], "history lists General and the campaign conversation");
assert(newButton() && newButton().getAttribute("aria-label") === "New conversation", "the + icon (New conversation) sits in the header tools");
assert(newButton().closest(".app-chat-head-tools") && newButton().nextElementSibling === histBtn(), "New sits before the history button in the header tools");
assert(!host.querySelector('select[aria-label="Conversation"], [data-chat-home-link], [data-chat-conversations]'), "no conversation select, row or Home link");
equal(openLabel(), "General", "General is the default and the scope row names it");
await optionLabels();
equal(rows()[0].getAttribute("aria-checked"), "true", "the Conversations menu checks General as the open one");
assert(pane().includes("general answer") && !pane().includes("campaign brief"), "General shows only its own thread");
await pick("Spring launch");
assert(pane().includes("campaign brief") && !pane().includes("general answer"), "the campaign conversation shows only its own thread");

// A send names the selected conversation as a selector only.
await type("hello campaign");
equal(messagePosts.length, 1, "one message sent");
equal(messagePosts[0].conversation, "c-camp", "the send targets the selected conversation id");
for (const forged of ["subject", "scope", "thread", "install_id", "is_general"]) assert(!(forged in messagePosts[0]), `no client ${forged} field`);

// /new and /clear create a fresh conversation, never a message.
await type("/new");
equal(messagePosts.length, 1, "/new is not sent as a message");
equal(createPosts, [{ install: "install-crm", body: { context_id: "ctx-a" } }], "/new creates one conversation with no subject");
assert(openLabel() !== null, "the fresh conversation has an open-conversation label");
await openHistory(); // CAD-1700: the shareable link lives in the Conversations menu.
const newConversationLink = host.querySelector("[data-chat-conversation-link]") as HTMLAnchorElement | null;
assert(newConversationLink, "the fresh conversation has a shareable link");
equal(new URL(newConversationLink.href).searchParams.get("conversation"), "new-1", "the fresh conversation is the selected conversation");
assert((await optionLabels()).length === 3 && (await optionLabels()).includes("General"), "older conversations stay listed");
equal(box().value, "", "the command is cleared from the composer");
await type("/clear");
equal(messagePosts.length, 1, "/clear is not sent as a message");
equal(createPosts.length, 2, "/clear creates another conversation");
await React.act(async () => { newButton().dispatchEvent(new MouseEvent("click", { bubbles: true })); });
await settle();
equal(createPosts.length, 3, "+ New is the same call");
equal(createPosts.map((p) => p.body), [{ context_id: "ctx-a" }, { context_id: "ctx-a" }, { context_id: "ctx-a" }], "New never carries a subject");

// Create-on-first-send: opening a campaign page creates nothing; the first message
// creates the conversation (idempotent subject) and is sent to the returned id.
{
  const createsBefore = createPosts.length, sendsBefore = messagePosts.length;
  await React.act(async () => { await autoSelectCampaignConversation("install-crm", "cmp-fresh"); });
  await settle();
  equal(createPosts.length, createsBefore, "opening a campaign page creates no conversation");
  equal(openLabel(), "New campaign conversation (unsaved)", "the unsaved campaign draft is named in the scope row");
  await type("first brief");
  equal(createPosts.length, createsBefore + 1, "exactly one create on first send");
  equal(createPosts[createPosts.length - 1].body, { context_id: "ctx-a", subject: "campaign:cmp-fresh" }, "the create names the campaign subject");
  equal(messagePosts.length, sendsBefore + 1, "the first message is sent");
  equal(messagePosts[messagePosts.length - 1].conversation, `new-${created}`, "sent to the id the create returned");
}

// CAD-1700: the turn bar. Another conversation's running turn shows nothing
// here; this conversation's own queued message reads "Queued after …".
const bar = () => host.querySelector("[data-turn-bar]") as HTMLElement | null;
const barButton = (label: string) =>
  Array.from(bar()?.querySelectorAll("button") ?? []).find((b) => b.textContent === label) ?? null;
// CAD-1880: the running turn's Stop is the composer's send slot; a working
// turn paints no dock row at all.
const stopButton = () => host.querySelector<HTMLButtonElement>('[aria-label="Stop this turn"]');
const typingBubble = () => host.querySelector('[data-kind="typing"]');
await pick("Spring launch");
turn = { state: "working", message: "m-gen-elsewhere", conversation: "c-gen" };
await React.act(async () => { await resources.masterState.refresh(); });
await settle();
assert(bar()?.dataset.state !== "working" && bar()?.dataset.state !== "queued" && stopButton() === null, `another conversation's running turn shows no Working row and no Stop here (got ${bar()?.dataset.state})`);
turn = null;
await React.act(async () => { await resources.masterState.refresh(); });
await settle();

// The selected conversation's own turn: Sending while its POST is in flight;
// Queued (with Cancel, never Stop) once the daemon queues it behind another
// conversation's turn; Working with Stop in the composer carrying this exact
// message once it runs. Nothing bleeds into another conversation.
await pick("Spring launch");
{
  let resolveSend: ((r: Response) => void) | null = null;
  deferSend = (r) => { resolveSend = r; };
  await type("queued brief update");
  assert(resolveSend !== null, "the send is still in flight");
  equal(bar()?.dataset.state, "sending", "an in-flight send shows Sending");
  await React.act(async () => { resolveSend!(json({ message: "x", state: "queued", duplicate: false })); });
  await settle();
  deferSend = null;

  const sentMessage = messagePosts[messagePosts.length - 1].message;
  turn = { state: "working", message: "m-gen-elsewhere", conversation: "c-gen" };
  queue = [{ message: sentMessage, conversation: "c-camp" }];
  await React.act(async () => { await resources.masterState.refresh(); });
  await settle();
  equal(bar()?.dataset.state, "queued", "an owned queued message shows Queued");
  assert(bar()!.textContent!.includes("after General"), `Queued names what is ahead; got ${bar()?.textContent}`);
  assert(barButton("Stop") === null && stopButton() === null, "a queued message offers no Stop");
  turn = { state: "working", message: "m-home", conversation: "home-thread" };
  await React.act(async () => {
    await resources.masterThread.refresh();
    await resources.masterState.refresh();
  });
  await settle();
  assert(bar()!.textContent!.includes("after Home"), "the real Home thread ID is labelled Home");
  assert(barButton("View") !== null, "a Home predecessor has a View action");
  await React.act(async () => { barButton("Cancel")!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  equal(commandPosts.at(-1), { command: "cancel", message: sentMessage }, "Cancel names exactly this conversation's queued message");

  threads["c-camp"].push(ent(3, "operator", "message", "queued brief update", sentMessage));
  await React.act(async () => { await conversationThread("c-camp").refresh(); });
  // The server note retires the optimistic row before the owned message
  // reaches the queue head. Position 21 must still be tracked from metadata.
  queue = [
    ...Array.from({ length: 20 }, (_, index) => ({ message: `ahead-${index}`, conversation: "c-gen" })),
    { message: sentMessage, conversation: "c-camp" },
  ];
  await React.act(async () => { await resources.masterState.refresh(); });
  await settle();
  equal(bar()?.dataset.state, "queued", "position 21 stays Queued after optimistic retirement");
  assert(bar()!.textContent!.includes("after General"), "position 21 names its immediate predecessor, not the running Home turn");
  assert(barButton("View") && barButton("Cancel") && !barButton("Stop"), "position 21 retains View and Cancel, never Stop");
  turn = { state: "working", message: sentMessage, conversation: "c-camp", summary: "customers.search" };
  queue = [];
  // No timer: a `master` stream frame invalidates the state (CAD-1700).
  await React.act(async () => { await resources.masterState.invalidate(); });
  await settle();
  assert(bar() === null, "the owned running turn paints no dock row after a master frame");
  assert(typingBubble() !== null, "the owned running turn shows its inline typing bubble");
  assert(stopButton() !== null, "the owned running turn offers Stop in the composer");
  // CAD-1859 + CAD-1884: the app pane paints the same inline typing bubble as
  // Home. The run that owns the turn paints it, so there is exactly one and
  // no second standalone row behind it.
  const typingRows = Array.from(host.querySelectorAll('.app-chat-list [data-kind="typing"]')) as HTMLElement[];
  assert(typingRows.length === 1, `a working turn paints exactly one typing bubble (got ${typingRows.length})`);
  const owner = typingRows[0].closest("[data-chat-run], [data-chat-typing]");
  assert(owner !== null, "the typing bubble is the turn's own row or the pane's tail row");
  const paneRows = Array.from(host.querySelectorAll(".app-chat-list > li")) as HTMLElement[];
  assert(paneRows.length > 0 && paneRows[paneRows.length - 1] === owner, "the typing turn is the last row in the pane");
  // CAD-1880: `writing` is the same running turn with its assistant text
  // last. The dock row is gone, so the bubble must survive that transition
  // or the pane would show nothing at all while it writes.
  threads["c-camp"].push(ent(4, "agent", "assistant_text", "Drafting the brief", sentMessage));
  await React.act(async () => { await conversationThread("c-camp").refresh(); });
  await settle();
  assert(bar() === null, "a writing turn paints no dock row");
  assert(
    host.querySelector('.app-chat-list [data-chat-run] [data-kind="typing"]') !== null,
    "a writing turn keeps the inline typing bubble in the pane",
  );
  // CAD-1886: two days of transcript paint two day rules, and the rule
  // belongs to the list, not to a turn.
  threads["c-camp"].push({ ...ent(9, "operator", "message", "yesterday's note", "m-yest"), created: "2026-10-02T00:00:00Z" });
  await React.act(async () => { await conversationThread("c-camp").refresh(); });
  await settle();
  const dayRules = Array.from(host.querySelectorAll("[data-day-sep]"));
  assert(dayRules.length === 2, `two days paint two day rules (got ${dayRules.length})`);
  assert(dayRules.every((rule) => !rule.closest("[data-chat-run]")), "a day rule is its own list row");
  // The store state the daemon's own operator entry produces: the optimistic
  // row retires, so nothing is left sending or working for this pane.
  turn = null;
  await React.act(async () => {
    const held = conversationThread("c-camp").get().data;
    if (held) conversationThread("c-camp").write(() => ({ ...held, pending: [] }));
    await resources.masterState.refresh();
  });
  await settle();
  assert(host.querySelectorAll(".app-chat-list [data-chat-typing]").length === 0, "a settled turn leaves no typing row behind");
  turn = { state: "working", message: sentMessage, conversation: "c-camp", summary: "customers.search" };
  await React.act(async () => { await resources.masterState.invalidate(); });
  await settle();
  operations = [{
    id: "op-tags", action_id: "customer.tags.update", status: "pending_permission", revision: 1,
    summary: "Tag the selected customer", resource_refs: [],
    permission_request: {
      reason: "This changes one customer", allow_always: true,
      scope: { install_id: "install-crm", context_id: "ctx-a", action_id: "customer.tags.update", resource_id: "ada" },
      preview: { customer_label: "Ada", before_tags: [], after_tags: ["vip"], expected_revision: 1 },
    },
  }];
  const { noteMasterChanged } = require("../src/lib/masterChanged");
  await React.act(async () => { noteMasterChanged(); });
  await settle();
  assert(barButton("Review") !== null && barButton("Stop") === null && stopButton() === null, "a pushed operation permission replaces Stop with Review");
  const approval = host.querySelector('[data-operation-status="pending_permission"]') as HTMLElement;
  assert(approval, "the actionable app approval card is mounted");
  assert(host.querySelector('[data-permission-thread="permission-old"] [data-permission-decided]'), "a decided historic permission precedes the current operation");
  const scrolled = new Set<HTMLElement>();
  const originalScroll = HTMLElement.prototype.scrollIntoView;
  HTMLElement.prototype.scrollIntoView = function () { scrolled.add(this); };
  try {
    await React.act(async () => { barButton("Review")!.click(); });
    assert(scrolled.has(approval), "Review reaches the pending operation, not a historic decided permission");
  } finally {
    HTMLElement.prototype.scrollIntoView = originalScroll;
  }
  operations = [];
  await React.act(async () => { noteMasterChanged(); });
  await settle();
  await React.act(async () => { stopButton()!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  equal(commandPosts.at(-1), { command: "stop", message: sentMessage }, "Stop names exactly this conversation's running turn");

  await pick("General");
  assert(bar() === null, "the turn does not bleed into another conversation");
  turn = null;
  await React.act(async () => { await resources.masterState.refresh(); });
  await settle();
  await pick("Spring launch");
}

// A failed stored request retains its app binding even after a context
// switch and a later draft capture another context in the same conversation.
{
  threads["c-gen"].push({
    ...ent(3, "operator", "message", "retry the original request", "m-stored-failure"),
    payload: { app: { install_id: "install-crm", context_id: "ctx-a" } },
  });
  await React.act(async () => { await conversationThread("c-gen").refresh(); });
  await mount("install-other");
  crmContext = "ctx-b";
  await mount("install-crm", { operator: true, readOnly: false }, "ctx-b");
  await pick("General");
  await type("current-context probe");
  equal(messagePosts.at(-1).app, { install_id: "install-crm", context_id: "ctx-b" }, "the mounted composer is now in context B");
  threads["c-gen"].push(ent(4, "operator", "message", "current-context probe", messagePosts.at(-1).message));
  await React.act(async () => { await conversationThread("c-gen").refresh(); });
  threads["c-gen"].push({
    ...ent(5, "agent", "turn_result", "Could not finish", "m-stored-failure"),
    created: new Date().toISOString(), payload: { status: "failed", error: "Try again" },
  });
  await React.act(async () => { await conversationThread("c-gen").refresh(); });
  await settle();
  assert(barButton("Retry"), "the stored failed turn has a Retry action");
  await React.act(async () => {
    conversationThread("c-gen").write((state) => ({ ...state!, pending: [...state!.pending, {
      message: "m-older-transport-failure", text: "older failed transport brief", state: "failed", at: Date.now(),
      refs: [{ kind: "issue", id: "CAD-A" }], attachments: [{ id: "file-a" }],
      app: { install_id: "install-crm", context_id: "ctx-a" },
    }] }));
  });
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(box(), "later unsent draft");
    box().dispatchEvent(new Event("input", { bubbles: true }));
  });
  await React.act(async () => { barButton("Retry")!.click(); });
  await settle();
  equal(messagePosts.at(-1).app, { install_id: "install-crm", context_id: "ctx-a" }, "Retry uses the failed request's binding, never the current context or later draft");
  equal(messagePosts.at(-1).text, "retry the original request", "Retry transmits the stored request, not the later draft");
  equal(box().value, "later unsent draft", "Retry preserves the later draft");
  const storedRetryMessage = messagePosts.at(-1).message;
  const olderRow = Array.from(host.querySelectorAll(".app-chat-list li")).find((row) => row.textContent?.includes("older failed transport brief"));
  const rowRetry = olderRow && Array.from(olderRow.querySelectorAll("button")).find((button) => button.textContent?.trim() === "Retry");
  assert(rowRetry, "the unrelated failed send retains its own row Retry");
  await React.act(async () => { rowRetry.click(); });
  await settle();
  equal(messagePosts.at(-1).message, "m-older-transport-failure", "the row Retry alone resends its own original id");
  equal(messagePosts.at(-1).refs, [{ kind: "issue", id: "CAD-A" }], "row Retry retains the captured references");
  equal(messagePosts.at(-1).attachments, [{ id: "file-a" }], "row Retry retains the captured files");
  threads["c-gen"].push(ent(6, "operator", "message", "retry the original request", storedRetryMessage));
  await React.act(async () => {
    await conversationThread("c-gen").refresh();
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(box(), "");
    box().dispatchEvent(new Event("input", { bubbles: true }));
  });
  await mount("install-other");
  crmContext = "ctx-a";
  await mount("install-crm");
}

// CAD-1810: an app page may open with the assistant collapsed (unless a
// decision is waiting); this test needs the pane visible.
if (host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed")) {
  await React.act(async () => { (host.querySelector(".app-chat-rail") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
}
assert(!host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed"), "the assistant is visible for this check");

// A hidden but mounted pane must not clear another tab's unread state.
await pick("General");
await React.act(async () => {
  Object.defineProperty(win.document, "visibilityState", { value: "hidden", configurable: true });
  win.document.dispatchEvent(new win.Event("visibilitychange"));
  convs["install-crm"].find((c) => c.id === "c-gen").reply_seq = 5;
  await conversationList("install-crm").refresh();
});
await settle();
assert(((await readSeen("install-crm"))["c-gen"] ?? 0) < 5, "the mounted hidden pane leaves the reply unseen");
await React.act(async () => {
  Object.defineProperty(win.document, "visibilityState", { value: "visible", configurable: true });
  win.document.dispatchEvent(new win.Event("visibilitychange"));
});
await settle();
equal((await readSeen("install-crm"))["c-gen"], 5, "returning to the visible pane marks the displayed reply seen");

// Collapse state is per app.
await React.act(async () => { (host.querySelector(".app-chat-collapse") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
assert(host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed"), "CRM chat collapses");
await mount("install-other");
assert(host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed"), "another app opens with its own assistant collapsed");
await React.act(async () => { (host.querySelector(".app-chat-rail") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
await settle();
assert(!host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed"), "another app's chat expands independently");
equal(await optionLabels(), ["General"], "another app lists only its own conversations");
await mount("install-crm");
assert(host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed"), "the CRM chat stays collapsed");
await mount("install-other");
assert(!host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed"), "the other app's own choice persists");

// A cached legacy response leaves the real composer editable while the next
// conversation-list request is pending. Draft text must survive when that
// same request resolves to the install's General conversation.
{
  const fixtureFetch = globalThis.fetch;
  let listMode: "legacy" | "pending" = "legacy";
  let resolveModernList: (response: Response) => void = () => { throw new Error("modern list request did not start"); };
  let modernListPending = false;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/app-installations/install-crm/conversations" && (init?.method ?? "GET") === "GET") {
      if (listMode === "legacy") return json({ error: "not found" }, 404);
      modernListPending = true;
      return new Promise<Response>((resolve) => { resolveModernList = resolve; });
    }
    return fixtureFetch(input as RequestInfo | URL, init);
  }) as typeof fetch;
  try {
    await React.act(async () => { (host.querySelector(".app-chat-collapse") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    await settle();
    await React.act(async () => { await conversationList("install-crm").refresh(); });
    await settle();
    assert(box() && !box().disabled, "the actual legacy-mode composer is editable");

    listMode = "pending";
    let refresh!: Promise<void>;
    await React.act(async () => { refresh = conversationList("install-crm").refresh(); });
    await settle();
    assert(modernListPending, "the next real conversation-list lookup is pending");
    assert(!box().disabled, "cached legacy data keeps the mounted composer editable during refresh");
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(box(), "draft during legacy refresh");
      box().dispatchEvent(new Event("input", { bubbles: true }));
    });
    equal(box().value, "draft during legacy refresh", "the text was entered through the mounted textarea");
    await React.act(async () => { resolveModernList(json({ conversations: [conv("c-gen", { is_general: true })] })); });
    await React.act(async () => { await refresh!; });
    await settle();
    equal(openLabel(), "General", "the same app resolves to its General conversation");
    equal(box().value, "draft during legacy refresh", "the real composer keeps the draft after General resolves");
  } finally {
    if (modernListPending) resolveModernList(json({ conversations: [conv("c-gen", { is_general: true })] }));
    globalThis.fetch = fixtureFetch;
  }
}

// CAD-1326: Social's quick-action rows show only while the open conversation
// has no messages; they return when a new empty conversation starts.
await mount("install-social");
const prompts = () => host.querySelector("[data-chat-prompts]");
assert(prompts(), "an empty conversation shows the quick actions");
await pick("Has messages");
assert(!prompts(), "a conversation with messages hides the quick actions");
await React.act(async () => { newButton().dispatchEvent(new MouseEvent("click", { bubbles: true })); });
await settle();
assert(prompts(), "a new empty conversation brings the quick actions back");

// A read-only viewer cannot create a conversation by any route.
await mount("install-crm", { operator: true, readOnly: true });
const before = createPosts.length;
assert(newButton().disabled, "+ New is disabled read-only");
assert(box().disabled, "the composer (and so /new) is disabled read-only");
await React.act(async () => { newButton().dispatchEvent(new MouseEvent("click", { bubbles: true })); });
await settle();
equal(createPosts.length, before, "no conversation is created read-only");
await pick("General");
turn = { state: "working", message: "m-gen", conversation: "c-gen" };
await React.act(async () => { await resources.masterState.refresh(); });
await settle();
assert(bar() === null && !stopButton() && typingBubble() !== null, "a read-only pane observes its working turn through the inline bubble, without offering a mutating Stop");
turn = { state: "working", message: "m-elsewhere", conversation: "c-camp" };
queue = [{ message: "m-gen", conversation: "c-gen" }];
await React.act(async () => { await resources.masterState.refresh(); });
await settle();
assert(bar()?.dataset.state === "queued" && !barButton("Cancel"), "a read-only pane observes its queue without offering a mutating Cancel");
assert(barButton("View"), "read-only mode retains non-mutating predecessor navigation");
queue = [];

// Context navigation cannot manufacture a successful network reconnect.
const Conversation = (require("../src/features/app-shell/chat/Conversation") as typeof ConversationModule).default;
const renderPane = async (contextId: string) => {
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(Conversation, {
      mode: { kind: "app", installId: "install-crm", contextId, screen: null, recordOpen: false,
        contextName: null, scopeLabel: "CRM", descriptor: null, conversationRequest: null,
        contextProof: { status: "ready", contextId, error: null }, appPath: null,
        fileUpload: { declared: false, available: false }, fileUploadLoading: false },
      density: "compact", viewer: { operator: true, readOnly: false },
      binding: { scope: { install_id: "install-crm", context_id: contextId }, error: null },
      onFileUploadUnavailable: () => undefined, collapsed: false,
      onCollapsed: () => undefined, onOpenView: () => undefined,
    })));
  });
  await settle();
};
controlledTranscript = true;
turn = { state: "working", message: "m-reconnect", conversation: "c-gen" };
await renderPane("ctx-a");
await pick("General");
assert(stopButton(), "the connected General pane has its own live turn");
holdReconnect = true;
await React.act(async () => { disconnectTranscript?.(); });
await settle();
assert(host.querySelector('[data-chat-conv-label][data-stale="true"]')?.textContent?.includes("Reconnecting…") && !stopButton(), "the actual stream disconnect marks the link last known and hides turn actions");
await renderPane("ctx-b");
assert(host.querySelector('[data-chat-conv-label][data-stale="true"]') !== null && !stopButton(), "a same-conversation context change must not declare the disconnected stream live");

// A fresh snapshot is not actionable until this scope's operation read
// has reconciled. A failed read remains stale, then a real Retry recovers.
await React.act(async () => { root.render(null); });
holdReconnect = false;
heldOperations = [];
await renderPane("ctx-a");
assert(host.querySelector('[data-chat-conv-label][data-stale="true"]') !== null && !stopButton(), "a turn snapshot cannot enable Stop before scoped approval reconciliation");
// Settle the discarded pre-snapshot batch, then its fenced follow-up.
for (let batch = 0; batch < 2; batch++) {
  await React.act(async () => { heldOperations!.splice(0).forEach((resolve) => resolve(json({ error: "Unavailable" }, 503))); });
  await settle();
}
assert(host.querySelector('[data-chat-conv-label][data-stale="true"]') !== null && !stopButton(), "failed post-reconnect operation reads keep actions hidden");
operations = [{
  id: "op-reconnected", action_id: "customer.tags.update", status: "pending_permission", revision: 1,
  summary: "Review the reconnected change", resource_refs: [],
  permission_request: { reason: "This changes one customer", allow_always: true,
    scope: { install_id: "install-crm", context_id: "ctx-a", action_id: "customer.tags.update", resource_id: "ada" },
    preview: { customer_label: "Ada", before_tags: [], after_tags: ["vip"], expected_revision: 1 } },
}];
heldOperations = null;
const retryActivity = Array.from(host.querySelectorAll(".app-assistant-activity button")).find((button) => button.textContent?.trim() === "Retry");
assert(retryActivity, "the failed activity read offers its existing Retry");
await React.act(async () => { (retryActivity as HTMLButtonElement).click(); });
await settle();
assert(!bar()?.dataset.stale && barButton("Review") && !barButton("Stop") && !stopButton() && host.querySelector('[data-operation-status="pending_permission"]'), "reconciliation exposes the real approval card and Review, never a transient Stop");

// AOS-378: a failed canonical identity refresh cannot leave a stale private
// profile labelled current, nor borrow Home/the coordinator as a fallback.
hostedIdentity = true;
controlledTranscript = false;
turn = null;
operations = [];
await React.act(async () => { await resources.masterState.refresh(); });
await mount("install-crm");
await pick("General");
assert(host.querySelector("[data-assistant-name]")?.textContent === "Poppy", "the selected hosted conversation names its own assistant");
assert(host.querySelector("[data-identity-state]")?.getAttribute("data-identity-state") === "ready", "the fresh private profile is current");
// Home is idle while this hosted app has its own running/queued operation.
// Both display and command reconciliation must read the selected scope.
turn = { state: "working", message: "m-gen", conversation: "c-gen", summary: "customers.search" };
await React.act(async () => { await resources.masterConversation("c-gen").refresh(); });
await settle();
assert(bar() === null && stopButton(), "a hosted app shows its own working turn while Home is idle");
await React.act(async () => { stopButton()!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
await settle();
equal(commandPosts.at(-1), { command: "stop", message: "m-gen" }, "hosted Stop controls the selected app message");
assert(!stopButton(), "Stop refreshes the selected hosted conversation after the operation ends");
turn = { state: "working", message: "m-other", conversation: "c-camp" };
queue = [{ message: "m-gen", conversation: "c-gen" }];
await React.act(async () => { await resources.masterConversation("c-gen").refresh(); });
await settle();
assert(bar()?.dataset.state === "queued" && barButton("Cancel") && !barButton("Stop") && !stopButton(), "a hosted app shows its own queued turn without borrowing idle Home");
await React.act(async () => { barButton("Cancel")!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
await settle();
equal(commandPosts.at(-1), { command: "cancel", message: "m-gen" }, "hosted Cancel controls the selected queued app message");
assert(!barButton("Cancel"), "Cancel refreshes the selected hosted conversation after queue removal");
identityUnavailable = true;
await React.act(async () => { await resources.masterConversation("c-gen").refresh(); });
await settle();
assert(host.querySelector("[data-identity-state]")?.getAttribute("data-identity-state") === "unavailable", "a 503 makes the retained private profile unavailable, not current");
assert(!host.querySelector("[data-assistant-identity]"), "an unreadable profile does not claim private/workspace ownership from stale data");
assert(host.querySelector("[data-assistant-name]")?.textContent !== "Poppy", "a failed identity read never displays the stale committed name as current");

// CAD-1993: the pane's send keeps one identity from the working placeholder
// through the first step, and a turn split by a system row keeps distinct
// segment keys — one head and one typing row, never two.
convs["install-crm"].push(conv("c-split", { title: "Split turn" }));
threads["c-split"] = [ent(1, "operator", "message", "split ask", "m-split")];
await React.act(async () => { await conversationList("install-crm").refresh(); });
await settle();
await pick("Split turn");
identityUnavailable = false;
turn = { state: "working", message: "m-split", conversation: "c-split" };
await React.act(async () => {
  await conversationThread("c-split").refresh();
  await resources.masterConversation("c-split").refresh();
});
await settle();
const placeholder = host.querySelector(".app-chat-list [data-chat-typing]") as HTMLElement | null;
assert(placeholder !== null, "a working turn with no stored step paints the send placeholder");
// A queued message after the working send must not take over the
// placeholder's key: the first step still has to update this row.
threads["c-split"].push(ent(2, "operator", "message", "queued behind", "m-next"));
queue = [{ message: "m-next", conversation: "c-split" }];
await React.act(async () => {
  await conversationThread("c-split").refresh();
  await resources.masterConversation("c-split").refresh();
});
await settle();
assert(placeholder.isConnected, "a newer queued send leaves the working placeholder mounted");
threads["c-split"].push(ent(3, "agent", "thinking", "plan the split", null));
await React.act(async () => { await conversationThread("c-split").refresh(); });
await settle();
const landed = host.querySelector(".app-chat-list [data-chat-run]") as HTMLElement | null;
assert(landed !== null, "the first step paints the turn row");
assert(landed === placeholder, "the head updates in place when the first step lands");
// Completion must not rename the run to the queued send either.
threads["c-split"].push(ent(4, "agent", "turn_result", "Split answer.", null));
turn = null;
await React.act(async () => {
  await conversationThread("c-split").refresh();
  await resources.masterConversation("c-split").refresh();
});
await settle();
assert(host.querySelector(".app-chat-list [data-chat-run]") === landed, "the head survives completion past a queued send");
// The daemon's working id differs from the send id and a system row
// splits the untagged run: ownership follows the adopted send, so the
// last segment paints the dots and no second head appears.
threads["c-split"] = [
  ent(1, "operator", "message", "split ask", "m-split"),
  ent(2, "agent", "thinking", "plan the split", null),
  ent(3, "system", "message", "system note", null),
  ent(4, "agent", "tool_call", "Bash: cadence board read", null),
];
turn = { state: "working", message: "server-split", conversation: "c-split" };
await React.act(async () => {
  await conversationThread("c-split").refresh();
  await resources.masterConversation("c-split").refresh();
});
await settle();
equal(host.querySelectorAll(".app-chat-list [data-chat-run] .turn-head").length, 1, "a split turn paints one head");
equal(host.querySelectorAll('.app-chat-list [data-chat-run] [data-kind="typing"]').length, 1, "a split turn paints one typing row");
equal(host.querySelectorAll(".app-chat-list [data-chat-typing]").length, 0, "no standalone typing head follows a split turn");
assert(host.querySelector(".app-chat-list [data-chat-run]") === landed, "the split keeps the head's identity");
// A system row between the working send and a queued message is transparent:
// the real run is still adopted, so there is one head and one typing row.
threads["c-split"] = [
  ent(1, "operator", "message", "split ask", "m-split"),
  ent(2, "system", "message", "system note", null),
  ent(3, "operator", "message", "queued behind", "m-next"),
  ent(4, "agent", "thinking", "plan the split", null),
];
turn = { state: "working", message: "m-split", conversation: "c-split" };
await React.act(async () => {
  await conversationThread("c-split").refresh();
  await resources.masterConversation("c-split").refresh();
});
await settle();
equal(host.querySelectorAll(".app-chat-list [data-chat-run] .turn-head").length, 1, "a system-only gap paints one head");
equal(host.querySelectorAll('.app-chat-list [data-chat-run] [data-kind="typing"]').length, 1, "a system-only gap paints one typing row");
equal(host.querySelectorAll(".app-chat-list [data-chat-typing]").length, 0, "a system-only gap appends no standalone typing head");

await React.act(async () => { root.unmount(); });
console.log("conversations checks passed");
}
void main();
