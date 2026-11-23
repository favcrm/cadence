export {};
/**
 * CAD-2649 / TASK-014: Official API parity for WhatsAppReplyComposer,
 * screenProtocol (`whatsapp-compose` provider field), and screenServices (`window_closed` 409).
 */
import type { MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { WhatsAppReplySelection } from "../src/features/workspace-apps/screen/WhatsAppReplyComposer";

declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/apps/waba" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "HTMLTextAreaElement", "HTMLButtonElement", "HTMLImageElement", "navigator",
    "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "location",
    "history", "sessionStorage", "localStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "ResizeObserver", {
    value: class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
    configurable: true,
    writable: true,
  });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const WhatsAppReplyComposer = (
    require("../src/features/workspace-apps/screen/WhatsAppReplyComposer") as typeof import("../src/features/workspace-apps/screen/WhatsAppReplyComposer")
  ).default;
  const { recordServices } = require("../src/features/workspace-apps/screen/screenServices") as typeof import("../src/features/workspace-apps/screen/screenServices");
  const { parseChild } = require("../src/features/workspace-apps/screen/screenProtocol") as typeof import("../src/features/workspace-apps/screen/screenProtocol");

  // 1. screenProtocol whatsapp-compose accepts Linked legacy, explicit linked, and explicit official IDs cleanly.
  assert(
    parseChild({ v: 2, op: "whatsapp-compose", conversation_id: "15550001111@s.whatsapp.net" })?.op === "whatsapp-compose",
    "legacy 3-key linked compose parses",
  );
  const parsedOfficial = parseChild({
    v: 2,
    op: "whatsapp-compose",
    conversation_id: "conv_official_42",
    provider: "official",
  });
  assert(
    parsedOfficial?.op === "whatsapp-compose" && parsedOfficial.provider === "official" && parsedOfficial.conversation_id === "conv_official_42",
    "official compose with opaque id parses",
  );
  assert(
    parseChild({ v: 2, op: "whatsapp-compose", conversation_id: "conv_official_42" }) === null,
    "omitted provider stays strict on Linked JID grammar",
  );
  assert(
    parseChild({ v: 2, op: "whatsapp-compose", conversation_id: "conv_1", provider: "whatsapp_web" }) === null,
    "unknown provider is rejected",
  );

  // 2. screenServices preserves 409 window_closed & stale_approval codes on whatsapp.*.
  const token = "a".repeat(64);
  const receipt: MountReceipt = {
    bridge_nonce: "n1",
    generation: 1,
    mount: "https://apps.example.test/m",
    tag: "waba",
    action_token: token,
    tools: [],
    host_contract: "screen-records.v1",
    operations: ["whatsapp.conversations.show", "whatsapp.reply.prepare", "whatsapp.reply.send", "whatsapp.reply.status"],
    navigation: null,
    display_session_key: "b".repeat(64),
    install_id: "inst_1",
    context_id: "ctx_1",
  };
  let mockFetchStatus = 200;
  let mockFetchBody: unknown = {};
  globalThis.fetch = (async () =>
    new Response(JSON.stringify(mockFetchBody), {
      status: mockFetchStatus,
      headers: { "Content-Type": "application/json" },
    })) as typeof fetch;
  const services = recordServices(receipt, JSON.stringify(["inst_1", "ctx_1"]), {
    current: () => true,
    command: () => {},
    refused: () => {},
  });
  mockFetchStatus = 409;
  mockFetchBody = { code: "window_closed", error: "The WhatsApp 24-hour window is closed." };
  const windowRefusal = await services.call("whatsapp.reply.prepare", {
    provider: "official",
    conversation_id: "conv_1",
    text: "hi",
  });
  assert(
    !windowRefusal.ok && windowRefusal.refusal.code === "window_closed",
    `screenServices preserves 409 window_closed: ${JSON.stringify(windowRefusal)}`,
  );

  const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 10)); });
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);

  // 3. Official composer open 24h window: verifies without workRef, hides attachment picker, prepares decision=approved, sends!
  {
    const calls: { op: string; args: Record<string, unknown> }[] = [];
    const availabilities: string[] = [];
    const announced: boolean[] = [];
    const mockServices = {
      call: async (op: string, args: Record<string, unknown>) => {
        calls.push({ op, args });
        if (op === "whatsapp.conversations.show") {
          return {
            ok: true as const,
            data: {
              contract: "agenticos.whatsapp-door.v1",
              conversationId: "conv_official_open",
              channelConnectionId: "cc_1",
              status: "open",
              contact: { id: "c1", name: "Alice", phone: "+85290001111", email: null },
              replyMode: "freeform",
              freeformUntil: new Date(Date.now() + 3600_000).toISOString(),
              lastInboundAt: new Date().toISOString(),
              lastOutboundAt: null,
              unreadCount: 1,
              handoffAt: null,
            },
          };
        }
        if (op === "whatsapp.reply.prepare") {
          return {
            ok: true as const,
            data: {
              effect_id: "wa_official_eff_1",
              decision: "approved",
              expires_at: new Date(Date.now() + 600_000).toISOString(),
              digest: "c".repeat(64),
              replyMode: "freeform",
              freeformUntil: new Date(Date.now() + 3600_000).toISOString(),
            },
          };
        }
        return { ok: false as const, refusal: { code: "failed", text: "unexpected" } };
      },
      planner: async (op: string, args: Record<string, unknown>) => {
        calls.push({ op: `planner:${op}`, args });
        return {
          plan: { kind: "confirm" as const },
          run: async () => ({
            ok: true as const,
            data: {
              reply: {
                state: "sent",
                error_code: null,
                favcrm_message_id: "m1",
                provider_message_id: "wamid.1",
                deduped: false,
                repeated: false,
              },
            },
          }),
        };
      },
    } as unknown as ReturnType<typeof recordServices>;

    const selection: WhatsAppReplySelection = {
      conversationId: "conv_official_open",
      provider: "official",
      receiptScope: "scope_1",
      services: mockServices,
      current: () => true,
      announceAvailability: (s) => availabilities.push(s),
      announce: (_dirty, refresh) => { if (refresh) announced.push(true); },
    };

    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(WhatsAppReplyComposer, { selection, bounds: [0, 10000] }),
        ),
      );
    });
    await flush();

    assert(host.querySelector(".wa-reply-card")?.getAttribute("data-provider") === "official", "official provider attribute present");
    assert(host.querySelector(".wa-reply-attach") === null, "Linked attachment picker hidden in official mode");
    assert(availabilities[availabilities.length - 1] === "verified", "open official window verifies");

    const textarea = host.querySelector<HTMLTextAreaElement>("textarea.app-chat-box")!;
    assert(!textarea.readOnly, "textarea enabled when official window is open");
    await React.act(async () => {
      const setter = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")?.set;
      setter?.call(textarea, "Official reply hello");
      textarea.dispatchEvent(new win.Event("input", { bubbles: true }));
    });
    const sendBtn = host.querySelector<HTMLButtonElement>("button.wa-reply-send")!;
    assert(!sendBtn.disabled, "send button enabled");
    await React.act(async () => {
      const down = new win.MouseEvent("pointerdown", { button: 0, bubbles: true });
      Object.defineProperty(down, "isTrusted", { value: true, configurable: true });
      const click = new win.MouseEvent("click", { detail: 1, bubbles: true });
      Object.defineProperty(click, "isTrusted", { value: true, configurable: true });
      sendBtn.dispatchEvent(down);
      sendBtn.dispatchEvent(click);
    });
    await flush();

    assert(calls.some((c) => c.op === "whatsapp.reply.prepare" && c.args.provider === "official"), "prepares with provider=official");
    assert(calls.some((c) => c.op === "planner:whatsapp.reply.send" && c.args.provider === "official" && c.args.effect_id === "wa_official_eff_1"), "confirms send via planner with effect_id");
    assert(host.textContent?.includes("Delivered"), "shows quiet delivered pill");
  }

  // 4. Official composer closed 24h window (`template_only`): disables composer and names 24h template requirement.
  {
    const availabilities: string[] = [];
    const mockServices = {
      call: async (op: string) => {
        if (op === "whatsapp.conversations.show") {
          return {
            ok: true as const,
            data: {
              contract: "agenticos.whatsapp-door.v1",
              conversationId: "conv_official_closed",
              channelConnectionId: "cc_1",
              status: "open",
              contact: { id: "c2", name: "Bob", phone: "+85290002222", email: null },
              replyMode: "template_only",
              freeformUntil: null,
              lastInboundAt: null,
              lastOutboundAt: null,
              unreadCount: 0,
              handoffAt: null,
            },
          };
        }
        return { ok: false as const, refusal: { code: "failed", text: "unexpected" } };
      },
    } as unknown as ReturnType<typeof recordServices>;

    const selection: WhatsAppReplySelection = {
      conversationId: "conv_official_closed",
      provider: "official",
      receiptScope: "scope_2",
      services: mockServices,
      current: () => true,
      announceAvailability: (s) => availabilities.push(s),
      announce: () => {},
    };

    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(WhatsAppReplyComposer, { selection, bounds: [0, 10000] }),
        ),
      );
    });
    await flush();

    assert(host.querySelector('.wa-reply-card')?.getAttribute("data-window-closed") === "true", "window closed marker rendered");
    assert(availabilities[availabilities.length - 1] === "blocked", "closed window announces blocked");
    assert(host.querySelector<HTMLTextAreaElement>("textarea.app-chat-box")?.readOnly === true, "textarea disabled in closed window");
    assert(host.querySelector<HTMLButtonElement>("button.wa-reply-send")?.disabled === true, "send disabled in closed window");
    assert(host.textContent?.includes("24-hour free-form reply window is closed"), "closed window explains template requirement");
  }

  await React.act(async () => { root.unmount(); });
  host.remove();
  console.log("whatsapp official composer checks passed");
}

void main();
