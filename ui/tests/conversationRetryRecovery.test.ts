import { streamFixture } from "./streamFixture";
import type * as HappyDomModule from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDomModule from "react-dom/client";
import type * as LocaleModule from "../src/lib/locale";
import type * as ConversationModule from "../src/features/app-shell/chat/Conversation";
import type * as ConversationClientModule from "../src/features/app-shell/conversationClient";
/**
 * Create-on-first-send must stay recoverable when creating the conversation
 * fails again on Retry. The failed envelope keeps the original brief, Retry
 * and Discard stay available, and a later unsent composer draft is not what
 * the recovered send transmits.
 */
declare function require(name: string): unknown;
const { Window } = require("happy-dom") as typeof HappyDomModule;
const win = new Window({ url: "http://localhost/app-installations/install-crm?ctx=ctx-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "HTMLButtonElement", "HTMLFormElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"] as const)
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module") as { prototype: { require: (this: unknown, id: string) => unknown } };
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
const Conversation = (require("../src/features/app-shell/chat/Conversation") as typeof ConversationModule).default;
const { autoSelectCampaignConversation, outboxFor } = require("../src/features/app-shell/conversationClient") as typeof ConversationClientModule;
const process = require("process") as { exitCode?: number };
const originalFetch = globalThis.fetch;

const INSTALL = "install-crm";
const CONTEXT = "ctx-a";
const CAMPAIGN = "cmp-fresh";
const SUBJECT = `campaign:${CAMPAIGN}`;
const ORIGIN = `app|${INSTALL}|${SUBJECT}`;
const CREATED = "conv-recovered";
const BRIEF = "synthetic-brief-cad-1452";
const LATER = "later-unsent-draft-cad-1452";
const CREATE_ERROR = "The conversation store refused this create";

const assert: (condition: unknown, message: string) => asserts condition = (condition, message) => {
  if (!condition) throw new Error(message);
};

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

async function paint(): Promise<void> {
  await React.act(async () => {
    await eventTurn();
    await eventTurn();
    await eventTurn();
  });
}

function json(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json" } });
}

function jsonObject(value: unknown, why: string): object {
  let parsed = value;
  if (typeof value === "string") {
    try {
      parsed = JSON.parse(value);
    } catch {
      throw new Error(`${why}: body is not JSON`);
    }
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error(`${why}: expected a JSON object`);
  return parsed;
}

function field(record: object, key: string): unknown {
  if (!Object.prototype.hasOwnProperty.call(record, key)) return undefined;
  return Reflect.get(record, key);
}

interface ListedConversation {
  id: string;
  title: string | null;
  subject: string | null;
  is_general: boolean;
}

const conversations: ListedConversation[] = [
  { id: "c-gen", title: null, subject: null, is_general: true },
];
const forbiddenMessages: string[] = [];
const sentMessages: string[] = [];
let messagesAllowed = false;

interface CreateHold {
  started: Promise<void>;
  body: () => string;
  fail: (message: string) => void;
  succeed: () => void;
}

interface PendingCreate {
  promise: Promise<Response>;
  take: (body: string) => void;
}

let pendingCreate: PendingCreate | null = null;

function holdCreate(): CreateHold {
  let recorded = "";
  let resolveStarted: (() => void) | null = null;
  const started = new Promise<void>((resolve) => {
    resolveStarted = resolve;
  });
  let finish: ((response: Response) => void) | null = null;
  const promise = new Promise<Response>((resolve) => {
    finish = resolve;
  });
  let settled = false;
  const settle = (response: Response) => {
    if (settled) return;
    settled = true;
    finish?.(response);
  };
  pendingCreate = {
    promise,
    take(body: string) {
      recorded = body;
      pendingCreate = null;
      resolveStarted?.();
    },
  };
  return {
    started,
    body: () => recorded,
    fail(message: string) {
      settle(json({ error: message }, 503));
    },
    succeed() {
      if (!conversations.some((row) => row.id === CREATED)) {
        conversations.push({ id: CREATED, title: null, subject: SUBJECT, is_general: false });
      }
      messagesAllowed = true;
      settle(json({
        conversation: { id: CREATED, title: null, subject: SUBJECT, is_general: false },
        created: true,
      }));
    },
  };
}

function assertCreateBody(raw: string, why: string): void {
  const record = jsonObject(raw, why);
  assert(field(record, "context_id") === CONTEXT, `${why}: context_id`);
  assert(field(record, "subject") === SUBJECT, `${why}: subject`);
}


globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = new URL(typeof input === "string" || input instanceof URL ? input : input.url, "http://localhost");
  const method = (init?.method ?? "GET").toUpperCase();
  const path = url.pathname;
  if (method === "GET" && path === `/api/app-installations/${INSTALL}/conversations`) {
    return json({ alias: "master", install_id: INSTALL, conversations });
  }
  if (method === "POST" && path === `/api/app-installations/${INSTALL}/conversations`) {
    const hold = pendingCreate;
    assert(hold, "a create POST needs a held outcome");
    const body = typeof init?.body === "string" ? init.body : "";
    hold.take(body);
    return hold.promise;
  }
  if (method === "POST" && path === "/api/threads/master/messages") {
    const body = typeof init?.body === "string" ? init.body : "";
    if (!messagesAllowed) {
      forbiddenMessages.push(body);
      return json({ error: "message before conversation create" }, 409);
    }
    sentMessages.push(body);
    return json({ message: "queued", state: "queued", duplicate: false });
  }
  if (method === "GET" && path === "/api/threads/master") {
    return json({ entries: [], more_before: false, thread: null });
  }
  if (method === "GET" && path === "/api/threads/master/stream") return streamFixture(path, { alias: "master", provider: "pi", endpoint_kind: "pi", live: false, turn: null }, init?.signal);
  if (method === "GET" && path === "/api/master/state") {
    return json({ alias: "master", provider: "pi", endpoint_kind: "pi", live: false, turn: null });
  }
  if (method === "GET" && path === `/api/app-installations/${INSTALL}/assistant/actions`) {
    return json({ contract: "app-assistant/v1", actions: [], descriptor_digest: "recovery-fixture" });
  }
  if (method === "GET" && path === `/api/app-installations/${INSTALL}/assistant/operations`) return json({ operations: [] });
  if (method === "GET" && path === `/api/app-installations/${INSTALL}/assistant/permissions`) return json({ permissions: [] });
  throw new Error(`unexpected ${method} ${path}`);
}) as typeof fetch;

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);

