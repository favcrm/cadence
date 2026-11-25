/** CAD-2810: Channel notification UX — persistent alert dedupe, Bell/rich rows/redaction, route focus auto-clear, sidebar badge + tab title, background desktop alerts, and in-room jump pill. */
declare const require: (<T = unknown>(name: string) => T) & {
  resolve: (id: string) => string;
  cache: Record<string, unknown>;
};
declare const process: { exit(code: number): never };

import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as NavigationModule from "../screen-ui/src/navigation";
import type * as NotificationInboxModule from "../src/features/threads/NotificationInbox";
import type * as NotificationToastModule from "../src/features/threads/NotificationToast";
import type * as ChannelsUnreadModule from "../src/features/threads/channelsUnread";
import type * as NavListModule from "../src/ui/NavList";
import type * as WhatsAppUnreadProbesModule from "../src/features/app-shell/WhatsAppUnreadProbes";
import type * as WhatsAppUnreadModule from "../src/features/app-shell/whatsappUnread";
import type * as HumanThreadsModule from "../src/features/threads/HumanThreads";
import type * as ClientModule from "../src/features/threads/humanThreadsClient";
import type * as LocationModule from "../src/lib/useLocation";
import type * as LocaleModule from "../src/lib/locale";
import type * as SessionModule from "../src/lib/privateSession";
import type * as SessionKeyModule from "../src/lib/sessionKey";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

async function nextTurn(): Promise<void> {
  await Promise.resolve();
  await new Promise<void>((resolve) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = () => {
      channel.port1.close();
      channel.port2.close();
      resolve();
    };
    channel.port2.postMessage(null);
  });
}

async function flush(turns = 8): Promise<void> {
  for (let i = 0; i < turns; i += 1) {
    await nextTurn();
  }
}

