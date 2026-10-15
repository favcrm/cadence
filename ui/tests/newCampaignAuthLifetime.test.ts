import type * as HappyDomModule from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type * as PrivateSessionModule from "../src/lib/privateSession";
import type * as LandingModule from "../src/features/app-shell/campaign/landing";
import type * as NewCampaignModule from "../src/features/app-shell/campaign/NewCampaignDialog";
import type * as SessionKeyModule from "../src/lib/sessionKey";
/**
 * New campaign keeps save, conversation open and the draft brief inside the
 * admitted Activity generation. A late save body cannot continue as the next
 * session, or after the same generation was paused and shown again.
 */
declare function require(name: string): unknown;
const { Window } = require("happy-dom") as typeof HappyDomModule;
const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLFormElement", "HTMLButtonElement", "navigator", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"] as const)
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module") as { prototype: { require: (id: string) => unknown } };
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.call(this, id);
};
const React = require("react") as typeof ReactModule;
const { createRoot } = require("react-dom/client") as typeof ReactDomModule;
const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
const NewCampaignDialog = (require("../src/features/app-shell/campaign/NewCampaignDialog") as typeof NewCampaignModule).default;
const { allowPrivateReads, pausePrivateReads, privateSessionGeneration, resetPrivateSession } =
  require("../src/lib/privateSession") as typeof PrivateSessionModule;
const { peekLanding } = require("../src/features/app-shell/campaign/landing") as typeof LandingModule;
const { setSessionKey } = require("../src/lib/sessionKey") as typeof SessionKeyModule;
const process = require("process") as { argv: string[]; exitCode: number };
const originalFetch = globalThis.fetch;

const assert: (condition: unknown, message: string) => asserts condition = (condition, message) => {
  if (!condition) throw new Error(message);
};
function equal(actual: unknown, expected: unknown, message: string): void {
  if (actual !== expected) throw new Error(`${message}: expected ${String(expected)}, got ${String(actual)}`);
}
const scope = { installId: "install-crm", contextId: "ctx-a" };
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
type Call = { method: string; path: string; body: string | null; generation: number; credential: string | null };
const calls: Call[] = [];
type Gate = { started: Promise<void>; release: () => void };
let saveGate: Gate | null = null;
let holdSave = false;
const releases: Array<() => void> = [];

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
function eventTurn(): Promise<void> {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = () => {
      channel.port1.onmessage = null;
      channel.port1.close();
      channel.port2.close();
      resolve();
    };
    channel.port2.postMessage(null);
  });
}
async function drain(): Promise<void> {
  await eventTurn();
  await eventTurn();
}
function heldJson(value: unknown): { stream: ReadableStream<Uint8Array> } & Gate {
  const started = deferred<void>();
  const opened = deferred<void>();
  let released = false;
  const release = () => {
    if (released) return;
    released = true;
    opened.resolve();
  };
  releases.push(release);
  const bytes = new TextEncoder().encode(JSON.stringify(value));
  const stream = new ReadableStream<Uint8Array>({
    async pull(controller) {
      started.resolve();
      await opened.promise;
      try {
        controller.enqueue(bytes);
        controller.close();
      } catch {
        // The reader may already have stopped; the bytes stay releasable until then.
      }
    },
  }, { highWaterMark: 0 });
  return { stream, started: started.promise, release };
}
function json(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json" } });
}
function bodyField(body: string | null, field: string): unknown {
  const value: unknown = JSON.parse(body ?? "{}");
  assert(value !== null && typeof value === "object" && !Array.isArray(value), "request body is a JSON object");
  return Reflect.get(value, field);
}
globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const path = new URL(String(input), "http://localhost").pathname;
  const method = (init?.method ?? "GET").toUpperCase();
  const body = typeof init?.body === "string" ? init.body : null;
  calls.push({ method, path, body, generation: privateSessionGeneration(), credential: new Headers(init?.headers).get("X-Cadence-Session") });
  if (path.endsWith("/segments/list")) return json({ segments: [] });
  if (method === "POST" && path.endsWith("/content/campaigns")) {
    const id = bodyField(body, "campaign_id");
    assert(typeof id === "string" && id !== "", "save request supplies its campaign id");
    const receipt = { content: { campaign_id: id } };
    if (holdSave) {
      const gate = heldJson(receipt);
      saveGate = gate;
      return new Response(gate.stream, { status: 200, headers: { "content-type": "application/json" } });
    }
    return json(receipt);
  }
  if (method === "POST" && path.endsWith("/conversations")) {
    const subject = bodyField(body, "subject");
    assert(typeof subject === "string" && subject !== "", "campaign conversation request supplies its subject");
    return json({ conversation: { id: "conv-campaign", subject, is_general: false, title: null }, created: true });
  }
  if (method === "GET" && path.endsWith("/conversations")) return json({ conversations: [{ id: "conv-campaign", subject: null, is_general: false, title: null }] });
  if (method === "POST" && path.endsWith("/messages")) return json({ message: "queued", state: "queued", duplicate: false });
  if (path === "/api/master/state") return json({ alias: "master", provider: "pi", endpoint_kind: "pi", live: false });
  if (path === "/api/locale") return json({ organization_locale: null, user_locale: null });
  throw new Error(`Unexpected ${method} ${path}`);
}) as typeof fetch;