function composerBox(): HTMLTextAreaElement {
  const box = host.querySelector("#app-shell-chat-box");
  assert(box instanceof HTMLTextAreaElement, "the real composer is mounted");
  assert(box.disabled === false, "the composer can still be edited");
  return box;
}

async function setComposerText(text: string): Promise<void> {
  await React.act(async () => {
    const box = composerBox();
    const set = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    assert(set, "the textarea value setter is available");
    set.call(box, text);
    box.dispatchEvent(new Event("input", { bubbles: true }));
    await eventTurn();
  });
}

function outboxAction(label: "Retry" | "Discard"): HTMLButtonElement {
  const panel = host.querySelector("[data-chat-outbox]");
  assert(panel, "the failed create keeps Retry and Discard on screen");
  const found = Array.from(panel.querySelectorAll("button")).find((button) => button.textContent?.trim() === label);
  assert(found instanceof HTMLButtonElement, `${label} is shown`);
  assert(found.disabled === false, `${label} stays enabled`);
  return found;
}

function failedEnvelope() {
  const held = outboxFor(ORIGIN);
  assert(held !== null, "the submitted envelope is still held");
  assert(held.state === "failed", "the envelope is failed rather than stuck sending");
  assert(held.text === BRIEF, "the failed envelope keeps the synthetic brief");
  assert(host.querySelector("[data-chat-outbox]")?.textContent?.includes(BRIEF) === true, "the synthetic brief stays visible");
  assert(host.querySelector("[data-chat-status='sending']") === null, "a refused create is not shown as still sending");
  return held;
}

