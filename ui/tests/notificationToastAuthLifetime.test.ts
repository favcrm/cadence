import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as AppModule from "../src/App";
import type * as LocaleModule from "../src/lib/locale";
import type * as ToastModule from "../src/features/threads/NotificationToast";
import type * as SessionKeyModule from "../src/lib/sessionKey";
import type { Meta } from "../src/lib/types";
import type { NotificationInbox as NotificationFeed } from "../src/features/threads/humanThreadsClient";
/**
 * Private notification toasts follow the admitted workspace.
 * An alert the operator can already see must disappear when verification
 * becomes unknown, must not return on the same admission, and must be gone
 * after a real sign-out. A later identity can open only its own alert.
 * The same-session issue draft stays concealed and is restored with the
 * workspace. Checks read the document, not provider placement.
 */
declare function require<T>(name: string): T;

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const BOARD = "/projects/cadence/issues?view=kanban";
const DRAFT = "Unsent same-tab toast draft";
const KEY = "toast-auth-tab";
const NEXT_KEY = "toast-auth-next";
const VAULT = "Vault Thread";
const LEDGER = "Ledger Thread";
const SIGNAL = "Signal Thread";
const HARBOR = "Harbor Thread";

type Dom = Window & Pick<typeof globalThis, "Node" | "Element" | "HTMLElement" | "HTMLInputElement" | "HTMLButtonElement" | "SVGElement" | "Event" | "MouseEvent" | "getComputedStyle"> & {
  happyDOM: { close: () => Promise<void> };
  setTimeout: (callback: (...args: unknown[]) => void, delay?: number, ...args: unknown[]) => number;
  clearTimeout: (id: number) => void;
  setInterval: (callback: (...args: unknown[]) => void, delay?: number, ...args: unknown[]) => number;
  clearInterval: (id: number) => void;
};
type ClockTimer = { due: number; interval: number | null; run: () => void };
type Probe = { done: boolean; answer: (response: Response) => void; fail: (error: Error) => void };
type Room = { room: string; title: string; seq: number };

function proof(id: string, sub: string): Meta {
  return {
    // CAD-1704: the notification inbox exists only on the hosted public
    // surface (serve.rs dispatches /api/human-threads inside public_host).
    // This fixture mocks that route, so it models that surface.
    hosted: true,
    anonymous_reads: false,
    read_only: false,
    signed_in: true,
    operator: true,
    actor: "operator",
    tailnet_url: null,
    version: "test",
    build_commit: "test",
    build_time: "2026-10-15T00:00:00Z",
    session: {
      id,
      origin: "loopback",
      created: 1_700_000_000,
      last_used: 1_700_000_000,
      idle_expires_at: 4_102_444_800,
      expires_at: 4_102_444_800,
      user_agent: "acceptance",
      user: { sub, handle: sub, name: "Operator", email: "operator@example.test", role: "operator" },
    },
  };
}

function feed(humanId: string, groups: Room[]): NotificationFeed {
  return {
    human: { id: humanId, name: "Operator" },
    unread_total: groups.length,
    groups: groups.map((group) => ({
      room: group.room,
      kind: "dm",
      title: group.title,
      unread: 1,
      muted: false,
      mention: false,
      alert_seq: group.seq,
    })),
    preferences: { dm: true, mention: true, sound: false, preview: true, quiet: "off", zone: "UTC" },
    push: { available: false, public_key: null },
  };
}