async function main(): Promise<void> {
  const { Window } = require<{ Window: new (options: { url: string }) => Record<string, any> }>("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  win.document.title = "cadence board";

  let visibilityState: "visible" | "hidden" = "visible";
  Object.defineProperty(win.document, "visibilityState", {
    get: () => visibilityState,
    configurable: true,
  });

  const constructedDesktopAlerts: Array<{
    title: string;
    options: { body?: string; tag?: string } | undefined;
    closed: boolean;
    onclick: (() => void) | null;
    onclose: (() => void) | null;
    close: () => void;
  }> = [];

  class MockNotification {
    static permission: NotificationPermission = "default";
    static requestPermission = async (): Promise<NotificationPermission> => {
      MockNotification.permission = "granted";
      return "granted";
    };
    title: string;
    options: { body?: string; tag?: string } | undefined;
    closed = false;
    onclick: (() => void) | null = null;
    onclose: (() => void) | null = null;

    constructor(title: string, options?: { body?: string; tag?: string }) {
      this.title = title;
      this.options = options;
      constructedDesktopAlerts.push(this);
    }

    close = () => {
      this.closed = true;
      this.onclose?.();
    };
  }
  win.Notification = MockNotification;

  let chimesPlayed = 0;
  class MockAudioContext {
    state = "running";
    currentTime = 0;
    destination = {};
    resume = async () => undefined;
    createOscillator() {
      chimesPlayed += 1;
      return {
        type: "sine",
        frequency: { setValueAtTime: () => undefined },
        connect: () => undefined,
        start: () => undefined,
        stop: () => undefined,
      };
    }
    createGain() {
      return {
        gain: {
          setValueAtTime: () => undefined,
          exponentialRampToValueAtTime: () => undefined,
        },
        connect: () => undefined,
      };
    }
  }
  win.AudioContext = MockAudioContext;

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
    "StorageEvent",
    "Notification",
    "AudioContext",
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
  Object.defineProperty(globalThis, "setInterval", { value: () => 1, configurable: true });
  Object.defineProperty(globalThis, "clearInterval", { value: () => undefined, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  win.HTMLElement.prototype.scrollIntoView = () => undefined;
  win.HTMLDialogElement.prototype.show = function (this: { open: boolean }) { this.open = true; };
  win.HTMLDialogElement.prototype.showModal = function (this: { open: boolean }) { this.open = true; };
  win.HTMLDialogElement.prototype.close = function (this: { open: boolean }) { this.open = false; };

  const loader = require<{
    prototype: { require: (this: unknown, id: string) => unknown };
  }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") {
      return new Proxy({}, { get: (_target, prop) => ({ __iconName: String(prop) }) });
    }
    if (id === "@hugeicons/react") {
      return {
        HugeiconsIcon: (props: { icon?: { __iconName?: string } }) =>
          React.createElement("span", { "data-hugeicon": props.icon?.__iconName ?? "" }),
      };
    }
    if (id === "react-markdown") return { __esModule: true, default: () => null };
    if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
    return originalRequire.call(this, id);
  };

  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const { NavigationProvider } = require<typeof NavigationModule>("../screen-ui/src/navigation");
  const { LocaleProvider } = require<typeof LocaleModule>("../src/lib/locale");
  const { NotificationToastProvider } = require<typeof NotificationToastModule>("../src/features/threads/NotificationToast");
  const { useChannelsUnread } = require<typeof ChannelsUnreadModule>("../src/features/threads/channelsUnread");
  const { NavList } = require<typeof NavListModule>("../src/ui/NavList");
  const { WhatsAppUnreadProbes } = require<typeof WhatsAppUnreadProbesModule>("../src/features/app-shell/WhatsAppUnreadProbes");
  const { setWhatsAppUnreadSnapshot, clearWhatsAppUnreadSnapshot } = require<typeof WhatsAppUnreadModule>("../src/features/app-shell/whatsappUnread");
  const { HumanRoomTimeline } = require<typeof HumanThreadsModule>("../src/features/threads/HumanThreads");
  const { navigate } = require<typeof LocationModule>("../src/lib/useLocation");
  const { allowPrivateReads, resetPrivateSession } = require<typeof SessionModule>("../src/lib/privateSession");
  const { setSessionKey } = require<typeof SessionKeyModule>("../src/lib/sessionKey");
  let NotificationInbox = require<typeof NotificationInboxModule>("../src/features/threads/NotificationInbox").default;

  const HUMAN_ID = "human-secret-1";
  const ROOM_ID = "room-secret-alpha";

  let inboxStatus = 200;
  let alertSeq = 5;
  let unreadTotal = 2;
  let previewEnabled = true;
  let soundEnabled = true;
  let latestPreview: string | null = "PR #2647 ready for review";
  let roomPosts: ClientModule.HumanPost[] = [
    {
      id: "post-1",
      read_cursor: "csc1_post1",
      sender: { id: "human-secret-1", label: "Mei" },
      body: "Checking notification flow",
      created: 1700000100,
      silent: false,
      outcomes: [],
    },
    {
      id: "post-2",
      read_cursor: "csc1_post2",
      sender: { id: "ag-dev", label: "dev", kind: "agent" },
      body: "Initial channel reply",
      created: 1700000200,
      silent: false,
      outcomes: [],
    },
  ];

  const json = (data: unknown, status = 200) =>
    new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json" } });

  globalThis.fetch = (async (input: unknown) => {
    const url = new URL(String(input), "http://localhost");
    if (url.pathname === "/api/human-threads/stream") return new Response("", { status: 404 });
    if (url.pathname === "/api/human-threads/notifications") {
      if (inboxStatus !== 200) return json({ error: "unauthorized" }, inboxStatus);
      const payload: ClientModule.NotificationInbox = {
        human: { id: HUMAN_ID, name: "Mei" },
        unread_total: unreadTotal,
        groups: [
          {
            room: ROOM_ID,
            kind: "mixed_group",
            title: "Launch Ops",
            unread: unreadTotal,
            muted: false,
            mention: true,
            alert_seq: alertSeq,
            latest_sender: "scribe",
            latest_sender_kind: "agent",
            latest_preview: previewEnabled ? latestPreview : null,
            latest_at: 1700000250,
          },
        ],
        preferences: {
          dm: true,
          mention: true,
          sound: soundEnabled,
          preview: previewEnabled,
          quiet: "off",
          zone: "UTC",
        },
        push: { available: false, public_key: null },
      };
      return json(payload);
    }
    if (url.pathname === "/api/human-threads/rooms/room-jump") {
      const page: ClientModule.HumanRoomPage = {
        room: "room-jump",
        scope_id: "csrid_jump",
        members: [
          { kind: "human", id: HUMAN_ID, label: "Mei" },
          { kind: "agent", id: "ag-dev", label: "dev", look: "builder" },
        ],
        posts: roomPosts,
        events: [],
        before: "csc1_before",
        more_before: false,
        can_ask: true,
        can_manage: false,
        agent_chain_count: 0,
        agent_chain_limit: 4,
      };
      return json(page);
    }
    if (url.pathname === "/api/human-threads/agents") {
      return json({ agents: [{ uid: "ag-dev", alias: "dev", look: "builder" }] });
    }
    if (url.pathname === "/api/agents") {
      return json({ daemon: "reachable", agents: [], totals: null, by_issue: {} });
    }
    return json({});
  }) as typeof fetch;

  setSessionKey("channel-notif-ux-test");
  allowPrivateReads();

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);

  function ChannelsCountProbe() {
    const n = useChannelsUnread();
    return React.createElement("span", { "data-channels-unread": String(n) });
  }

  function Harness({ epoch }: { epoch: string }) {
    return React.createElement(NavigationProvider, {
      adapter: {
        navigate,
        href: (href) => href,
        modifiedClicks: "browser",
      },
      children: React.createElement(
        LocaleProvider,
        null,
        React.createElement(
          NotificationToastProvider,
          null,
          React.createElement(NotificationInbox, { authEpoch: epoch }),
          React.createElement(ChannelsCountProbe),
          React.createElement(NavList, {
            screen: "home",
            navHref: (route) => (route.screen === "home" ? "/" : `/${route.screen}`),
          }),
          React.createElement(WhatsAppUnreadProbes, {
            apps: [],
            resolveHref: () => null,
          }),
        ),
      ),
    });
  }

  const toastCards = () =>
    Array.from(document.querySelectorAll<HTMLButtonElement>("button.lnk"))
      .filter((btn) => btn.textContent?.trim() === "Open")
      .map((btn) => btn.parentElement!)
      .filter(Boolean);

  const renderHarness = async (epoch: string) => {
    await React.act(async () => {
      root.render(React.createElement(Harness, { epoch }));
    });
    await flush();
  };

  // 1. Initial load: Bell icon, rich toast + sound chime, and hashed localStorage watermarks.
  await renderHarness("ep-1");

  const bellIcon = host.querySelector("button.bell-button [data-hugeicon]");
  assert(bellIcon?.getAttribute("data-hugeicon") === "BellIcon", "Header trigger uses BellIcon instead of threads bubble");
  equal(host.querySelector("button.bell-button .notification-count")?.textContent, "2", "Bell displays unread count 2");

  const initialToasts = toastCards();
  equal(initialToasts.length, 1, "Initial inbox poll shows 1 toast");
  assert(
    initialToasts[0].textContent?.includes("New mention in Launch Ops") &&
      initialToasts[0].textContent?.includes("@scribe: PR #2647 ready for review"),
    "Toast displays title and @agent sender + preview when preview preference is on",
  );
  assert(chimesPlayed >= 1, "Web Audio chime synthesizes tones when preferences.sound is true");
  const chimesAfterFirst = chimesPlayed;

  const storageEntries: Array<[string, string]> = [];
  for (let i = 0; i < win.localStorage.length; i += 1) {
    const k = win.localStorage.key(i);
    if (k && k.startsWith("cadence.notif.announced.v1:")) {
      storageEntries.push([k, win.localStorage.getItem(k) ?? ""]);
    }
  }
  equal(storageEntries.length, 1, "Persists per-human announced watermark key in localStorage");
  assert(
    !storageEntries[0][0].includes(HUMAN_ID) && !storageEntries[0][1].includes(ROOM_ID),
    "Stored watermark keys and values hash both human.id and room id",
  );

  // 2. Popover rich metadata row (@Mention, Group channel, timestamp, sender preview) + sidebar badge + unified tab title.
  const bellBtn = host.querySelector<HTMLButtonElement>("button.bell-button")!;
  await React.act(async () => {
    bellBtn.click();
  });
  await flush(4);
  const popover = host.querySelector<HTMLElement>("dialog.notification-popover")!;
  const rowText = popover.querySelector(".notification-row")?.textContent ?? "";
  assert(rowText.includes("Group channel"), "Popover row labels non-DM thread as Group channel");
  assert(popover.querySelector(".chip-mention")?.textContent === "@Mention", "Popover row shows @Mention badge");
  assert(
    popover.querySelector(".notification-preview")?.textContent === "@scribe: PR #2647 ready for review",
    "Popover row shows sender-prefixed preview snippet",
  );
  const rowTime = popover.querySelector(".notification-time")?.textContent?.trim() ?? "";
  assert(rowTime.length > 0 && rowTime !== "Time unavailable", "Popover row renders formatted timestamp from Unix seconds");
  const markAllBtn = Array.from(popover.querySelectorAll<HTMLButtonElement>("button.lnk"))
    .find((b) => b.textContent?.trim() === "Mark all read");
  assert(
    Boolean(markAllBtn?.getAttribute("title")?.includes("without marking channel history read")),
    "Mark all read explains its scope via title attribute",
  );

  const threadsBadge = host.querySelector('a[href="/threads"] .navbadge.navbadge-accent');
  equal(threadsBadge?.textContent, "2", "Sidebar Channels row renders numeric accent unread badge");
  equal(win.document.title, "(2) cadence board", "Tab title prefixes total unread Channels count");

  setWhatsAppUnreadSnapshot("wa-1", {
    provider: "linked",
    available: true,
    totalUnread: 3,
    unreadThreads: 1,
    latest: null,
  });
  await flush(4);
  equal(win.document.title, "(5) cadence board", "Tab title combines WhatsApp unread (3) + Channels unread (2)");
  clearWhatsAppUnreadSnapshot("wa-1");
  await flush(4);
  equal(win.document.title, "(2) cadence board", "Tab title drops WhatsApp count cleanly when cleared");

  // 3. Reload simulation: unmount, clear NotificationInbox module cache (in-memory `announced`), remount with same alert_seq=5.
  await React.act(async () => {
    root.render(null);
  });
  await flush(4);
  const inboxPath = require.resolve("../src/features/threads/NotificationInbox");
  delete require.cache[inboxPath];
  NotificationInbox = require<typeof NotificationInboxModule>("../src/features/threads/NotificationInbox").default;
  await renderHarness("ep-1");
  equal(toastCards().length, 0, "Reload with same alert_seq suppresses duplicate toast via localStorage watermark");
  equal(chimesPlayed, chimesAfterFirst, "Reload suppression does not replay audio chime");

  // 4. Bumping alert_seq with preview=false surfaces generic toast & redacts popover snippet; navigating to room clears toast.
  alertSeq = 6;
  previewEnabled = false;
  soundEnabled = false;
  latestPreview = "Sensitive body that must stay hidden";
  await renderHarness("ep-2");
  const redactedToasts = toastCards();
  equal(redactedToasts.length, 1, "Newer alert_seq=6 surfaces toast after reload");
  assert(
    !redactedToasts[0].textContent?.includes("Sensitive body") &&
      redactedToasts[0].textContent?.includes("Unread until you open it."),
    "preview=false redacts message text from toast",
  );
  equal(chimesPlayed, chimesAfterFirst, "sound=false suppresses audio chime");
  assert(
    host.querySelector("dialog.notification-popover .notification-preview") === null,
    "preview=false redacts message snippet from popover rows",
  );

  await React.act(async () => {
    navigate(`/threads?h=${encodeURIComponent(ROOM_ID)}`);
  });
  await flush(4);
  equal(toastCards().length, 0, "Navigating to the room automatically clears its lingering toast");

  // Return to Home for background desktop notification checks.
  await React.act(async () => {
    navigate("/");
  });
  await flush(4);

  // 5. Desktop Notification API fallback when push.available === false.
  const settingsBtn = host.querySelector<HTMLButtonElement>('button[aria-label="Notification settings"]')!;
  await React.act(async () => {
    settingsBtn.click();
  });
  await flush(4);
  const settingsDialog = host.querySelector<HTMLElement>("dialog.notification-settings")!;
  assert(
    settingsDialog.textContent?.includes("Alert me while this tab is in the background."),
    "Settings shows background-tab browser alert copy when Web Push is absent",
  );
  const enableDesktopBtn = Array.from(settingsDialog.querySelectorAll<HTMLButtonElement>("button.lnk"))
    .find((b) => b.textContent?.trim() === "Enable browser alerts")!;
  await React.act(async () => {
    enableDesktopBtn.click();
  });
  await flush(6);
  equal(win.localStorage.getItem("cadence.desktop_alerts.v1"), "on", "Opt-in sets cadence.desktop_alerts.v1=on");
  assert(
    settingsDialog.textContent?.includes("On for open tabs in the background."),
    "Settings updates copy after permission is granted",
  );

  visibilityState = "hidden";
  alertSeq = 7;
  previewEnabled = true;
  latestPreview = "Background ping";
  await renderHarness("ep-3");
  equal(constructedDesktopAlerts.length, 1, "Background tab fires Desktop Notification when alert_seq advances");
  equal(constructedDesktopAlerts[0].title, "New mention in Launch Ops", "Desktop Notification uses localized title");
  equal(constructedDesktopAlerts[0].options?.body, "@scribe: Background ping", "Desktop Notification carries preview when enabled");

  visibilityState = "visible";
  await React.act(async () => {
    navigate(`/threads?h=${encodeURIComponent(ROOM_ID)}`);
    win.document.dispatchEvent(new Event("visibilitychange"));
  });
  await flush(4);
  assert(constructedDesktopAlerts[0].closed, "Focusing active room closes its open Desktop Notification");

  const reopenSettingsBtn = host.querySelector<HTMLButtonElement>('button[aria-label="Notification settings"]')!;
  await React.act(async () => {
    reopenSettingsBtn.click();
  });
  await flush(4);
  const liveSettingsDialog = host.querySelector<HTMLElement>("dialog.notification-settings")!;
  const disableDesktopBtn = Array.from(liveSettingsDialog.querySelectorAll<HTMLButtonElement>("button.lnk"))
    .find((b) => b.textContent?.trim() === "Turn off browser alerts")!;
  await React.act(async () => {
    disableDesktopBtn.click();
  });
  await flush(4);
  equal(win.localStorage.getItem("cadence.desktop_alerts.v1"), "off", "Disabling browser alerts flips storage key to off");

  // 6. 401 and resetPrivateSession clear stored watermarks, Channels badge, and tab title.
  inboxStatus = 401;
  await renderHarness("ep-4");
  let remainingKeys = 0;
  for (let i = 0; i < win.localStorage.length; i += 1) {
    if (win.localStorage.key(i)?.startsWith("cadence.notif.announced.v1:")) remainingKeys += 1;
  }
  equal(remainingKeys, 0, "401 removes stored announced watermarks from localStorage");
  assert(host.querySelector('a[href="/threads"] .navbadge') === null, "401 removes sidebar Channels badge");
  equal(win.document.title, "cadence board", "401 resets document.title back to base title");

  inboxStatus = 200;
  alertSeq = 8;
  unreadTotal = 1;
  await React.act(async () => {
    navigate("/");
  });
  await renderHarness("ep-5");
  equal(win.document.title, "(1) cadence board", "Re-establishing session updates unread badge and title");
  await React.act(async () => {
    resetPrivateSession();
  });
  await flush(4);
  equal(win.document.title, "cadence board", "resetPrivateSession resets document.title");

  // 7. HumanRoomTimeline scrolled-up floating jump-to-latest pill.
  allowPrivateReads();
  let refresh = 0;
  const renderRoom = async () => {
    await React.act(async () => {
      root.render(
        React.createElement(
          LocaleProvider,
          null,
          React.createElement(HumanRoomTimeline, {
            id: "room-jump",
            me: { id: HUMAN_ID, name: "Mei" },
            refresh,
            onState: () => undefined,
            onOpenContext: () => undefined,
            detailsOpen: false,
            onInvite: () => undefined,
            onChanged: () => undefined,
            isDm: false,
            roomTitle: "Jump Room",
            people: [{ id: HUMAN_ID, subject: "sub-me", name: "Mei" }],
          }),
        ),
      );
    });
    await flush(8);
  };

  await renderRoom();
  const scrollBox = host.querySelector<HTMLDivElement>(".threads-messages")!;
  Object.defineProperty(scrollBox, "scrollHeight", { value: 1200, configurable: true });
  Object.defineProperty(scrollBox, "clientHeight", { value: 400, configurable: true });
  scrollBox.scrollTop = 0;
  await React.act(async () => {
    scrollBox.dispatchEvent(new Event("scroll"));
  });
  await flush(4);

  roomPosts = [
    ...roomPosts,
    {
      id: "post-3",
      read_cursor: "csc1_post3",
      sender: { id: "ag-dev", label: "dev", kind: "agent" },
      body: "New agent update 1",
      created: 1700000300,
      silent: false,
      outcomes: [],
    },
    {
      id: "post-4",
      read_cursor: "csc1_post4",
      sender: { id: HUMAN_ID, label: "Mei" },
      body: "Self message should not increment jump pill",
      created: 1700000320,
      silent: false,
      outcomes: [],
    },
    {
      id: "post-5",
      read_cursor: "csc1_post5",
      sender: { id: "ag-dev", label: "dev", kind: "agent" },
      body: "New agent update 2",
      created: 1700000340,
      silent: false,
      outcomes: [],
    },
  ];
  refresh = 1;
  await renderRoom();

  const jumpBtn = host.querySelector<HTMLButtonElement>("button.threads-jump-pill");
  assert(jumpBtn?.textContent?.includes("2 new messages"), "Scrolled-up room displays count of incoming non-self messages");
  await React.act(async () => {
    jumpBtn!.click();
  });
  await flush(4);
  equal(scrollBox.scrollTop, 800, "Clicking jump pill scrolls timeline to latest bottom");
  assert(host.querySelector("button.threads-jump-pill") === null, "Jump pill dismisses after jumping to latest");

  await React.act(async () => {
    root.unmount();
  });
  console.log("channel notifications UX checks passed");
}

main()
  .then(() => process.exit(0))
  .catch((err) => {
    console.error(err);
    process.exit(1);
  });
