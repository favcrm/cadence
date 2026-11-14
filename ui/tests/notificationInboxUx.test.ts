import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as InboxModule from "../src/features/threads/NotificationInbox";
import type * as ToastModule from "../src/features/threads/NotificationToast";
import type * as ClientModule from "../src/features/threads/humanThreadsClient";
import type * as LocaleModule from "../src/lib/locale";
import type * as LocaleTextModule from "../src/lib/localeText";
import type * as SessionModule from "../src/lib/privateSession";

declare function require<T>(name: string): T;

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

async function main(): Promise<void> {
  type Dom = Window & Pick<
    typeof globalThis,
    "Node" | "Element" | "HTMLElement" | "HTMLButtonElement" | "HTMLDialogElement" | "SVGElement" | "Event" | "MouseEvent" | "KeyboardEvent" | "PointerEvent"
  >;
  const { Window: BrowserWindow } = require<{ Window: new (options: { url: string }) => Dom }>("happy-dom");
  const win = new BrowserWindow({ url: "http://localhost/" });
  const pointerCtor = win.PointerEvent ?? win.MouseEvent;
  Object.defineProperties(globalThis, {
    window: { value: win, configurable: true },
    document: { value: win.document, configurable: true },
    navigator: { value: win.navigator, configurable: true },
    location: { value: win.location, configurable: true },
    history: { value: win.history, configurable: true },
    sessionStorage: { value: win.sessionStorage, configurable: true },
    localStorage: { value: win.localStorage, configurable: true },
    Node: { value: win.Node, configurable: true },
    Element: { value: win.Element, configurable: true },
    HTMLElement: { value: win.HTMLElement, configurable: true },
    HTMLButtonElement: { value: win.HTMLButtonElement, configurable: true },
    HTMLDialogElement: { value: win.HTMLDialogElement, configurable: true },
    SVGElement: { value: win.SVGElement, configurable: true },
    Event: { value: win.Event, configurable: true },
    MouseEvent: { value: win.MouseEvent, configurable: true },
    KeyboardEvent: { value: win.KeyboardEvent, configurable: true },
    PointerEvent: { value: pointerCtor, configurable: true },
    addEventListener: { value: win.addEventListener.bind(win), configurable: true },
    removeEventListener: { value: win.removeEventListener.bind(win), configurable: true },
    IS_REACT_ACT_ENVIRONMENT: { value: true, configurable: true },
  });

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
  const NotificationInbox = require<typeof InboxModule>("../src/features/threads/NotificationInbox").default;
  const { NotificationToastProvider } = require<typeof ToastModule>("../src/features/threads/NotificationToast");
  const { translateUiText } = require<typeof LocaleModule>("../src/lib/locale");
  const { LocaleTextProvider } = require<typeof LocaleTextModule>("../src/lib/localeText");
  const { allowPrivateReads, resetPrivateSession } = require<typeof SessionModule>("../src/lib/privateSession");
  const { setImmediate: nextTurn } = require<{ setImmediate: (callback: () => void) => void }>("node:timers");
  const flush = () => React.act(async () => { await new Promise<void>(nextTurn); });
  async function settle(check: () => boolean, message: string) {
    for (let attempt = 0; attempt < 40; attempt += 1) {
      await flush();
      if (check()) return;
    }
    throw new Error(message);
  }

  const emptyFeed: ClientModule.NotificationInbox = {
    human: { id: "human-1", name: "Operator" },
    unread_total: 0,
    groups: [],
    preferences: { dm: true, mention: true, sound: false, preview: true, quiet: "off", zone: "UTC" },
    push: { available: false, public_key: null },
  };
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (input: RequestInfo | URL) => {
    const path = new URL(input instanceof Request ? input.url : String(input), "http://localhost").pathname;
    if (path === "/api/human-threads/notifications") {
      return new Response(JSON.stringify(emptyFeed), { headers: { "Content-Type": "application/json" } });
    }
    throw new Error(`Unexpected request ${path}`);
  };

  const host = document.createElement("div");
  const outside = document.createElement("div");
  document.body.append(host, outside);
  let root: Root | null = createRoot(host);

  try {
    resetPrivateSession();
    allowPrivateReads();
    await React.act(async () => {
      root?.render(
        React.createElement(NotificationToastProvider, null, React.createElement(NotificationInbox, { authEpoch: "ux-1" })),
      );
    });
    await settle(() => host.querySelector("button.bell-button") !== null, "notification bell mounts");

    const bell = () => host.querySelector("button.bell-button") as HTMLButtonElement;
    const popover = () => host.querySelector("dialog.notification-popover") as HTMLDialogElement;

    assert(bell().getAttribute("aria-expanded") === "false", "bell starts with aria-expanded=false");
    assert(!popover().open, "popover starts closed");

    // Clicking bell toggles open; clicking bell again closes and restores aria-expanded=false.
    await React.act(async () => { bell().click(); });
    assert(popover().open && bell().getAttribute("aria-expanded") === "true", "first bell click opens popover");
    await React.act(async () => { bell().click(); });
    assert(!popover().open && bell().getAttribute("aria-expanded") === "false", "second bell click closes popover");

    // Pressing Escape dismisses non-modal popover and returns focus to the bell button.
    await React.act(async () => { bell().click(); });
    assert(popover().open, "popover reopens before Escape");
    const closeBtn = host.querySelector('button[aria-label="Close notifications"]') as HTMLButtonElement;
    closeBtn.focus();
    await React.act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    assert(!popover().open && bell().getAttribute("aria-expanded") === "false", "Escape closes non-modal popover");
    assert(document.activeElement === bell(), "Escape returns focus to the bell button");

    // Outside pointerdown closes the popover.
    await React.act(async () => { bell().click(); });
    assert(popover().open, "popover opens before outside pointerdown");
    await React.act(async () => {
      outside.dispatchEvent(new pointerCtor("pointerdown", { bubbles: true, cancelable: true }));
    });
    assert(!popover().open && bell().getAttribute("aria-expanded") === "false", "outside pointerdown dismisses popover");

    // Disabled Mark all read & distinct empty-state copy on Unread vs All tabs.
    const markAll = Array.from(popover().querySelectorAll("button")).find((b) => b.textContent === "Mark all read") as HTMLButtonElement;
    assert(markAll.disabled === true, "Mark all read is disabled when unread_total === 0");
    assert(popover().textContent?.includes("You're all caught up."), "Unread tab empty state copy");
    const allTab = Array.from(popover().querySelectorAll("button")).find((b) => b.textContent === "All") as HTMLButtonElement;
    await React.act(async () => { allTab.click(); });
    assert(popover().textContent?.includes("No conversations yet.") && popover().textContent?.includes("Direct messages and mentions will appear here."), "All tab renders distinct empty state copy");

    // Traditional Chinese (zh-TW) localization renders translated UI strings.
    await React.act(async () => {
      root?.render(
        React.createElement(
          LocaleTextProvider,
          { value: (english: string) => translateUiText(english, "zh-TW") },
          React.createElement(NotificationToastProvider, null, React.createElement(NotificationInbox, { key: "zh", authEpoch: "ux-zh" })),
        ),
      );
    });
    await settle(() => (host.textContent?.includes("全部標示為已讀") ?? false), "zh-TW localized inbox renders");
    const zhText = host.textContent ?? "";
    assert(zhText.includes("通知") && zhText.includes("你的對話"), "zh-TW heading localized");
    assert(zhText.includes("全部標示為已讀") && zhText.includes("已讀完所有訊息。"), "zh-TW tools and empty state localized");
    const zhAllTab = Array.from(popover().querySelectorAll("button")).find((b) => b.textContent === "全部") as HTMLButtonElement;
    await React.act(async () => { zhAllTab.click(); });
    assert(popover().textContent?.includes("尚無對話通知。"), "zh-TW All tab empty state localized");

    console.log("notification inbox ux checks passed");
  } finally {
    const mounted = root;
    root = null;
    await React.act(async () => { mounted?.unmount(); resetPrivateSession(); });
    globalThis.fetch = originalFetch;
    loader.prototype.require = originalRequire;
    win.close();
  }
}

void main().catch((error: unknown) => {
  console.error(error);
  require<{ exitCode?: number }>("process").exitCode = 1;
});
