export {};
/**
 * CAD-2743: WhatsApp UX polish — unread store & toast arrival suppression,
 * NavList badge & rolled-up count, screen-whatsapp-ux.v1 capability fencing,
 * and per-thread text drafts that keep the rail unlocked + emit optimistic echo.
 */
import type { MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import type { WhatsAppReplySelection } from "../src/features/workspace-apps/screen/WhatsAppReplyComposer";

declare function require(name: string): any;
declare const process: { exit(code: number): never };

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 15));
  await Promise.resolve();
}

function setTextarea(box: HTMLTextAreaElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  setter?.call(box, value);
  box.dispatchEvent(new Event("input", { bubbles: true }));
}

function trustedClick(button: HTMLButtonElement): void {
  const down = new MouseEvent("pointerdown", { bubbles: true, cancelable: true, button: 0 });
  Object.defineProperty(down, "isTrusted", { value: true });
  button.dispatchEvent(down);
  const click = new MouseEvent("click", { bubbles: true, cancelable: true, detail: 1 });
  Object.defineProperty(click, "isTrusted", { value: true });
  button.dispatchEvent(click);
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
  const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");
  const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const { resetPrivateSession, allowPrivateReads } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const {
    fetchWhatsAppUnread,
    markWhatsAppThreadUnread,
    parseWhatsAppUnreadSnapshot,
    shouldNotifyWhatsAppArrival,
    formatWhatsAppUnreadNote,
    setWhatsAppUnreadSnapshot,
    clearWhatsAppUnreadSnapshot,
  } = require("../src/features/app-shell/whatsappUnread") as typeof import("../src/features/app-shell/whatsappUnread");
  const { NavList: NavListView } = require("../src/ui/NavList") as typeof import("../src/ui/NavList");
  const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
  const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
  const { updateSidebarPrefs, DEFAULT_SIDEBAR_PREFS } = require("../src/features/app-shell/sidebarPrefs") as typeof import("../src/features/app-shell/sidebarPrefs");
  const { parseChild, SCREEN_WHATSAPP_UX_V1 } = require("../src/features/workspace-apps/screen/screenProtocol") as typeof import("../src/features/workspace-apps/screen/screenProtocol");
  const { ScreenChannel } = require("../src/features/workspace-apps/screen/screenLifecycle") as typeof import("../src/features/workspace-apps/screen/screenLifecycle");
  const composerMod = require("../src/features/workspace-apps/screen/WhatsAppReplyComposer") as typeof import("../src/features/workspace-apps/screen/WhatsAppReplyComposer");
  const WhatsAppReplyComposer = composerMod.default;
  const { getWhatsAppDraftPreviews } = composerMod;

  // 1. Unread snapshot parser, arrival notification rules & note formatting
  const baseSnap = parseWhatsAppUnreadSnapshot({
    provider: "linked",
    available: true,
    total_unread: 2,
    unread_threads: 1,
    latest: {
      conversation_id: "15550001111@s.whatsapp.net",
      label: "Alice",
      preview: "Are you free today?",
      media_kind: null,
      file_name: null,
      ptt: false,
      last_inbound_at: "2026-11-24T01:00:00.000Z",
      unread_count: 2,
    },
  });
  assert(baseSnap && baseSnap.totalUnread === 2 && baseSnap.latest?.label === "Alice", "unread snapshot parses");
  assert(
    !shouldNotifyWhatsAppArrival(null, baseSnap, null, "waba-1", true),
    "cold baseline does not toast",
  );
  const nextSnap = parseWhatsAppUnreadSnapshot({
    provider: "linked",
    available: true,
    total_unread: 3,
    unread_threads: 2,
    latest: {
      conversation_id: "15550002222@s.whatsapp.net",
      label: "Bob",
      preview: null,
      media_kind: "audio",
      file_name: null,
      ptt: true,
      last_inbound_at: "2026-11-24T01:02:00.000Z",
      unread_count: 1,
    },
  })!;
  assert(
    !shouldNotifyWhatsAppArrival(baseSnap, nextSnap, { installId: "waba-1", conversationId: "15550002222@s.whatsapp.net" }, "waba-1", true),
    "suppressed when actively viewing that thread in foreground",
  );
  assert(
    shouldNotifyWhatsAppArrival(baseSnap, nextSnap, { installId: "waba-1", conversationId: "15550001111@s.whatsapp.net" }, "waba-1", true),
    "notifies when viewing another conversation",
  );
  assert(
    formatWhatsAppUnreadNote(nextSnap.latest!, (s) => s) === "🎤 Voice message",
    "PTT voice message toast note formatted cleanly",
  );
  assert(
    formatWhatsAppUnreadNote(
      { ...nextSnap.latest!, mediaKind: "document", fileName: "invoice.pdf", ptt: false },
      (s) => s,
    ) === "📄 invoice.pdf",
    "document filename toast note formatted cleanly",
  );

  // 2. NavList numeric badge & rolled-up parent badge
  setWhatsAppUnreadSnapshot("install-waba", nextSnap);
  const appMenu = {
    apps: [
      { installId: "install-waba", kind: "waba", title: "WhatsApp Business", href: "/apps/waba", current: false, sections: null },
    ],
  };
  const renderNav = () =>
    renderToStaticMarkup(
      React.createElement(
        NavigationProvider,
        {
          adapter: hostNavigation,
          children: React.createElement(
            LocaleProvider,
            null,
            React.createElement(NavListView, {
              screen: "home",
              navHref: (r: { screen: string }) => `/${r.screen}`,
              appMenu,
            } as any),
          ),
        },
      ),
    );
  updateSidebarPrefs(() => ({ ...DEFAULT_SIDEBAR_PREFS, appsOpen: true }));
  const expandedHtml = renderNav();
  assert(
    expandedHtml.includes('class="navbadge navbadge-accent num"') && expandedHtml.includes(">3<"),
    "expanded Apps shows WhatsApp numeric badge on child row",
  );
  updateSidebarPrefs(() => ({ ...DEFAULT_SIDEBAR_PREFS, appsOpen: false }));
  const foldedHtml = renderNav();
  const appsAnchor = /<a [^>]*>(?:(?!<\/a>).)*Apps(?:(?!<\/a>).)*<\/a>/.exec(foldedHtml)?.[0] ?? "";
  assert(
    appsAnchor.includes("navbadge navbadge-accent") && appsAnchor.includes(">3<"),
    "folded Apps rolls WhatsApp badge count onto parent link",
  );
  clearWhatsAppUnreadSnapshot("install-waba");

  // 3. screenProtocol & ScreenChannel screen-whatsapp-ux.v1 capability fencing
  const readyWithUx = parseChild({
    v: 1,
    op: "ready",
    accepts: [
      "screen.v2",
      "screen-records.v1",
      "screen-locale.v1",
      "screen-whatsapp-compose.v1",
      "screen-whatsapp-reply-availability.v1",
      "screen-whatsapp-reply-layout.v2",
      "screen-whatsapp-media.v1",
      SCREEN_WHATSAPP_UX_V1,
    ],
  });
  assert(
    readyWithUx?.op === "ready" &&
      Array.isArray(readyWithUx.accepts) &&
      (readyWithUx.accepts as readonly string[]).includes(SCREEN_WHATSAPP_UX_V1),
    "8+ accepts with screen-whatsapp-ux.v1 accepted",
  );
  assert(
    parseChild({ v: 2, op: "whatsapp-thread-unread", conversation_id: "15550001111@s.whatsapp.net", provider: "linked", unread: false })?.op === "whatsapp-thread-unread",
    "valid whatsapp-thread-unread parses",
  );
  assert(
    parseChild({ v: 2, op: "whatsapp-thread-unread", conversation_id: "bad\nid", provider: "linked", unread: false }) === null,
    "control char in whatsapp-thread-unread rejected",
  );
  assert(
    parseChild({ v: 2, op: "whatsapp-thread-unread", conversation_id: "c1", provider: "sms", unread: false }) === null,
    "invalid provider rejected",
  );

  class Port {
    onmessage: ((event: { data: unknown }) => void) | null = null;
    onmessageerror: (() => void) | null = null;
    sent: unknown[] = [];
    closed = false;
    start() {}
    close() { this.closed = true; }
    postMessage(value: unknown) { this.sent.push(value); }
    receive(value: unknown) { this.onmessage?.({ data: value }); }
  }
  const nonce = "c".repeat(64);
  const digest = `sha256:${"a".repeat(64)}`;
  const receipt: MountReceipt = {
    mount: `/api/app-screen/${"d".repeat(64)}`,
    bridge_nonce: nonce,
    generation: 1,
    tag: "waba",
    host_contract: "screen-records.v1",
    action_token: "e".repeat(64),
    tools: [],
    navigation: null,
    install_id: "install-waba",
    context_id: "ctx-1",
    digest,
    display_scope_key: "b".repeat(64),
    display_session_key: "f".repeat(64),
    remote_images: [],
    operations: ["whatsapp.conversations.show"],
  };
  const push: ScreenPush = {
    v: 2, op: "screen", install_id: "install-waba", digest, tag: "waba", context_id: "ctx-1",
    installation: { install_id: "install-waba", digest, title: "WhatsApp Business", name: "waba", version: "0.5.0", summary: "" },
    contexts: [{ id: "ctx-1", label: "Work" }], runs: [], outbox: [], now: 1,
    publish_intents: { status: "ok", withheld: 0, rows: [] },
    actions: receipt.operations,
  };
  const init = { v: 1, op: "init", tag: "waba", bridge_nonce: nonce, generation: 1 };

  // Without opt-in -> closes on whatsapp-thread-unread
  {
    const source = {} as Window;
    const port = new Port();
    let removed = 0;
    const ch = new ScreenChannel(source, receipt, push, () => { removed++; }, () => {}, () => {}, async () => null, {
      call: async () => ({ ok: true, data: null }),
      planner: async () => ({ code: "forbidden", text: "no" }),
      onSlot: () => {},
      onLink: () => {},
      onWhatsAppThreadUnread: () => {},
    });
    ch.receive({ source, origin: "null", data: init, ports: [port as unknown as MessagePort] });
    port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-records.v1"] });
    assert(!ch.whatsappUx, "whatsappUx false when frame omitted opt-in");
    port.receive({ v: 2, op: "whatsapp-thread-unread", conversation_id: "1555@s.whatsapp.net", provider: "linked", unread: false });
    assert(port.closed && removed === 1, "un-negotiated whatsapp-thread-unread closes channel");
  }

  // With opt-in -> allows unread sync + host->child ops
  {
    const source = {} as Window;
    const port = new Port();
    const unreadEvents: Array<[string, string, boolean]> = [];
    const ch = new ScreenChannel(source, receipt, push, () => {}, () => {}, () => {}, async () => null, {
      call: async () => ({ ok: true, data: null }),
      planner: async () => ({ code: "forbidden", text: "no" }),
      onSlot: () => {},
      onLink: () => {},
      onWhatsAppThreadUnread: (cid, provider, unread) => { unreadEvents.push([cid, provider, unread]); },
    });
    ch.receive({ source, origin: "null", data: init, ports: [port as unknown as MessagePort] });
    port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-records.v1", SCREEN_WHATSAPP_UX_V1] });
    assert(ch.whatsappUx, "whatsappUx enabled after opt-in");
    port.receive({ v: 2, op: "whatsapp-thread-unread", conversation_id: "15550001111@s.whatsapp.net", provider: "linked", unread: false });
    assert(unreadEvents.length === 1 && unreadEvents[0][2] === false, "whatsapp-thread-unread relayed to host action");
    ch.whatsappOpenThread("15550001111@s.whatsapp.net");
    ch.whatsappDrafts({ "15550001111@s.whatsapp.net": "Draft hi" });
    ch.whatsappOutboundEcho({
      phase: "pending",
      temp_id: "tmp-1",
      conversation_id: "15550001111@s.whatsapp.net",
      provider: "linked",
      text: "Sending...",
      media_kind: null,
      file_name: null,
      created_at: "2026-11-24T01:05:00.000Z",
    });
    assert(
      port.sent.some((m: any) => m.op === "whatsapp-open-thread") &&
        port.sent.some((m: any) => m.op === "whatsapp-drafts") &&
        port.sent.some((m: any) => m.op === "whatsapp-outbound-echo" && m.phase === "pending"),
      "outbound host->child UX messages posted over port",
    );
  }

  // 4. WhatsAppReplyComposer per-thread drafts + rail unlock + optimistic echo + privateSession reset
  const dirtyEvents: boolean[] = [];
  const echoEvents: string[] = [];
  let sentRefreshCount = 0;
  const mockServices = {
    async call(op: string) {
      if (op === "whatsapp.conversations.show") {
        return {
          ok: true as const,
          data: {
            contract: "agenticos.whatsapp-door.v1",
            conversationId: currentConv,
            workRef: `lw_${"a".repeat(32)}_${"b".repeat(64)}`,
            contact: { name: "Alice", phone: "+15550001111" },
          },
        };
      }
      if (op === "whatsapp.reply.prepare") {
        return { ok: true as const, data: { decision: "prepared", effect_id: `wae_${"c".repeat(32)}` } };
      }
      return { ok: true as const, data: {} };
    },
    async planner() {
      return {
        run: async () => ({ ok: true as const, data: { reply: { state: "sent" } } }),
      };
    },
  };
  let currentConv = "15550001111@s.whatsapp.net";
  const makeSelection = (conversationId: string): WhatsAppReplySelection => {
    currentConv = conversationId;
    return {
      conversationId,
      provider: "linked",
      draftScope: "install-ux-test",
      preserveTextDrafts: true,
      receiptScope: "scope-ux",
      services: mockServices as unknown as WhatsAppReplySelection["services"],
      current: () => true,
      announce: (dirty) => { dirtyEvents.push(dirty); },
      announceOutboundEcho: (echo) => { echoEvents.push(`${echo.phase}:${echo.conversation_id}:${echo.text}`); },
      onSentRefreshUnread: () => { sentRefreshCount++; },
    };
  };

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  try {
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(WhatsAppReplyComposer, { key: "a1", selection: makeSelection("15550001111@s.whatsapp.net") })));
      await flush();
    });
    let tArea = container.querySelector("textarea") as HTMLTextAreaElement;
    await React.act(async () => {
      setTextarea(tArea, "Hello Alice");
      await flush();
    });
    assert(dirtyEvents.every((d) => d === false), "plain text draft keeps rail unlocked when preserveTextDrafts=true");
    assert(getWhatsAppDraftPreviews("install-ux-test")["15550001111@s.whatsapp.net"] === "Hello Alice", "draft preview saved for chat A");

    // Switch to chat B
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(WhatsAppReplyComposer, { key: "b1", selection: makeSelection("15550002222@s.whatsapp.net") })));
      await flush();
    });
    tArea = container.querySelector("textarea") as HTMLTextAreaElement;
    assert(tArea.value === "", "chat B starts with empty draft");
    await React.act(async () => {
      setTextarea(tArea, "Follow up Bob");
      await flush();
    });

    // Switch back to chat A -> draft restored -> send clears draft + emits pending & sent + calls onSentRefreshUnread
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(WhatsAppReplyComposer, { key: "a2", selection: makeSelection("15550001111@s.whatsapp.net") })));
      await flush();
    });
    tArea = container.querySelector("textarea") as HTMLTextAreaElement;
    assert(tArea.value === "Hello Alice", "switching back to chat A restores draft");
    const sendBtn = container.querySelector("button.wa-reply-send") as HTMLButtonElement;
    await React.act(async () => {
      trustedClick(sendBtn);
      await flush();
    });
    assert(!getWhatsAppDraftPreviews("install-ux-test")["15550001111@s.whatsapp.net"], "sending clears sent thread draft");
    assert(
      getWhatsAppDraftPreviews("install-ux-test")["15550002222@s.whatsapp.net"] === "Follow up Bob",
      "other thread draft remains intact after send",
    );
    assert(
      echoEvents[0] === "pending:15550001111@s.whatsapp.net:Hello Alice" &&
        echoEvents[1] === "sent:15550001111@s.whatsapp.net:Hello Alice" &&
        sentRefreshCount === 1,
      "optimistic outbound echo emits pending -> sent and triggers unread refresh",
    );

    // Private session reset wipes remaining drafts
    resetPrivateSession();
    allowPrivateReads();
    assert(
      Object.keys(getWhatsAppDraftPreviews("install-ux-test")).length === 0,
      "resetPrivateSession clears stored WhatsApp drafts",
    );
  } finally {
    await React.act(async () => {
      root.unmount();
    });
    container.remove();
  }

  // 5. Regression: fetchWhatsAppUnread and markWhatsAppThreadUnread must read res.json() inside withPrivateOperation
  // before operation.dispose() aborts init.signal.
  {
    const realFetch = globalThis.fetch;
    let getAbortedDuringJson: boolean | null = null;
    let postAbortedDuringJson: boolean | null = null;
    try {
      (globalThis as Record<string, unknown>).fetch = async (_url: unknown, init?: { method?: string; signal?: AbortSignal }) => {
        const sig = init?.signal;
        const isPost = (init?.method ?? "GET") === "POST";
        return {
          ok: true,
          status: 200,
          async json() {
            await Promise.resolve();
            if (isPost) postAbortedDuringJson = sig?.aborted ?? null;
            else getAbortedDuringJson = sig?.aborted ?? null;
            if (isPost) return { ok: true, unread_count: 0 };
            return { provider: "linked", available: true, total_unread: 1, unread_threads: 1, latest: null };
          },
          clone() {
            return this;
          },
        };
      };
      const got = await fetchWhatsAppUnread("install-abort-check");
      assert(got.ok && got.data.totalUnread === 1, "fetchWhatsAppUnread resolves inside withPrivateOperation");
      assert(getAbortedDuringJson === false, "GET signal remains un-aborted during res.json()");
      const marked = await markWhatsAppThreadUnread("install-abort-check", "linked", "15550001111@s.whatsapp.net", false);
      assert(marked.ok && marked.unreadCount === 0, "markWhatsAppThreadUnread resolves inside withPrivateOperation");
      assert(postAbortedDuringJson === false, "POST signal remains un-aborted during res.json()");
    } finally {
      (globalThis as Record<string, unknown>).fetch = realFetch;
    }
  }

  console.log("whatsappUx.test.tsx: ALL PASSED");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
