/** CAD-2811 frontend checks: member attachments, member model/effort picker, and unified AssistantComposer. */
declare function require<T = unknown>(name: string): T;

import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as AssistantComposerModule from "../src/features/app-shell/chat/AssistantComposer";
import type * as AssistantModelStateModule from "../src/features/app-shell/chat/assistantModelState";
import type * as MemberChatModule from "../src/features/home/MemberChat";
import type * as MemberThreadModule from "../src/features/home/memberThread";
import type * as LocaleModule from "../src/lib/locale";
import type * as PrivateSessionModule from "../src/lib/privateSession";
import type { MasterModels, MasterState } from "../src/lib/types";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main(): Promise<void> {
  const { Window } = require<{ Window: new (options: { url: string }) => Record<string, any> }>("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  const pointerCtor = win.PointerEvent ?? win.MouseEvent;
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLButtonElement",
    "HTMLDialogElement",
    "HTMLInputElement",
    "HTMLTextAreaElement",
    "HTMLSelectElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "sessionStorage",
    "localStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "PointerEvent", { value: pointerCtor, configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    value: (cb: (time: number) => void) => setTimeout(() => cb(Date.now()), 0),
    configurable: true,
  });
  Object.defineProperty(globalThis, "cancelAnimationFrame", {
    value: (id: ReturnType<typeof setTimeout>) => clearTimeout(id),
    configurable: true,
  });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });

  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };

  let masterStatePayload: Record<string, unknown> = {
    alias: "master",
    provider: "cadence",
    endpoint_kind: "durable_object",
    live: true,
    model: "thinkingmachines/inkling-2-mini",
    model_label: "Inkling 2 Mini",
    effort: "low",
    commands: ["stop", "model", "effort"],
    session: { id: "tc_member_home_1" },
    turn: null,
    queue: [],
  };
  let masterStateStatus = 200;
  let sendStatus = 200;
  let conversationId = "tc_member_home_1";
  let historyEntries: Array<Record<string, unknown>> = [];
  let holdMasterState: (() => void) | null = null;
  let masterStateGate: Promise<void> | null = null;
  const streamControllers: Array<ReadableStreamDefaultController<Uint8Array>> = [];
  const requests: Array<{ method: string; url: string; body?: unknown }> = [];

  const serverModelsPayload: MasterModels = {
    current: { model: "thinkingmachines/inkling-2-mini", effort: "low" },
    models: [
      {
        id: "deepseek/deepseek-v4.1-flash-1017",
        label: "DeepSeek V4.1 Flash",
        is_default: true,
        efforts: [],
        default_effort: null,
      },
      {
        id: "thinkingmachines/inkling-2-mini",
        label: "Inkling 2 Mini",
        is_default: false,
        efforts: ["low", "medium", "high"],
        default_effort: "low",
      },
    ],
    efforts: ["low", "medium", "high"],
  };

  Object.defineProperty(globalThis, "fetch", {
    value: async (input: string | URL, init?: { method?: string; body?: string }) => {
      const rawUrl = typeof input === "string" ? input : input.toString();
      const url = new URL(rawUrl, "http://localhost");
      const method = init?.method ?? "GET";
      const parsedBody = init?.body ? (JSON.parse(init.body) as unknown) : undefined;
      requests.push({ method, url: `${url.pathname}${url.search}`, body: parsedBody });

      if (url.pathname === "/api/member-assistant/state") {
        return new Response(
          JSON.stringify({
            conversation_id: conversationId,
            assistant: {
              org: "org_1",
              sub: "user_member",
              assistant_id: "asst_1",
              alias: "assistant",
              conversation_id: conversationId,
              state: "active",
              bootstrap_state: "complete",
              identity: { name: "Nova", tone: null, look: "robot", revision: 1 },
            },
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      if (url.pathname === "/api/member-assistant/history") {
        return new Response(
          JSON.stringify({
            conversation: { id: conversationId, alias: "assistant" },
            entries: historyEntries,
            cursor: historyEntries.length > 0 ? "csc1_cur1" : null,
            before_cursor: historyEntries.length > 0 ? "csc1_bef1" : null,
            more_before: false,
            more_after: false,
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      if (url.pathname === "/api/member-assistant/send") {
        if (sendStatus !== 200) {
          return new Response(JSON.stringify({ error: { code: "bad_request", message: "refused" } }), {
            status: sendStatus,
            headers: { "content-type": "application/json" },
          });
        }
        const payload = parsedBody as { message: string };
        return new Response(JSON.stringify({ message: payload.message, state: "queued" }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.pathname === "/api/master/state") {
        if (masterStateGate) await masterStateGate;
        return new Response(JSON.stringify(masterStatePayload), {
          status: masterStateStatus,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.pathname === "/api/master/models") {
        return new Response(JSON.stringify(serverModelsPayload), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.pathname === "/api/master/command") {
        const payload = parsedBody as { command: string; arg?: string };
        if (payload.command === "model") {
          masterStatePayload = { ...masterStatePayload, model: payload.arg, model_label: payload.arg };
        } else if (payload.command === "effort") {
          masterStatePayload = { ...masterStatePayload, effort: payload.arg };
        }
        return new Response(JSON.stringify({ ok: true, result: "Updated." }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.pathname === "/api/agents") {
        return new Response(
          JSON.stringify({
            daemon: "reachable",
            agents: [],
            profiles: [],
            totals: null,
            by_issue: {},
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      if (url.pathname === "/api/team/members") {
        return new Response(JSON.stringify({ members: [] }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.pathname === "/api/threads/master/stream") {
        const body = new ReadableStream<Uint8Array>({
          start(controller) {
            streamControllers.push(controller);
            controller.enqueue(new TextEncoder().encode("event: open\ndata: {}\n\n"));
          },
        });
        return new Response(body, { headers: { "content-type": "text/event-stream" } });
      }
      return new Response(JSON.stringify({}), { status: 404, headers: { "content-type": "application/json" } });
    },
    configurable: true,
    writable: true,
  });

  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const { LocaleProvider } = require<typeof LocaleModule>("../src/lib/locale");
  const { allowPrivateReads, resetPrivateSession } = require<typeof PrivateSessionModule>(
    "../src/lib/privateSession",
  );
  const MemberChat = require<typeof MemberChatModule>("../src/features/home/MemberChat").default;
  const AssistantComposer = require<typeof AssistantComposerModule>(
    "../src/features/app-shell/chat/AssistantComposer",
  ).default;
  const { isBareMasterStreamSnapshot, mergeMasterStreamSnapshot } = require<typeof AssistantModelStateModule>(
    "../src/features/app-shell/chat/assistantModelState",
  );
  const { memberThreadStore, sendToAssistant } = require<typeof MemberThreadModule>(
    "../src/features/home/memberThread",
  );

  allowPrivateReads();

  // 0. Pure stream-merge contract checks (CAD-2817).
  const canonical: MasterState = {
    alias: "master",
    provider: "cadence",
    endpoint_kind: "durable_object",
    live: true,
    identity: { name: "Nova", tone: null, look: "robot", revision: "1" },
    model: "thinkingmachines/inkling-2-mini",
    model_label: "Inkling 2 Mini",
    effort: "low",
    commands: ["stop", "model", "effort"],
    session: { id: "tc_member_home_1" },
    turn: null,
    queue: [],
  };
  const bareSse: MasterState = {
    alias: "master",
    provider: "cadence",
    endpoint_kind: "durable_object",
    live: true,
    identity: { name: "Nova", tone: null, look: "robot", revision: "1" },
    model: "thinkingmachines/inkling-2-mini",
    model_label: "thinkingmachines/inkling-2-mini",
    effort: null,
    commands: ["new", "stop"],
    session: { id: "tc_member_home_1" },
    turn: { state: "working", message: "m-1", conversation: "tc_member_home_1", position: null },
    queue: [],
  };
  assert(isBareMasterStreamSnapshot(bareSse), "bare DO SSE frame detected via missing model/effort commands");
  assert(!isBareMasterStreamSnapshot(canonical), "canonical frame with model/effort is not bare");
  const mergedBare = mergeMasterStreamSnapshot(canonical, bareSse);
  assert(
    JSON.stringify(mergedBare.commands) === JSON.stringify(["stop", "model", "effort"]) &&
      mergedBare.effort === "low" &&
      mergedBare.model_label === "Inkling 2 Mini" &&
      mergedBare.turn?.message === "m-1",
    "mergeMasterStreamSnapshot preserves commands, effort, and label on bare DO frame",
  );
  const enrichedClear = mergeMasterStreamSnapshot(canonical, {
    ...canonical,
    model: "deepseek/deepseek-v4.1-flash-1017",
    model_label: "deepseek/deepseek-v4.1-flash-1017",
    effort: null,
    commands: ["stop", "model", "effort"],
  });
  assert(
    enrichedClear.effort === null && enrichedClear.model === "deepseek/deepseek-v4.1-flash-1017",
    "mergeMasterStreamSnapshot applies authoritative reset from enriched SSE frame",
  );

  // 1. Test `sendToAssistant` sends optional attachments and preserves them on pending row.
  const scope = "sess_1:user_member";
  const chfId = "chf-0123456789abcdef0123456789abcdef";
  sendStatus = 400;
  const failedReceipt = await sendToAssistant(scope, "Read report", "msg-att-retry", undefined, [
    { id: chfId, name: "report.csv", size: 120, mime: "text/csv" },
  ]);
  assert(!failedReceipt.ok, "400 settles failure cleanly");
  const pendingRow = memberThreadStore(scope).get().data?.pending.find((p) => p.message === "msg-att-retry");
  assert(
    pendingRow?.attachments?.length === 1 && pendingRow.attachments[0]?.id === chfId,
    "pending row stores attachments for retry",
  );
  sendStatus = 200;
  const okReceipt = await sendToAssistant(scope, "Read report", "msg-att-retry", undefined, pendingRow?.attachments);
  assert(okReceipt.ok, "retry with stored pending attachments succeeds");
  const sentReq = requests.filter((r) => r.url === "/api/member-assistant/send").at(-1);
  assert(
    JSON.stringify(sentReq?.body) ===
      JSON.stringify({ text: "Read report", message: "msg-att-retry", attachments: [{ id: chfId }] }),
    "member-assistant/send payload carries attachments",
  );

  // 2. Mount MemberChat and verify attach + model select + effort pill render & work scoped to tc_member_home_1.
  const host: Element = win.document.createElement("div");
  win.document.body.appendChild(host);
  const root = createRoot(host);
  const flush = async () => {
    await React.act(async () => {
      await new Promise((r) => setTimeout(r, 25));
    });
  };

  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(MemberChat, {
          scope,
          readOnly: false,
          onOpenIssue: () => undefined,
        }),
      ),
    );
  });
  await flush();
  await flush();

  const attachBtn = host.querySelector(".app-chat-attach") as HTMLButtonElement | null;
  const modelBtn = host.querySelector("[data-composer-model]") as HTMLButtonElement | null;
  const effortBtn = host.querySelector("[data-composer-effort]") as HTMLButtonElement | null;
  assert(attachBtn !== null && !attachBtn.disabled, "MemberChat enables file attach button");
  assert(modelBtn !== null && !modelBtn.disabled, "MemberChat renders enabled model selector pill");
  assert(effortBtn !== null && !effortBtn.disabled, "MemberChat renders enabled effort pill");
  assert(
    requests.some((r) => r.url === "/api/master/state?conversation=tc_member_home_1"),
    "MemberChat reads model capabilities scoped to its own conversation_id",
  );

  await React.act(async () => {
    effortBtn.click();
  });
  await flush();
  const mediumItem = (
    Array.from(win.document.body.querySelectorAll(".composer-pop-item")) as HTMLButtonElement[]
  ).find((el) => el.textContent?.includes("Med"));
  assert(mediumItem !== undefined, "MemberChat effort menu renders Med option");
  await React.act(async () => {
    mediumItem.click();
  });
  await flush();
  assert(
    requests.some(
      (r) =>
        r.method === "POST" &&
        r.url === "/api/master/command?conversation=tc_member_home_1" &&
        JSON.stringify(r.body) === JSON.stringify({ command: "effort", arg: "medium" }),
    ),
    "MemberChat picking effort sends conversation-scoped /api/master/command",
  );

  // 2b. Live regression: a bare DO SSE `event: master` frame MUST NOT wipe out model or effort pills.
  const memberStream = streamControllers.at(-1);
  assert(memberStream !== undefined, "MemberChat opened SSE stream");
  historyEntries = [
    { id: "csrid_live_1", role: "operator", kind: "message", text: "Working task", message: "m_live_1", created: new Date().toISOString() },
  ];
  await React.act(async () => {
    memberStream.enqueue(
      new TextEncoder().encode(
        `event: master\ndata: ${JSON.stringify({
          alias: "master",
          provider: "cadence",
          endpoint_kind: "durable_object",
          live: true,
          model: "thinkingmachines/inkling-2-mini",
          model_label: "thinkingmachines/inkling-2-mini",
          effort: null,
          commands: ["new", "stop"],
          session: { id: "tc_member_home_1" },
          turn: { state: "working", message: "m_live_1", conversation: "tc_member_home_1", position: null },
          queue: [],
        })}\n\n`,
      ),
    );
  });
  await flush();
  await flush();
  assert(
    host.querySelector("[data-composer-model]") !== null &&
      host.querySelector("[data-composer-effort]") !== null &&
      host.querySelector("[data-composer-effort]")?.textContent?.includes("Med"),
    "bare DO SSE master frame keeps model & effort pills mounted with preserved Med effort",
  );

  // 2c. Unified Member UX: Stop button works on running turn, /effort command submits, and /new is refused locally.
  const stopBtn = host.querySelector('button[aria-label*="Stop"], button[title*="Stop"]') as HTMLButtonElement | null;
  assert(stopBtn !== null, "MemberChat renders Stop button during running turn");
  await React.act(async () => {
    stopBtn.click();
  });
  await flush();
  assert(
    requests.some(
      (r) =>
        r.method === "POST" &&
        r.url === "/api/master/command?conversation=tc_member_home_1" &&
        JSON.stringify(r.body) === JSON.stringify({ command: "stop", message: "m_live_1" }),
    ),
    "MemberChat Stop button sends conversation-scoped stop command",
  );

  const textarea = host.querySelector("textarea") as HTMLTextAreaElement;
  const form = host.querySelector("form[data-composer]") as HTMLFormElement;
  const setTextarea = (val: string) => {
    const setter = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")?.set;
    setter?.call(textarea, val);
    textarea.dispatchEvent(new win.Event("input", { bubbles: true }));
  };
  await React.act(async () => {
    setTextarea("/effort high");
  });
  await flush();
  await React.act(async () => {
    form.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true }));
  });
  await flush();
  assert(
    requests.some(
      (r) =>
        r.method === "POST" &&
        r.url === "/api/master/command?conversation=tc_member_home_1" &&
        JSON.stringify(r.body) === JSON.stringify({ command: "effort", arg: "high" }),
    ),
    "MemberChat slash /effort command hits conversation-scoped /api/master/command",
  );

  const cmdCountBeforeNew = requests.filter((r) => r.url.startsWith("/api/master/command")).length;
  await React.act(async () => {
    setTextarea("/new");
  });
  await flush();
  await React.act(async () => {
    form.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true }));
  });
  await flush();
  assert(
    requests.filter((r) => r.url.startsWith("/api/master/command")).length === cmdCountBeforeNew,
    "MemberChat blocks /new slash command locally without calling backend",
  );

  // 2d. Race condition: bare SSE master frame arrives BEFORE GET /api/master/state resolves.
  await React.act(async () => {
    root.unmount();
  });
  conversationId = "tc_member_race_1";
  historyEntries = [];
  masterStatePayload = {
    ...masterStatePayload,
    effort: "low",
    commands: ["stop", "model", "effort"],
    session: { id: "tc_member_race_1" },
    turn: null,
  };
  masterStateGate = new Promise<void>((resolve) => {
    holdMasterState = resolve;
  });
  const hostRace: Element = win.document.createElement("div");
  win.document.body.appendChild(hostRace);
  const rootRace = createRoot(hostRace);
  await React.act(async () => {
    rootRace.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(MemberChat, {
          scope: "sess_race:user_member",
          readOnly: false,
          onOpenIssue: () => undefined,
        }),
      ),
    );
  });
  await flush();
  const raceStream = streamControllers.at(-1);
  assert(raceStream !== undefined, "race test opened SSE stream");
  await React.act(async () => {
    raceStream.enqueue(
      new TextEncoder().encode(
        `event: master\ndata: ${JSON.stringify({
          alias: "master",
          provider: "cadence",
          endpoint_kind: "durable_object",
          live: true,
          model: "thinkingmachines/inkling-2-mini",
          model_label: "thinkingmachines/inkling-2-mini",
          effort: null,
          commands: ["new", "stop"],
          session: { id: "tc_member_race_1" },
          turn: null,
          queue: [],
        })}\n\n`,
      ),
    );
  });
  await flush();
  await React.act(async () => {
    holdMasterState?.();
    masterStateGate = null;
  });
  await flush();
  await flush();
  assert(
    hostRace.querySelector("[data-composer-model]") !== null &&
      hostRace.querySelector("[data-composer-effort]") !== null,
    "race condition resolved: model and effort pills mount once canonical /api/master/state lands",
  );
  await React.act(async () => {
    rootRace.unmount();
  });

  // 3. Graceful fallback when older server returns commands without model/effort.
  conversationId = "tc_member_legacy_1";
  masterStateStatus = 200;
  masterStatePayload = {
    ...masterStatePayload,
    commands: ["new", "stop"],
    session: { id: "tc_member_legacy_1" },
  };
  const hostLegacy: Element = win.document.createElement("div");
  win.document.body.appendChild(hostLegacy);
  const rootLegacy = createRoot(hostLegacy);
  await React.act(async () => {
    rootLegacy.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(MemberChat, {
          scope: "sess_legacy:user_member",
          readOnly: false,
          onOpenIssue: () => undefined,
        }),
      ),
    );
  });
  await flush();
  await flush();
  assert(
    hostLegacy.querySelector("[data-composer-model]") === null &&
      hostLegacy.querySelector("[data-composer-effort]") === null,
    "older commands: ['new','stop'] cleanly hides model & effort pills",
  );

  // 4. Unified AssistantComposer renders consistently in standalone compact & full capability modes.
  const hostUnified: Element = win.document.createElement("div");
  win.document.body.appendChild(hostUnified);
  const rootUnified = createRoot(hostUnified);
  await React.act(async () => {
    rootUnified.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(AssistantComposer, {
          density: "compact",
          block: null,
          onSend: () => undefined,
          attach: { enabled: true, reason: "" },
          modelPicker: {
            master: {
              alias: "master",
              provider: "cadence",
              endpoint_kind: "durable_object",
              live: true,
              identity: { name: "Assistant", tone: null, look: "robot", revision: "1" },
              model: "thinkingmachines/inkling-2-mini",
              effort: "low",
              commands: ["stop", "model", "effort"],
            },
            readOnly: false,
            canPickModel: true,
            canPickEffort: true,
            compact: true,
          },
        }),
      ),
    );
  });
  await flush();
  assert(
    hostUnified.querySelector(".app-chat-attach") !== null &&
      hostUnified.querySelector("[data-composer-model]") !== null &&
      hostUnified.querySelector("[data-composer-effort]") !== null,
    "unified AssistantComposer mounts attach, model, and effort together",
  );

  await React.act(async () => {
    rootLegacy.unmount();
    rootUnified.unmount();
  });
  resetPrivateSession();
  console.log("memberComposerUnify checks passed");
}

main().catch((error) => {
  console.error(error);
  throw error;
});