async function main(): Promise<void> {
  const { Window: BrowserWindow } = require<{ Window: new (options: { url: string }) => Dom }>("happy-dom");
  const win = new BrowserWindow({ url: `http://localhost${BOARD}` });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLButtonElement", "HTMLFormElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"] as const) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : Reflect.get(win, name), configurable: true, writable: true });
  }
  for (const name of ["addEventListener", "removeEventListener"] as const) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "getComputedStyle", { value: win.getComputedStyle.bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  const { setImmediate: nextTurn } = require<{ setImmediate: (callback: () => void) => void }>("node:timers");
  // Every timer stays on this clock. Zero-delay work settles on the event loop;
  // a positive delay runs only when the test advances the clock.
  let clock = 0;
  let timerSerial = 0;
  const clockTimers = new Map<number, ClockTimer>();
  const savedTimeout = win.setTimeout.bind(win);
  const savedClearTimeout = win.clearTimeout.bind(win);
  const savedInterval = win.setInterval.bind(win);
  const savedClearInterval = win.clearInterval.bind(win);
  const dueTimerIds = () => [...clockTimers.entries()]
    .filter(([, timer]) => timer.due <= clock)
    .sort((left, right) => left[1].due - right[1].due || left[0] - right[0])
    .map(([id]) => id);
  const takeDueTimer = (): ClockTimer | null => {
    const id = dueTimerIds()[0];
    if (id === undefined) return null;
    const timer = clockTimers.get(id);
    if (!timer) return null;
    if (timer.interval !== null && timer.interval > 0) clockTimers.set(id, { due: clock + timer.interval, interval: timer.interval, run: timer.run });
    else clockTimers.delete(id);
    return timer;
  };
  const scheduleTimer = (run: (...args: unknown[]) => void, delay = 0, args: unknown[], interval: number | null) => {
    const id = ++timerSerial;
    const wait = delay > 0 && Number.isFinite(delay) ? delay : 0;
    clockTimers.set(id, { due: clock + wait, interval, run: () => run(...args) });
    return id;
  };
  const cancelTimer = (id: number) => { clockTimers.delete(id); };
  const timerFn = (run: TimerHandler): ((...args: unknown[]) => void) => {
    if (typeof run !== "function") throw new Error("string timer handlers are not used");
    return (...args) => { Reflect.apply(run, undefined, args); };
  };
  win.setTimeout = (run, delay = 0, ...args) => scheduleTimer(timerFn(run), delay, args, null);
  win.clearTimeout = cancelTimer;
  win.setInterval = (run, delay = 0, ...args) => {
    const wait = delay > 0 && Number.isFinite(delay) ? delay : 0;
    return scheduleTimer(timerFn(run), wait, args, wait);
  };
  win.clearInterval = cancelTimer;
  for (const [name, value] of [
    ["setTimeout", win.setTimeout],
    ["clearTimeout", win.clearTimeout],
    ["setInterval", win.setInterval],
    ["clearInterval", win.clearInterval],
  ] as const) {
    Object.defineProperty(globalThis, name, { value, configurable: true });
  }
  Object.defineProperty(globalThis, "EventSource", {
    value: class {
      readyState = 0;
      addEventListener(): void {}
      close(): void {}
    },
    configurable: true,
  });
  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    if (id === "react-markdown") return { __esModule: true, default: () => null };
    if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
    return originalRequire.call(this, id);
  };
  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const AppView = require<typeof AppModule>("../src/App").default;
  const { LocaleProvider } = require<typeof LocaleModule>("../src/lib/locale");
  const { NotificationToastProvider } = require<typeof ToastModule>("../src/features/threads/NotificationToast");
  const { setSessionKey } = require<typeof SessionKeyModule>("../src/lib/sessionKey");

  const probes: Probe[] = [];
  const waiting: Array<(body: NotificationFeed) => void> = [];
  let holdNotifications = false;
  let nextFeed = feed("human-alpha", [
    { room: "vault-room", title: VAULT, seq: 1 },
    { room: "ledger-room", title: LEDGER, seq: 1 },
  ]);
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const fetchMock: typeof fetch = async (input, init) => {
    const raw = typeof input === "string" || input instanceof URL ? String(input) : input.url;
    const path = new URL(raw, "http://localhost").pathname;
    const method = (init?.method ?? "GET").toUpperCase();
    if (path === "/api/meta") {
      return new Promise<Response>((resolve, reject) => {
        const probe: Probe = {
          done: false,
          answer(response) { if (!probe.done) { probe.done = true; resolve(response); } },
          fail(error) { if (!probe.done) { probe.done = true; reject(error); } },
        };
        probes.push(probe);
        init?.signal?.addEventListener("abort", () => probe.fail(new Error("fixture metadata request aborted")), { once: true });
      });
    }
    if (path === "/api/human-threads/notifications") {
      if (!holdNotifications) return json(nextFeed);
      return new Promise<Response>((resolve) => {
        waiting.push((body) => resolve(json(body)));
      });
    }
    if (method === "POST" && path === "/api/session/logout") return json({ ok: true });
    if (path.endsWith("/stream")) {
      const signal = init?.signal ?? null;
      const body = new ReadableStream({
        start(controller) {
          const abort = () => { try { controller.error(new Error("fixture stream aborted")); } catch { /* already closed */ } };
          if (signal?.aborted) abort();
          else signal?.addEventListener("abort", abort, { once: true });
        },
      });
      return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
    }
    if (path === "/api/issues") return json({ issues: [{
      id: "CAD-1", project: "cadence", title: "Visible board record", status: "backlog", status_source: "file", priority: "P2",
      blocked_by: [], relates: [], refs: [], container: false, ready: true, blocked: false,
      created: "2026-10-11", rev: "fixture", counts: { comments: 0, artifacts: 0, refs: 0 }, checks: { done: 0, total: 0 },
    }] });
    if (path === "/api/projects") return json({ projects: [{ key: "cadence", prefix: "CAD", components: [], repos: [], issues: 1 }] });
    if (path === "/api/agents") return json({ agents: [], by_issue: {}, daemon: "reachable" });
    if (path === "/api/health") return json({ daemon: "reachable" });
    if (path === "/api/update/banner") return json(null);
    if (path === "/api/locale") return json({ organization_locale: null, user_locale: null });
    return json({ error: `fixture has no response for ${method} ${path}` }, 503);
  };
  globalThis.fetch = fetchMock;

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const href = () => `${location.pathname}${location.search}`;
  const drainDue = () => {
    for (let ran = 0; ran < 50; ran += 1) {
      const timer = takeDueTimer();
      if (!timer) return;
      timer.run();
    }
    throw new Error(`controlled clock did not settle (${dueTimerIds().length} due at ${clock}ms)`);
  };
  const flush = async () => {
    await React.act(async () => {
      for (let turn = 0; turn < 20; turn += 1) {
        await new Promise<void>(nextTurn);
        const pending = dueTimerIds().length;
        drainDue();
        if (pending === 0 && dueTimerIds().length === 0) return;
      }
    });
    if (dueTimerIds().length > 0) throw new Error(`event loop left ${dueTimerIds().length} due timers at ${clock}ms`);
  };
  const visible = (element: Element) => {
    for (let node: Element | null = element; node; node = node.parentElement) {
      const style = win.getComputedStyle(node);
      if (node.hasAttribute("hidden") || style.display === "none" || style.visibility === "hidden") return false;
    }
    return true;
  };
  const visibleElements = (selector: string) => Array.from(host.querySelectorAll(selector)).filter(visible);
  const buttons = (label: RegExp) => visibleElements("button").filter((button) => label.test(button.textContent?.trim() ?? ""));
  const authState = () => host.querySelector("[data-auth-state]")?.getAttribute("data-auth-state") ?? "";
  const visibleToasts = () => visibleElements(".notification-toast");
  const toastByTitle = (title: string) => visibleToasts().find((node) => node.textContent?.includes(title)) ?? null;
  const toastOpen = (title: string) => {
    const toast = toastByTitle(title);
    const button = toast?.querySelector("button.lnk") ?? null;
    return button instanceof HTMLButtonElement && button.textContent === "Open" ? button : null;
  };
  const visibleToastText = () => visibleToasts().map((node) => (node.textContent ?? "").replace(/\s+/g, " ").trim()).filter(Boolean).join(" | ");
  async function waitFor(check: () => boolean, message: string): Promise<void> {
    for (let attempt = 0; attempt < 40; attempt += 1) {
      await flush();
      if (check()) return;
    }
    throw new Error(message);
  }
  async function answerVerification(data: Meta): Promise<void> {
    for (let attempt = 0; attempt < 8; attempt += 1) {
      const active = probes.filter((probe) => !probe.done);
      if (active.length === 0) {
        await flush();
        if (probes.every((probe) => probe.done) && authState() === "ready") return;
        continue;
      }
      await React.act(async () => {
        for (const probe of active) probe.answer(json(data));
      });
      await flush();
    }
    assert(authState() === "ready", `admission did not become ready (${authState()})`);
  }
  async function failVerification(): Promise<void> {
    await waitFor(() => probes.some((probe) => !probe.done), "unknown access starts a real verification");
    const active = probes.filter((probe) => !probe.done);
    await React.act(async () => {
      for (const probe of active) probe.fail(new Error("temporary verification outage"));
    });
    await flush();
  }
  async function advance(milliseconds: number): Promise<void> {
    const until = clock + milliseconds;
    for (;;) {
      const nextDue = [...clockTimers.values()].reduce((soonest, timer) => Math.min(soonest, timer.due), Number.POSITIVE_INFINITY);
      if (nextDue > until) break;
      clock = nextDue;
      await React.act(async () => { drainDue(); });
      await flush();
    }
    clock = until;
    await flush();
  }
  async function releaseFeed(body: NotificationFeed): Promise<void> {
    nextFeed = body;
    const waiters = waiting.splice(0);
    await React.act(async () => {
      for (const resolve of waiters) resolve(body);
    });
    await flush();
  }

  sessionStorage.setItem("cadence.sessionKey", KEY);
  try {
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(NotificationToastProvider, null, React.createElement(AppView))));
    });
    await answerVerification(proof("toast-session-alpha", "operator-alpha"));
    await waitFor(() => toastByTitle(VAULT) !== null && toastByTitle(LEDGER) !== null, `admitted private rooms did not toast: ${visibleToastText()}`);
    assert(toastOpen(VAULT) !== null && toastOpen(LEDGER) !== null, "admitted toasts offer Open");
    assert(href() === BOARD, "an admitted toast does not navigate by itself");

    const ledger = toastByTitle(LEDGER);
    const dismiss = ledger?.querySelector('[aria-label="Dismiss notification"]') ?? null;
    assert(dismiss instanceof HTMLButtonElement, "admitted toast can be dismissed");
    await React.act(async () => { dismiss.click(); });
    await flush();
    assert(toastByTitle(LEDGER) === null, "dismiss removes the admitted toast");
    assert(toastByTitle(VAULT) !== null, "dismiss leaves the other admitted toast");
    assert(href() === BOARD, "dismiss does not navigate");

    const create = buttons(/^New issue$/)[0];
    assert(create instanceof HTMLButtonElement, "admitted operator can open an issue draft");
    await React.act(async () => { create.click(); });
    await flush();
    const draft = host.querySelector('form.board-new-issue input[name="title"]');
    assert(draft instanceof HTMLInputElement, "real issue draft opens");
    await React.act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      assert(setter, "the draft title is a real input");
      setter.call(draft, DRAFT);
      draft.dispatchEvent(new Event("input", { bubbles: true }));
    });
    assert(draft.value === DRAFT, "the same-session draft accepts text");

    holdNotifications = true;
    const beforeLoss = href();
    await React.act(async () => { window.dispatchEvent(new Event("focus")); });
    await failVerification();
    assert(authState() === "unavailable", `unknown access stays unverified (${authState()})`);
    assert(toastByTitle(VAULT) === null && toastOpen(VAULT) === null, `unknown access still shows the private toast: ${visibleToastText()}`);
    assert(visibleToasts().length === 0, `unknown access still shows a notification action: ${visibleToastText()}`);
    assert(href() === beforeLoss, "unknown access does not open a private thread");
    assert(draft.value === DRAFT && !visible(draft), "unknown access conceals the same-session draft without dropping it");

    const retry = buttons(/retry/i)[0];
    assert(retry instanceof HTMLButtonElement, "verification recovery is available");
    await React.act(async () => { retry.click(); });
    await answerVerification(proof("toast-session-alpha", "operator-alpha"));
    await waitFor(() => waiting.length > 0, "same-session re-admission reloads notifications through the boundary");
    const restored = host.querySelector('form.board-new-issue input[name="title"]');
    assert(restored instanceof HTMLInputElement && visible(restored) && restored.value === DRAFT, "same-session re-admission restores the draft");
    assert(toastByTitle(VAULT) === null && toastByTitle(LEDGER) === null, `re-admission resurrected a retired toast: ${visibleToastText()}`);
    assert(href() === beforeLoss, "re-admission does not follow a retired toast");
    await advance(7_000);
    assert(toastByTitle(VAULT) === null && toastByTitle(LEDGER) === null && visibleToasts().length === 0, "a retired alert timer does not restore the private toast");
    assert(href() === beforeLoss, "a retired alert timer does not navigate");

    await releaseFeed(feed("human-alpha", [{ room: "signal-room", title: SIGNAL, seq: 1 }]));
    await waitFor(() => toastByTitle(SIGNAL) !== null, "a new admitted alert still toasts");
    assert(toastByTitle(VAULT) === null && toastByTitle(LEDGER) === null, "a new alert does not restore a retired room");
    const account = visibleElements("button").find((button) => (button.getAttribute("aria-label") ?? "").startsWith("Account menu"));
    assert(account instanceof HTMLButtonElement, "admitted operator can open the account menu");
    await React.act(async () => { account.click(); });
    await flush();
    const signOut = buttons(/^Sign out$/)[0];
    assert(signOut instanceof HTMLButtonElement, "admitted operator can sign out");
    await React.act(async () => { signOut.click(); });
    await flush();
    await waitFor(
      () => authState() !== "ready" && host.querySelector("[data-app-shell]") === null && host.querySelector(".notification-toast") === null,
      `confirmed sign-out did not leave the gate (${authState()} shell=${host.querySelector("[data-app-shell]") !== null} toast=${host.querySelector(".notification-toast") !== null})`,
    );
    assert(host.querySelector("form.board-new-issue") === null, "confirmed sign-out clears the private draft");
    assert(href() === beforeLoss, "confirmed sign-out does not open a private thread");
    await advance(7_000);
    assert(host.querySelector(".notification-toast") === null, "a retired alert timer restores a toast after sign-out");
    assert(href() === beforeLoss, "a retired alert timer navigates after sign-out");

    holdNotifications = true;
    await React.act(async () => { setSessionKey(NEXT_KEY); });
    await flush();
    await answerVerification(proof("toast-session-harbor", "operator-harbor"));
    await waitFor(() => waiting.length > 0, "the next identity loads notifications only after admission");
    assert(host.textContent?.includes(VAULT) !== true && host.textContent?.includes(LEDGER) !== true && host.textContent?.includes(SIGNAL) !== true, "the next identity shows a retired room before its own feed");
    await releaseFeed(feed("human-harbor", [{ room: "harbor-room", title: HARBOR, seq: 1 }]));
    await waitFor(() => toastOpen(HARBOR) !== null, "the admitted next identity shows its own toast");
    assert(toastByTitle(VAULT) === null && toastByTitle(SIGNAL) === null, "the next identity does not restore an old alert");
    const open = toastOpen(HARBOR);
    assert(open instanceof HTMLButtonElement, "the admitted toast Open action is available");
    await React.act(async () => { open.click(); });
    await flush();
    assert(href() === "/threads?h=harbor-room", `Open follows the admitted room (${href()})`);
    assert(toastByTitle(HARBOR) === null, "Open dismisses the admitted toast");
    assert(!href().includes("vault-room") && !href().includes("signal-room") && !href().includes("ledger-room"), "Open does not follow a retired room");
    console.log("notification toast auth lifetime: private toast concealment passed");
  } finally {
    clockTimers.clear();
    await React.act(async () => { root.unmount(); });
    win.setTimeout = savedTimeout;
    win.clearTimeout = savedClearTimeout;
    win.setInterval = savedInterval;
    win.clearInterval = savedClearInterval;
    loader.prototype.require = originalRequire;
    await win.happyDOM.close();
  }
}

void main().catch((error: unknown) => {
  console.error(error);
  require<{ exitCode?: number }>("process").exitCode = 1;
});
