/** CAD-2892 Channels Wave 3 checks: optimistic send/retry, transient queued retirement,
 *  WhatsApp-style bubbles & grouping, live working/streaming previews, and unified + Add dialog. */
declare function require<T = unknown>(name: string): T;
declare const process: { exitCode?: number };

import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as HumanThreadsModule from "../src/features/threads/HumanThreads";
import type * as StreamModule from "../src/features/threads/humanThreadsStream";
import type * as ClientModule from "../src/features/threads/humanThreadsClient";
import type * as LocaleModule from "../src/lib/locale";
import type * as SessionModule from "../src/lib/privateSession";
import type * as SessionKeyModule from "../src/lib/sessionKey";
import type * as ComposerStoreModule from "../src/features/app-shell/chat/composerStore";
import type { SseOptions, SseSubscription } from "../src/lib/sse";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

function equal(actual: unknown, expected: unknown, msg: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}: expected ${e}, got ${a}`);
}

async function flushUi(ms = 25): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function main(): Promise<void> {
  const { Window } = require<{ Window: new (options: { url: string }) => Record<string, any> }>("happy-dom");
  const win = new Window({ url: "http://localhost/threads?h=grp_wave3" });
  const pointerCtor = win.PointerEvent ?? win.MouseEvent;
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLAnchorElement",
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
    "CustomEvent",
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
  Object.defineProperty(globalThis, "IntersectionObserver", {
    value: class {
      observe() {}
      disconnect() {}
      unobserve() {}
    },
    configurable: true,
  });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  win.HTMLElement.prototype.scrollIntoView = () => {};
  win.HTMLDialogElement.prototype.show = function (this: { open: boolean }) { this.open = true; };
  win.HTMLDialogElement.prototype.showModal = function (this: { open: boolean }) { this.open = true; };
  win.HTMLDialogElement.prototype.close = function (this: { open: boolean }) { this.open = false; };

  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, id);
  };

  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const { LocaleProvider } = require<typeof LocaleModule>("../src/lib/locale");
  const { allowPrivateReads, resetPrivateSession } = require<typeof SessionModule>("../src/lib/privateSession");
  const { setSessionKey } = require<typeof SessionKeyModule>("../src/lib/sessionKey");
  const { setComposerContent } = require<typeof ComposerStoreModule>("../src/features/app-shell/chat/composerStore");
  const {
    createHumanThreadsStreamStore,
    parseHumanThreadsPreviewFrame,
  } = require<typeof StreamModule>("../src/features/threads/humanThreadsStream");
  const { HumanRoomTimeline, HumanContextPanel } = require<typeof HumanThreadsModule>("../src/features/threads/HumanThreads");

  setSessionKey("channels-wave3-test");
  allowPrivateReads();

  // 1. parseHumanThreadsPreviewFrame & stream store: sanitizing, clamping, clear, stable snapshots, & reset
  const oversized = parseHumanThreadsPreviewFrame(JSON.stringify({
    room: "grp_1",
    agent_id: "uid-bookkeeper",
    alias: "bookkeeper",
    state: "streaming",
    text: "a".repeat(9000),
    step: "Checking Q4 ledger",
    started_at: 1000,
  }));
  assert(oversized !== null && oversized.text?.length === 8192, "clamps oversized preview text to 8192");

  const secretMasked = parseHumanThreadsPreviewFrame(JSON.stringify({
    room: "grp_1",
    agent_id: "uid-bookkeeper",
    alias: "bookkeeper",
    state: "streaming",
    text: '{"token":"sk-live-secret"}',
  }));
  assert(secretMasked !== null && secretMasked.text === null, "withholds secret-like preview JSON object");

  const proseBeforeJson = parseHumanThreadsPreviewFrame(JSON.stringify({
    room: "grp_1",
    agent_id: "uid-bookkeeper",
    alias: "bookkeeper",
    state: "streaming",
    text: 'Reconciling Q4\n{"secret":"abc"}',
  }));
  assert(proseBeforeJson !== null && proseBeforeJson.text === "Reconciling Q4", "strips trailing JSON block from streamed preview text");

  let capturedOpts: SseOptions | undefined;
  const subStub: SseSubscription = { close: () => {}, lastEventId: () => null };
  const store = createHumanThreadsStreamStore((opts) => {
    capturedOpts = opts;
    return subStub;
  });
  const unsub = store.subscribe(() => {});
  const sse = capturedOpts;
  assert(sse !== undefined && sse.events.includes("human_threads_preview"), "subscribes to human_threads_preview SSE events");
  sse.onEvent({
    id: null,
    type: "human_threads_preview",
    data: JSON.stringify({
      room: "grp_1",
      agent_id: "uid-bookkeeper",
      alias: "bookkeeper",
      state: "streaming",
      text: "Reconciling Q4 entries",
      started_at: 2000,
    }),
  });
  const snap1 = store.previewsForRoom("grp_1");
  const snap2 = store.previewsForRoom("grp_1");
  assert(snap1.length === 1 && snap1 === snap2, "keeps cached snapshot referentially stable across unchanged reads");
  sse.onEvent({
    id: null,
    type: "human_threads_preview",
    data: JSON.stringify({
      room: "grp_1",
      agent_id: "uid-bookkeeper",
      alias: "bookkeeper",
      state: "clear",
      text: null,
    }),
  });
  equal(store.previewsForRoom("grp_1"), [], "clears preview state on state=clear");
  unsub();

  // 2. Mocked room API for HumanRoomTimeline & HumanContextPanel
  const me: ClientModule.HumanMe = { id: "h-me", name: "Patrick" };
  const agents: ClientModule.HumanAgent[] = [
    { uid: "uid-bookkeeper", alias: "bookkeeper", look: "bookkeeper" },
    { uid: "uid-reviewer", alias: "codex", look: "builder" },
  ];
  const people: ClientModule.HumanPerson[] = [
    { id: "h-me", subject: "sub-me", name: "Patrick" },
    { id: "h-alice", subject: "sub-alice", name: "Alice Chen" },
    { id: "h-bob", subject: "sub-bob", name: "Bob Lin" },
  ];
  let roomPage: ClientModule.HumanRoomPage = {
    room: "grp_wave3",
    scope_id: "csrid_wave3room",
    members: [
      { kind: "human", id: "h-me", label: "Patrick" },
      { kind: "human", id: "h-alice", label: "Alice Chen" },
      { kind: "agent", id: "uid-bookkeeper", label: "bookkeeper", look: "bookkeeper" },
    ],
    posts: [
      {
        id: "post-1",
        read_cursor: "csc1_post1",
        sender: { id: "h-alice", label: "Alice Chen", kind: "human" },
        body: "First teammate message",
        created: 1_700_000_000,
        silent: false,
        outcomes: [],
      },
      {
        id: "post-2",
        read_cursor: "csc1_post2",
        sender: { id: "h-alice", label: "Alice Chen", kind: "human" },
        body: "Second consecutive message within 3m",
        created: 1_700_000_030,
        silent: false,
        outcomes: [{ kind: "queued", handle: "@bookkeeper", label: "bookkeeper" }],
      },
      {
        id: "post-3",
        read_cursor: "csc1_post3",
        sender: { id: "uid-bookkeeper", label: "bookkeeper", kind: "agent" },
        body: "Ledger looks clean!",
        created: 1_700_000_060,
        silent: false,
        outcomes: [],
      },
      {
        id: "post-4",
        read_cursor: "csc1_post4",
        sender: { id: "h-me", label: "Patrick", kind: "human" },
        body: "@bookkeeper double check Q4",
        created: 1_700_000_090,
        silent: false,
        outcomes: [{ kind: "queued", handle: "@bookkeeper", label: "bookkeeper" }],
      },
    ],
    events: [],
    active_agents: [
      {
        id: "uid-bookkeeper",
        alias: "bookkeeper",
        look: "bookkeeper",
        state: "working",
        started_at: 1_700_000_091_000,
        step: "Checking Q4 balances",
      },
    ],
    before: "csc1_post1",
    more_before: false,
    can_ask: true,
    can_manage: true,
    agent_chain_count: 0,
    agent_chain_limit: 5,
  };

  let postMode: "fail" | "succeed" = "fail";
  const postedPayloads: Array<{ client_id: string; body: string }> = [];
  const memberAdditions: Array<Record<string, unknown>> = [];

  Object.defineProperty(globalThis, "fetch", {
    value: async (input: string | Request, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.url;
      const method = (init?.method ?? "GET").toUpperCase();
      const jsonOk = (body: unknown) => ({
        ok: true,
        status: 200,
        statusText: "200",
        json: async () => body,
      });
      if (url.startsWith("/api/human-threads/agents")) return jsonOk({ agents });
      if (url.startsWith("/api/human-threads/people")) return jsonOk({ actor: me.id, people });
      if (url.startsWith("/api/human-threads/rooms/grp_wave3/posts") && method === "POST") {
        const parsed = JSON.parse(String(init?.body ?? "{}")) as { client_id: string; body: string };
        postedPayloads.push(parsed);
        if (postMode === "fail") {
          return {
            ok: false,
            status: 502,
            statusText: "Bad Gateway",
            json: async () => ({ error: { message: "Temporary relay error" } }),
          };
        }
        const createdPost: ClientModule.HumanPost = {
          id: "post-5",
          read_cursor: "csc1_post5",
          sender: { id: "h-me", label: "Patrick", kind: "human" },
          body: parsed.body,
          created: 1_700_000_120,
          silent: false,
          outcomes: [],
        };
        roomPage = { ...roomPage, posts: [...roomPage.posts, createdPost], active_agents: [] };
        return jsonOk({ created: true, post: createdPost });
      }
      if (url.startsWith("/api/human-threads/rooms/grp_wave3/members") && method === "POST") {
        const parsed = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        memberAdditions.push(parsed);
        return jsonOk({ ok: true });
      }
      if (url.startsWith("/api/human-threads/rooms/grp_wave3/read")) return jsonOk({ ok: true });
      if (url.startsWith("/api/human-threads/rooms/grp_wave3")) return jsonOk(roomPage);
      return { ok: false, status: 404, statusText: "404", json: async () => ({}) };
    },
    configurable: true,
    writable: true,
  });

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const ctxHost = document.createElement("div");
  document.body.appendChild(ctxHost);
  const ctxRoot = createRoot(ctxHost);

  try {
    await React.act(async () => {
      root.render(
        React.createElement(LocaleProvider, null,
          React.createElement(HumanRoomTimeline, {
            id: "grp_wave3",
            me,
            refresh: 0,
            onState: () => {},
            onOpenContext: () => {},
            onChanged: () => {},
            isDm: false,
            roomTitle: "finance-ops",
            people,
          }),
        ),
      );
    });
    for (let i = 0; i < 6; i += 1) await flushUi();

    // Check WhatsApp bubbles, Agent pill, grouping, clean subtitle, retired queued pills, and live working bubble
    const kicker = host.querySelector(".threads-kicker")?.textContent ?? "";
    assert(kicker === "3 members", `expected clean member count kicker without duplicate Channel, got "${kicker}"`);
    assert(host.querySelectorAll(".threads-wa-bubble.is-them").length >= 2, "incoming messages render with elevated WhatsApp card class");
    assert(host.querySelector(".threads-wa-bubble.is-agent .threads-wa-agent-pill")?.textContent === "Agent", "agent message displays Agent pill badge");
    assert(host.querySelector(".threads-msg.is-grouped") !== null, "consecutive messages from same sender within 3m collapse into grouped turn");

    // Both post-2 (answered by post-3) and post-4 (currently live working) have their stale queued outcomes retired
    const remainingQueued = Array.from(host.querySelectorAll('.threads-outcome[data-kind="queued"]'));
    equal(remainingQueued.length, 0, "retires stale @bookkeeper · queued pills when answered or actively working");

    const liveWorking = host.querySelector(".threads-live-working");
    assert(
      liveWorking !== null &&
        liveWorking.querySelector(".workdot.live") !== null &&
        (liveWorking.textContent ?? "").includes("Checking Q4 balances"),
      "renders live working bubble with Home workdot and step label from active_agents",
    );

    // Optimistic send -> immediate bubble before network -> failure Retry reusing client_id -> reconcile
    setComposerContent("threads-human:grp_wave3", "Optimistic hello", []);
    await flushUi();
    const submitBtn = host.querySelector<HTMLButtonElement>('.threads-composer-card button[type="submit"]');
    assert(submitBtn !== null, "composer submit button is present");

    await React.act(async () => {
      submitBtn.click();
    });
    for (let i = 0; i < 6; i += 1) await flushUi();

    equal(postedPayloads.length, 1, "posted optimistic message once");
    const firstClientId = postedPayloads[0]!.client_id;
    const failedBubble = host.querySelector(`.threads-msg-pending.is-failed[data-client-id="${firstClientId}"]`);
    assert(failedBubble !== null && (failedBubble.textContent ?? "").includes("Retry"), "marks failed optimistic post with inline Retry");
    const pendingTime = failedBubble.querySelector(".threads-wa-at")?.textContent ?? "";
    assert(pendingTime.length > 0 && !pendingTime.startsWith("+0"), `renders formatted epoch-seconds pending timestamp, got "${pendingTime}"`);

    postMode = "succeed";
    const retryBtn = failedBubble.querySelector<HTMLButtonElement>(".threads-pending-btn.is-retry");
    assert(retryBtn !== null, "Retry button rendered inside failed optimistic post");
    await React.act(async () => {
      retryBtn.click();
    });
    for (let i = 0; i < 6; i += 1) await flushUi();

    equal(postedPayloads.length, 2, "Retry sends second POST");
    equal(postedPayloads[1]!.client_id, firstClientId, "Retry reuses exact same client_id for idempotency");
    assert(host.querySelector(".threads-msg-pending") === null, "reconciles pending bubble cleanly after server post lands");
    assert((host.textContent ?? "").includes("Optimistic hello"), "reconciled post text remains visible in timeline");

    // 3. HumanContextPanel removes inline selects and uses section + Add buttons opening unified modal
    await React.act(async () => {
      ctxRoot.render(
        React.createElement(LocaleProvider, null,
          React.createElement(HumanContextPanel, {
            room: { id: "grp_wave3", kind: "mixed_group", title: "finance-ops", creator: "h-me", created: 100, unread: 0 },
            me,
            members: roomPage.members,
            denied: false,
            canManage: true,
            agentChainCount: 0,
            agentChainLimit: 5,
            people,
            peopleError: null,
            agents,
            onChanged: () => {},
            onOpened: () => {},
          }),
        ),
      );
    });
    for (let i = 0; i < 4; i += 1) await flushUi();

    assert(ctxHost.querySelector(".threads-add-inline") === null, "inline select+Add forms removed from Details panel");
    const addButtons = Array.from(ctxHost.querySelectorAll<HTMLButtonElement>(".threads-section-add"));
    equal(addButtons.length, 2, "Teammates and Shared Agents headings each render a + Add button");

    await React.act(async () => {
      addButtons[0]!.click();
    });
    for (let i = 0; i < 5; i += 1) await flushUi();

    const dialog = ctxHost.querySelector<HTMLDialogElement>("dialog.threads-add-dialog");
    assert(dialog !== null && (dialog.open || dialog.hasAttribute("open")), "clicking section + Add opens unified AddToChannelDialog");

    const bobCheckbox = Array.from(dialog.querySelectorAll<HTMLLabelElement>(".threads-add-choice")).find((el) =>
      (el.textContent ?? "").includes("Bob Lin"),
    )?.querySelector<HTMLInputElement>('input[type="checkbox"]');
    assert(bobCheckbox !== null && bobCheckbox !== undefined, "lists non-member Bob Lin inside Teammates tab");
    await React.act(async () => {
      bobCheckbox.click();
    });
    const modalSubmit = dialog.querySelector<HTMLButtonElement>('.threads-add-dialog-foot button[type="submit"]');
    assert(modalSubmit !== null && !modalSubmit.disabled, "enables Add button once teammate is selected");
    await React.act(async () => {
      modalSubmit.click();
    });
    for (let i = 0; i < 5; i += 1) await flushUi();

    equal(memberAdditions, [{ human_subject: "sub-bob" }], "unified Add dialog calls addHumanMember with human_subject");
  } finally {
    await React.act(async () => {
      ctxRoot.unmount();
      root.unmount();
    });
    ctxHost.remove();
    host.remove();
    resetPrivateSession();
  }
  console.log("channels wave 3 checks passed");
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