async function main(): Promise<void> {
  try {
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(Conversation, {
        mode: {
          kind: "app",
          installId: INSTALL,
          contextId: CONTEXT,
          screen: null,
          recordOpen: false,
          contextName: null,
          scopeLabel: "Acme",
          descriptor: null,
          conversationRequest: null,
          contextProof: { status: "ready", contextId: CONTEXT, error: null },
          appPath: null,
          fileUpload: { declared: false, available: false },
          fileUploadLoading: false,
        },
        density: "compact",
        viewer: { operator: true, readOnly: false },
        binding: { scope: { install_id: INSTALL, context_id: CONTEXT }, error: null },
        onFileUploadUnavailable: () => undefined,
        collapsed: false,
        onCollapsed: () => undefined,
        onOpenView: () => undefined,
      })));
      await eventTurn();
      await eventTurn();
      await eventTurn();
    });
    await React.act(async () => {
      await autoSelectCampaignConversation(INSTALL, CAMPAIGN);
      await eventTurn();
      await eventTurn();
    });
    assert(
      host.querySelector("[data-chat-conv-label]")?.textContent?.includes("New conversation (unsaved)") === true,
      "the first send is aimed at an unsaved campaign conversation",
    );

    const first = holdCreate();
    await setComposerText(BRIEF);
    await React.act(async () => {
      composerBox().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
      await eventTurn();
      await eventTurn();
    });
    await first.started;
    assertCreateBody(first.body(), "the first create");
    assert(forbiddenMessages.length === 0 && sentMessages.length === 0, "no message write before the conversation exists");
    await React.act(async () => {
      first.fail(CREATE_ERROR);
      await eventTurn();
      await eventTurn();
      await eventTurn();
    });
    const original = failedEnvelope();
    outboxAction("Retry");
    outboxAction("Discard");
    assert(original.error === CREATE_ERROR, "the first refusal stays on the envelope");

    await setComposerText(LATER);
    assert(composerBox().value === LATER, "a later draft can be typed beside the failed brief");

    const second = holdCreate();
    await React.act(async () => {
      outboxAction("Retry").dispatchEvent(new MouseEvent("click", { bubbles: true }));
      await eventTurn();
      await eventTurn();
    });
    await second.started;
    assertCreateBody(second.body(), "the retry create");
    assert(forbiddenMessages.length === 0 && sentMessages.length === 0, "a failed retry still does not send");
    await React.act(async () => {
      second.fail(CREATE_ERROR);
      await eventTurn();
      await eventTurn();
      await eventTurn();
    });
    const retried = failedEnvelope();
    assert(retried.message === original.message, "retry keeps the original message id");
    outboxAction("Retry");
    outboxAction("Discard");
    assert(composerBox().value === LATER, "the later draft survives the failed retry");

    const third = holdCreate();
    await React.act(async () => {
      outboxAction("Retry").dispatchEvent(new MouseEvent("click", { bubbles: true }));
      await eventTurn();
      await eventTurn();
    });
    await third.started;
    assertCreateBody(third.body(), "the recovered create");
    assert(sentMessages.length === 0, "the message waits until create succeeds");
    await React.act(async () => {
      third.succeed();
      await eventTurn();
      await eventTurn();
      await eventTurn();
    });
    await paint();

    assert(forbiddenMessages.length === 0, "no message was written before the successful create");
    const sentBody = sentMessages.at(-1);
    assert(typeof sentBody === "string", "the recovered send transmits its owned brief");
    const sent = jsonObject(sentBody, "recovered message");
    assert(field(sent, "text") === BRIEF, "the wire text is the retained synthetic brief");
    assert(field(sent, "message") === original.message, "the wire message id is the retained envelope");
    assert(field(sent, "conversation") === CREATED, "the message selects the conversation create returned");
    assert(field(sent, "subject") === undefined, "the message does not carry the campaign subject");
    const app = field(sent, "app");
    if (app === null || typeof app !== "object" || Array.isArray(app)) throw new Error("the message carries the captured app binding");
    assert(field(app, "install_id") === INSTALL, "the message install is the captured install");
    assert(field(app, "context_id") === CONTEXT, "the message context is the captured context");
    assert(sentMessages.every((body) => !body.includes(LATER)), "the later draft is not sent");
    assert(outboxFor(ORIGIN) === null, "the owned envelope is cleared once its conversation accepts it");
    assert(host.querySelector("[data-chat-outbox]") === null, "Retry and Discard leave once the owned send is accepted");
    const pending = Array.from(host.querySelectorAll("[data-pending]"));
    assert(pending.length === 1 && pending[0]?.getAttribute("data-pending") === "sent", "only the owned brief settles as sent");
    assert(pending[0]?.textContent?.includes(BRIEF) === true, "the settled row is the synthetic brief");
    assert(pending.every((node) => node.textContent?.includes(LATER) !== true), "the later draft is not settled as a message");
    assert(composerBox().value === LATER, "settlement clears only the owned brief and leaves the later draft");
    console.log("conversationRetryRecovery.test.ts: passed");
  } finally {
    await React.act(async () => {
      root.unmount();
      await eventTurn();
    });
    globalThis.fetch = originalFetch;
    loader.prototype.require = originalRequire;
    host.remove();
    await win.happyDOM.close();
  }
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
