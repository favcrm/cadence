import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as InboxModule from "../src/features/threads/NotificationInbox";
import type * as ToastModule from "../src/features/threads/NotificationToast";
import type * as ClientModule from "../src/features/threads/humanThreadsClient";
import type * as SessionModule from "../src/lib/privateSession";

declare function require<T>(name: string): T;

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

async function main(): Promise<void> {
  type Dom = Window & Pick<typeof globalThis, "Node" | "Element" | "HTMLElement" | "SVGElement" | "Event">;
  const { Window: BrowserWindow } = require<{ Window: new (options: { url: string }) => Dom }>("happy-dom");
  const win = new BrowserWindow({ url: "http://localhost/" });
  Object.defineProperties(globalThis, {
    window: { value: win, configurable: true }, document: { value: win.document, configurable: true },
    navigator: { value: win.navigator, configurable: true }, location: { value: win.location, configurable: true },
    history: { value: win.history, configurable: true }, sessionStorage: { value: win.sessionStorage, configurable: true },
    localStorage: { value: win.localStorage, configurable: true }, Node: { value: win.Node, configurable: true },
    Element: { value: win.Element, configurable: true }, HTMLElement: { value: win.HTMLElement, configurable: true },
    SVGElement: { value: win.SVGElement, configurable: true }, Event: { value: win.Event, configurable: true },
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
  const { flushSync } = require<{ flushSync: (action: () => void) => void }>("react-dom");
  const NotificationInbox = require<typeof InboxModule>("../src/features/threads/NotificationInbox").default;
  const { NotificationToastProvider } = require<typeof ToastModule>("../src/features/threads/NotificationToast");
  const clients = require<typeof ClientModule>("../src/features/threads/humanThreadsClient");
  const { pausePrivateReads, allowPrivateReads, resetPrivateSession } = require<typeof SessionModule>("../src/lib/privateSession");
  const originalMark = clients.markNotificationsRead;
  const originalMute = clients.setRoomMuted;
  const { setImmediate: nextTurn } = require<{ setImmediate: (callback: () => void) => void }>("node:timers");
  const flush = () => React.act(async () => { await new Promise<void>(nextTurn); });
  async function settle(check: () => boolean, message: string) {
    for (let attempt = 0; attempt < 40; attempt += 1) {
      await flush();
      if (check()) return;
    }
    throw new Error(message);
  }
  const feed: ClientModule.NotificationInbox = {
    human: { id: "human-synthetic", name: "Synthetic reviewer" }, unread_total: 1,
    groups: [{ room: "room-synthetic", kind: "dm", title: "Synthetic private room", unread: 1, muted: false, mention: false, alert_seq: 0 }],
    preferences: { dm: true, mention: true, sound: false, preview: true, quiet: "off", zone: "UTC" },
    push: { available: false, public_key: null },
  };
  const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
  let root: Root | null = null;
  let retired = false;
  let freshReads = 0;
  let abandonedReads = 0;
  let writes = 0;
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = new URL(input instanceof Request ? input.url : String(input), "http://localhost").pathname;
    if (path === "/api/human-threads/notifications") {
      if (retired) abandonedReads += 1;
      else freshReads += 1;
      return json(feed);
    }
    if (init?.method === "POST" && (path.endsWith("/notifications/read") || path.endsWith("/mute"))) {
      writes += 1;
      return json({ unread_total: 0, muted: true });
    }
    throw new Error(`Unexpected notification acceptance request ${path}`);
  };
  // Observe actual successful client settlement before the consumer's continuation;
  // forward the same promise unchanged while a genuine session/consumer retirement wins.
  function retireAfter<T>(pending: Promise<T>): Promise<T> {
    void pending.then(() => {
      retired = true;
      pausePrivateReads();
      const mounted = root;
      if (mounted !== null) flushSync(() => mounted.unmount());
      root = null;
      allowPrivateReads();
    });
    return pending;
  }
  clients.markNotificationsRead = () => retireAfter(originalMark());
  clients.setRoomMuted = (room, muted) => retireAfter(originalMute(room, muted));
  const host = document.createElement("div");
  document.body.append(host);
  try {
    for (const action of ["Mark all read", "Mute"]) {
      resetPrivateSession();
      allowPrivateReads();
      retired = false;
      abandonedReads = 0;
      const beforeReads = freshReads;
      const beforeWrites = writes;
      root = createRoot(host);
      await React.act(async () => {
        root?.render(React.createElement(NotificationToastProvider, null,
          React.createElement(NotificationInbox, { authEpoch: "synthetic-current" })));
      });
      await settle(() => Array.from(host.querySelectorAll("button")).some((button) => button.textContent === action), "the actual notification inbox renders its current action");
      assert(freshReads > beforeReads, "current admitted consumer loads the real feed");
      const button = Array.from(host.querySelectorAll("button")).find((item) => item.textContent === action);
      assert(button, "notification action is available");
      await React.act(async () => { button.click(); });
      await settle(() => writes > beforeWrites && root === null, "actual client mutation settles and old consumer retires before its continuation");
      await flush();
      assert(abandonedReads === 0, `${action} cannot start an abandoned follow-up feed read after re-admission`);
      assert(!host.textContent?.includes("Synthetic private room"), "retired consumer leaves no private room DOM");
    }
    console.log("notification follow-up pause checks passed");
  } finally {
    const mounted = root;
    await React.act(async () => { mounted?.unmount(); resetPrivateSession(); });
    clients.markNotificationsRead = originalMark;
    clients.setRoomMuted = originalMute;
    loader.prototype.require = originalRequire;
    win.close();
  }
}

void main().catch((error: unknown) => {
  console.error(error);
  require<{ exitCode?: number }>("process").exitCode = 1;
});