let notify: (campaignId: string) => void = () => {};
const render = async (mode: "visible" | "hidden" = "visible") => {
  const onCreated = notify;
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(React.Activity, {
      mode,
      key: privateSessionGeneration(),
      children: React.createElement(NewCampaignDialog, { scope, initialSegmentId: null, onCreated, onCancel: () => {} }),
    })));
    await drain();
  });
};
const nameSelector = "form.crm-newc input.field";
const briefSelector = "form.crm-newc textarea.field";
function field(selector: string): HTMLInputElement | HTMLTextAreaElement {
  const input = host.querySelector<HTMLInputElement | HTMLTextAreaElement>(selector);
  assert(input, `campaign field exists: ${selector}`);
  return input;
}
async function fill(selector: string, value: string): Promise<void> {
  const input = field(selector);
  const prototype = input.tagName === "TEXTAREA" ? win.HTMLTextAreaElement.prototype : win.HTMLInputElement.prototype;
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function submit(): Promise<void> {
  const form = host.querySelector("form.crm-newc");
  assert(form?.tagName === "FORM", "the new campaign form is mounted");
  await React.act(async () => {
    (form as HTMLFormElement).requestSubmit();
    await drain();
  });
}
function posts(pathSuffix: string, since = 0): Call[] {
  return calls.slice(since).filter((call) => call.method === "POST" && call.path.endsWith(pathSuffix));
}
function lastCall(rows: Call[], why: string): Call {
  const call = rows.at(-1);
  assert(call, why);
  return call;
}
function threadText(call: Call): string {
  const text = bodyField(call.body, "text");
  assert(typeof text === "string", "thread send carries text");
  return text;
}
function campaignId(call: Call): string {
  const id = bodyField(call.body, "campaign_id");
  assert(typeof id === "string" && id !== "", "save carries the campaign id");
  return id;
}
function heldSave(): Gate {
  const gate = saveGate;
  if (!gate) throw new Error("save headers returned while its body is still pending");
  return gate;
}
async function releaseSave(): Promise<void> {
  const gate = heldSave();
  await React.act(async () => {
    gate.release();
    await drain();
  });
}
async function fresh(onCreated: (campaignId: string) => void): Promise<void> {
  notify = onCreated;
  await React.act(async () => { resetPrivateSession(); });
  setSessionKey("synthetic-session-old-cad1452");
  allowPrivateReads();
  holdSave = false;
  saveGate = null;
  await render();
  assert(field(nameSelector).value === "" && field(briefSelector).value === "", "a new generation opens an empty campaign draft");
}

const scenario = process.argv[2] ?? "all";
const selected = (name: string) => scenario === "all" || scenario === name;

async function uninterrupted(): Promise<void> {
  const created: string[] = [];
  notify = (id) => { created.push(id); };
  allowPrivateReads();
  setSessionKey("synthetic-session-live-cad1452");
  await render();
  const name = "Live launch";
  const brief = "synthetic-brief-live-cad1452";
  await fill(nameSelector, name);
  await fill(briefSelector, brief);
  const cursor = calls.length;
  await submit();
  const saves = posts("/content/campaigns", cursor);
  const conversations = posts("/conversations", cursor);
  const threads = posts("/messages", cursor);
  const save = lastCall(saves, "an admitted session saves its campaign");
  const conversation = lastCall(conversations, "an admitted session opens its campaign conversation");
  const thread = lastCall(threads, "an admitted session sends its brief");
  const id = campaignId(save);
  assert(bodyField(save.body, "name") === name, "the save stores the typed campaign name");
  assert(bodyField(conversation.body, "subject") === `campaign:${id}`, "the conversation is this campaign's subject");
  const text = threadText(thread);
  assert(text === `Draft the email for campaign ${id} (“${name}”): ${brief}`, "the real thread send contains the exact draft brief");
  const app = bodyField(thread.body, "app");
  assert(app !== null && typeof app === "object" && "install_id" in app && app.install_id === scope.installId, "the brief stays on this installation");
  assert(created.length === 1 && created[0] === id, "the dialog reports the saved campaign");
  assert(peekLanding(id)?.tab === "email", "the dialog hands off to the saved campaign");
}

async function resetWhileSavePending(): Promise<void> {
  const oldNav: string[] = [];
  const newNav: string[] = [];
  await fresh((id) => { oldNav.push(id); });
  const oldName = "Old launch";
  const oldBrief = "synthetic-brief-old-cad1452";
  await fill(nameSelector, oldName);
  await fill(briefSelector, oldBrief);
  holdSave = true;
  saveGate = null;
  await submit();
  const pending = heldSave();
  await pending.started;
  const oldSave = posts("/content/campaigns").at(-1);
  if (!oldSave) throw new Error("the held save was sent");
  const oldId = campaignId(oldSave);
  notify = (id) => { newNav.push(id); };
  const generation = privateSessionGeneration();
  await React.act(async () => { resetPrivateSession(); });
  setSessionKey("synthetic-session-new-cad1452");
  allowPrivateReads();
  assert(privateSessionGeneration() === generation + 1, "reset admits a later generation");
  await render();
  assert(field(nameSelector).value === "" && field(briefSelector).value === "", "the new generation does not show the old draft");
  const cursor = calls.length;
  await releaseSave();
  assert(posts("/conversations", cursor).length === 0 && posts("/messages", cursor).length === 0, "the old save cannot open a conversation or send after the new session is admitted");
  equal(oldNav.length, 0, "the old save cannot navigate after the new session is admitted");
  equal(newNav.length, 0, "the old save cannot invoke the replacement session's navigation callback");
  assert(peekLanding(oldId) === null, "the old save cannot leave a landing for the new session");
  assert(!calls.slice(cursor).some((call) => call.body?.includes(oldBrief)), "the old brief is absent from every new-session request");
  const freshName = "Fresh launch";
  const freshBrief = "synthetic-brief-new-cad1452";
  await fill(nameSelector, freshName);
  await fill(briefSelector, freshBrief);
  holdSave = false;
  const retryAt = calls.length;
  await submit();
  const saves = posts("/content/campaigns", retryAt);
  const threads = posts("/messages", retryAt);
  const save = lastCall(saves, "the new generation saves its own draft");
  lastCall(posts("/conversations", retryAt), "the new generation opens its own campaign conversation");
  const thread = lastCall(threads, "the new generation sends its own brief");
  const id = campaignId(save);
  assert(threadText(thread) === `Draft the email for campaign ${id} (“${freshName}”): ${freshBrief}`, "the new session sends its exact own brief");
  assert(thread.credential === "synthetic-session-new-cad1452", "the accepted new brief uses only the replacement session credential");
  assert(!calls.slice(cursor).some((call) => call.body?.includes(oldBrief)), "the finished new session still never sent the old brief");
  assert(oldNav.length === 0 && newNav.length === 1 && newNav[0] === id, "only the new draft reports creation");
  assert(peekLanding(id)?.campaignId === id && peekLanding(oldId) === null, "navigation follows the new campaign only");
}

async function pauseWhileSavePending(): Promise<void> {
  const created: string[] = [];
  await fresh((id) => { created.push(id); });
  const name = "Kept launch";
  const brief = "synthetic-brief-kept-cad1452";
  await fill(nameSelector, name);
  await fill(briefSelector, brief);
  holdSave = true;
  saveGate = null;
  await submit();
  await heldSave().started;
  const generation = privateSessionGeneration();
  await React.act(async () => { pausePrivateReads(); });
  await render("hidden");
  allowPrivateReads();
  await render("visible");
  assert(privateSessionGeneration() === generation, "pause keeps the same generation");
  assert(field(nameSelector).value === name && field(briefSelector).value === brief, "hiding and showing the same generation keeps the typed name and brief");
  const cursor = calls.length;
  await releaseSave();
  assert(posts("/conversations", cursor).length === 0 && posts("/messages", cursor).length === 0, "the paused save does not open a conversation or send when it is shown again");
  equal(created.length, 0, "the paused save does not navigate when it is shown again");
  assert(field(nameSelector).value === name && field(briefSelector).value === brief, "abandoning the paused save keeps the typed draft");
  holdSave = false;
  const retryAt = calls.length;
  await submit();
  const threads = posts("/messages", retryAt);
  const save = lastCall(posts("/content/campaigns", retryAt), "an explicit retry saves the preserved campaign");
  lastCall(posts("/conversations", retryAt), "an explicit retry opens the preserved campaign conversation");
  const thread = lastCall(threads, "an explicit retry sends the preserved brief");
  assert(threadText(thread) === `Draft the email for campaign ${campaignId(save)} (“${name}”): ${brief}`, "the retry sends the exact brief that stayed on screen");
  assert(created.length === 1, "the retry reports the campaign");
  assert(!calls.slice(cursor).some((call) => call.method === "POST" && call.path.endsWith("/messages") && threadText(call) !== threadText(thread)), "no different brief is sent for the paused attempt");
}

async function run(): Promise<void> {
  if (selected("positive")) await uninterrupted();
  if (selected("reset")) await resetWhileSavePending();
  if (selected("pause")) await pauseWhileSavePending();
  console.log(`new campaign auth lifetime ${scenario}: passed`);
}

run().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
}).finally(async () => {
  for (const release of releases) release();
  await React.act(async () => {
    await drain();
    root.unmount();
  });
  globalThis.fetch = originalFetch;
  loader.prototype.require = originalRequire;
  host.remove();
  await win.happyDOM.close();
});
