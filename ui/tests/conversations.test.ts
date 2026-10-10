/** CAD-1098 S3: per-app assistant conversations in the shell chat. */
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
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
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
const AppShell = require("../src/features/app-shell/AppShell").default;
const { resources } = require("../src/lib/resources") as typeof import("../src/lib/resources");
const { parseSlash, autoSelectCampaignConversation, conversationThread } = require("../src/features/app-shell/conversationClient") as typeof import("../src/features/app-shell/conversationClient");

equal([parseSlash("/new"), parseSlash(" /CLEAR "), parseSlash("/new now"), parseSlash("hello")], ["new", "new", null, null], "only the bare commands are commands");

const inst = (id: string, name: string) => ({ install_id: id, title: name, name, version: "0.1.0", digest: "d", catalog_generation: "g", approved: true, storage_kind: "workspace", files: [], capabilities: null, connection_slots: [] });
const ctxs = (id: string) => ({ contexts: [{ id: "ctx-a", install_id: id, revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } }] });
const ent = (seq: number, role: string, kind: string, text: string, message: string) => ({ seq, role, kind, message, text, created: "2026-10-03T00:00:00Z", payload: null });
const conv = (id: string, over: object = {}) => ({ id, title: null, subject: null, is_general: false, ...over });
const convs: Record<string, any[]> = {
  "install-crm": [conv("c-gen", { is_general: true }), conv("c-camp", { subject: "campaign:cmp-1", title: "Spring launch" })],
  "install-other": [conv("o-gen", { is_general: true })],
  "install-social": [conv("s-gen", { is_general: true }), conv("s-full", { title: "Has messages" })],
};
const threads: Record<string, any[]> = {
  "c-gen": [ent(1, "operator", "message", "general question", "m-gen"), ent(2, "agent", "turn_result", "general answer", "m-gen")],
  "c-camp": [ent(1, "operator", "message", "campaign brief", "m-camp")],
  "o-gen": [],
  "s-gen": [],
  "s-full": [ent(1, "operator", "message", "earlier question", "m-s")],
};
let turn: any = null;
// Set inside a check to hold one send in flight (pending state "sending").
let deferSend: ((resolve: (r: Response) => void) => void) | null = null;
const messagePosts: any[] = [];
const createPosts: any[] = [];
let created = 0;
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  const method = init?.method ?? "GET";
  // The CRM package's own descriptor, served pinned to the install's digest.
  if (url.pathname === "/api/app-installations/install-crm/chat-descriptor") {
    return json({ descriptor: JSON.parse(require("fs").readFileSync(require("path").join(process.cwd(), "..", "workspace-apps", "crm", "app-chat.json"), "utf8")), digest: "d", app: "crm" });
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
    return json({ entries: threads[id] ?? [], more_before: false });
  }
  if (url.pathname === "/api/master/state") return json({ alias: "master", provider: "p", endpoint_kind: "k", live: true, turn });
  if (url.pathname === "/api/crm-send/list") return json({ sends: [] });
  if (url.pathname === "/api/crm-send/origin") return json({ unsubscribe_origin: null, stored: false });
  return json({ records: [], truncated: false, next_cursor: null, segments: [], exclusions: [], suppressions: [], contents: [], proposals: [], bindings: [] });
}) as typeof fetch;

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });
const settle = async () => { for (let i = 0; i < 8; i++) await flush(); };
async function mount(installId: string, viewer = { operator: true, readOnly: false }) {
  history.pushState(null, "", `/app-installations/${installId}?ctx=ctx-a`);
  await React.act(async () => { root.render(React.createElement(AppShell, { installId, viewer })); });
  await settle();
}
// CAD-1326: the select is gone; the history button lists conversations and
// the scope row names the open one (or the unsaved draft).
const histBtn = () => host.querySelector('[aria-label="Conversation history"]') as HTMLButtonElement;
const rows = () => Array.from(host.querySelectorAll("[data-chat-history] .app-chat-history-row")) as HTMLButtonElement[];
async function openHistory() {
  if (!host.querySelector("[data-chat-history]")) {
    await React.act(async () => { histBtn().dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  }
}
const optionLabels = async () => { await openHistory(); return rows().map((r) => r.textContent); };
const openLabel = () => host.querySelector("[data-chat-conv-label]")?.textContent?.replace(/^·\s*/, "") ?? null;
const pane = () => (host.querySelector("[data-chat-pane]") as HTMLElement).textContent ?? "";
const box = () => host.querySelector("#app-shell-chat-box") as HTMLTextAreaElement;
const newButton = () => host.querySelector("[data-chat-new]") as HTMLButtonElement;
async function pick(label: string) {
  await openHistory();
  const row = rows().find((r) => r.textContent === label);
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
equal(rows()[0].getAttribute("aria-current"), "true", "history marks General as the open one");
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
assert(openLabel() !== "General" && openLabel() !== "Spring launch", "the fresh conversation is the open one");
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

// Queued notice: the master is on another conversation's message while this one waits.
await pick("Spring launch");
turn = { state: "working", message: "m-gen-elsewhere" };
await React.act(async () => { await resources.masterState.refresh(); });
await settle();
assert(pane().includes("Assistant is finishing another task — your message is queued"), "queued notice shows while the master is busy elsewhere");
turn = { state: "working", message: "m-camp" };
await React.act(async () => { await resources.masterState.refresh(); });
await settle();
assert(!pane().includes("your message is queued"), "no notice when the running message is this conversation's own");
turn = null;
await React.act(async () => { await resources.masterState.refresh(); });
await settle();

// CAD-1168 follow-up: the selected conversation's own in-flight turn reads as
// a compact status — Sending… while its POST is in flight, Queued… when the
// daemon reports this conversation's message as the queue head. Neither bleeds
// into another conversation, and only the owned working turn offers Stop.
await pick("Spring launch");
{
  let resolveSend: ((r: Response) => void) | null = null;
  deferSend = (r) => { resolveSend = r; };
  await type("queued brief update");
  assert(resolveSend !== null, "the send is still in flight");
  assert(host.querySelector('[data-chat-status="sending"]') !== null, "an in-flight send shows Sending…");
  await React.act(async () => { resolveSend!(json({ message: "x", state: "queued", duplicate: false })); });
  await settle();
  deferSend = null;
  assert(host.querySelector('[data-chat-status="waiting"]') !== null, "send acknowledgement keeps a status until the daemon turn arrives");

  const sentMessage = messagePosts[messagePosts.length - 1].message;
  turn = { state: "queued", message: sentMessage };
  await React.act(async () => { await resources.masterState.refresh(); });
  await settle();
  assert(host.querySelector('[data-chat-status="queued"]') !== null, "an owned queued turn shows Queued…");
  assert(host.querySelector("[data-chat-stop]") === null, "a queued turn offers no Stop");

  // Once SSE/page reconciliation drops the pending row, the owned queue head
  // must keep polling: thread entries do not refresh the master projection.
  threads["c-camp"].push(ent(3, "operator", "message", "queued brief update", sentMessage));
  await React.act(async () => { await conversationThread("c-camp").refresh(); });
  await settle();
  turn = { state: "working", message: sentMessage };
  await React.act(async () => { await new Promise((r) => setTimeout(r, 6_100)); });
  await settle();
  assert(host.querySelector("[data-chat-stop]") !== null, "the owned working turn still offers Stop");
  assert(pane().includes("Working…"), "the working row still shows");

  await pick("General");
  assert(host.querySelector("[data-chat-status]") === null, "the status does not bleed into another conversation");
  turn = null;
  await React.act(async () => { await resources.masterState.refresh(); });
  await settle();
  await pick("Spring launch");
}

// Collapse state is per app.
await React.act(async () => { (host.querySelector(".app-chat-collapse") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
assert(host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed"), "CRM chat collapses");
await mount("install-other");
assert(!host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed"), "another app's chat is not collapsed");
equal(await optionLabels(), ["General"], "another app lists only its own conversations");
await mount("install-crm");
assert(host.querySelector(".app-shell-grid")!.hasAttribute("data-chat-collapsed"), "the CRM chat stays collapsed");

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
await React.act(async () => { root.unmount(); });
console.log("conversations checks passed");
}
void main();
