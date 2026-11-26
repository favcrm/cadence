import { streamFixture } from "./streamFixture";
import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as HomeModule from "../src/features/home/Home";
import type * as Locale from "../src/lib/locale";
import type * as Stores from "../src/lib/resources";
import type { ThreadEntry } from "../src/features/home/thread";
import type * as Send from "../src/features/home/send";
import type * as Decisions from "../src/features/decisions/useDecisions";
declare function require<T>(name: string): T;
const { Window } = require<typeof HappyDOM>("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "Event", "MouseEvent", "MutationObserver", "ResizeObserver", "location", "history", "localStorage", "sessionStorage"] as const)
  Object.defineProperty(globalThis, key, { value: key === "window" ? win : win[key], configurable: true });
for (const key of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
  Object.defineProperty(globalThis, key, { value: win[key].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react" || id === "remark-gfm") return { HugeiconsIcon: () => null };
  if (id === "react-markdown") return { __esModule: true, default: () => null };
  return originalRequire.call(this, id);
};
const React = require<typeof ReactModule>("react");
const { createRoot } = require<typeof ReactDOM>("react-dom/client");
const Home = require<typeof HomeModule>("../src/features/home/Home").default;
const { LocaleProvider } = require<typeof Locale>("../src/lib/locale");
const { resources } = require<typeof Stores>("../src/lib/resources");
const { sendToMaster } = require<typeof Send>("../src/features/home/send");
const { setImmediate: nextTurn } = require<{ setImmediate: () => Promise<void> }>("node:timers/promises");
// CAD-2012: history must never carry the rise-in class, not even on the first
// loaded render, whose class a later render removes before any DOM read.
const classesEverSet: string[] = [];
{
  const proto = win.Element.prototype as unknown as { setAttribute(name: string, value: string): void };
  const setAttribute = proto.setAttribute;
  proto.setAttribute = function (name: string, value: string) {
    if (name === "class") classesEverSet.push(String(value));
    return setAttribute.call(this, name, value);
  };
  const className = Object.getOwnPropertyDescriptor(win.Element.prototype, "className");
  if (className?.set) {
    const set = className.set;
    Object.defineProperty(win.Element.prototype, "className", { ...className, set(value: string) { classesEverSet.push(String(value)); set.call(this, value); } });
  }
}
// A stream that has opened but has not yet sent its first frame.
let holdStream = false;
// What the state read answers, when a case needs it to agree with the stream.
let servedState: unknown = null;
// What a hosted board's state read answers while a case holds its working flag.
let hostedStateRead: unknown = null;
let stagedActions: Record<string, unknown>[] = [];
const posts: Record<string, unknown>[] = [];
// The stream's own master frame: the fixture emits it on open, so a turn the
// test wants the stream to report must be in this object, not only the store.
const streamMaster: Record<string, unknown> = {
  alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true,
  identity: { name: null, tone: null, look: "robot", revision: "fixture" },
  turn: null, queue: [],
};
const commands: { command?: string; message?: string }[] = [];
const refs = [{ kind: "issue", id: "CAD-B" }];
const attachments = [{ id: "file-b" }];
const entries: ThreadEntry[] = [{ seq: 1, role: "operator", kind: "message", message: "turn-b", text: "Retry turn B", payload: { refs, attachments, conversation: "home-thread" } }];
const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname.endsWith("/stream")) {
    if (holdStream) return new Response(new ReadableStream<Uint8Array>({ start(controller) { init?.signal?.addEventListener("abort", () => controller.close(), { once: true }); } }), { headers: { "Content-Type": "text/event-stream" } });
    return streamFixture(url.href, streamMaster as never, init?.signal);
  }
  if (url.pathname === "/api/threads/master/messages") {
    posts.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
    return json({ message: posts.at(-1)?.message, state: "queued", duplicate: false });
  }
  if (url.pathname === "/api/master/command") {
    commands.push(JSON.parse(String(init?.body)) as { command?: string; message?: string });
    return json({ ok: true, state: "stopped" });
  }
  if (url.pathname === "/api/master/state" && hostedStateRead !== null) return json(hostedStateRead);
  if (url.pathname === "/api/assist/actions") return json({ actions: stagedActions });
  if (url.pathname === "/api/threads/master") return json({ thread: { id: "home-thread", alias: "master" }, entries, more_before: false });
  if (url.pathname === "/api/master/state" && servedState !== null) return json(servedState);
  if (url.pathname === "/api/master/state") return json({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] });
  if (url.pathname === "/api/agents") return json({ daemon: "reachable", agents: [{ alias: "master", state: "idle" }], totals: null });
  if (url.pathname === "/api/meta") return json({ operator: true, read_only: false });
  throw new Error(`Unexpected request ${url.pathname}`);
};
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
async function main() {
  const host = document.createElement("div"); document.body.append(host);
  const root = createRoot(host);
  const settle = async () => { for (let i = 0; i < 6; i++) await React.act(async () => { await nextTurn(); }); };
  const render = async (readOnly: boolean, hosted = false, decisions = false) => {
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(Home, {
        readOnly, hosted, decisions, overview: resources.overview.get(),
        onOpenIssue: () => undefined, onOpenDecision: () => undefined,
        permissionsHref: "/permissions", resolveAppPath: () => null,
      })));
    });
    await settle();
  };
  await render(false);
  assert(!classesEverSet.some((value) => value.includes("msg-in")), "stored history does not rise in on the first loaded render");
  entries.push({ seq: 2, role: "agent", kind: "turn_result", message: "turn-b", text: "Turn B failed", created: new Date().toISOString(), payload: { status: "failed", error: "Turn B failed" } });
  await React.act(async () => {
    resources.masterThread.write((state) => ({ ...state!, entries: [...entries], pending: [{ message: "transport-a", text: "Do not retry A", state: "failed", at: 1, refs: [{ kind: "issue", id: "CAD-A" }], attachments: [{ id: "file-a" }], conversation: "other-home" }] }));
  });
  await settle();
  const retry = Array.from(host.querySelectorAll<HTMLButtonElement>("[data-turn-bar] button")).find((button) => button.textContent === "Retry");
  assert(retry, "the displayed failed B turn offers Retry");
  await React.act(async () => { retry.click(); });
  await settle();
  const sent = posts.at(-1);
  assert(sent?.text === "Retry turn B" && sent.message !== "transport-a" && sent.message !== "turn-b", "Home's failed-turn bar retries B with a fresh id, never unrelated transport failure A");
  assert(JSON.stringify(sent.refs) === JSON.stringify(refs) && JSON.stringify(sent.attachments) === JSON.stringify(attachments) && sent.conversation === "home-thread", "Home turn Retry preserves its references, files and captured conversation");
  assert(resources.masterThread.get().data?.pending.some((row) => row.message === "transport-a" && row.state === "failed"), "unrelated A remains available for its own row Retry");

  // CAD-1880: a writable Home turn runs with no dock row and Stop in the
  // composer's send slot; the click posts the same command the bar used to.
  commands.length = 0;
  // The composer's Stop needs a live stream and a writable board: seed the
  // agent row the mount's /api/agents answer would carry, let that reopen the
  // stream, then publish the running turn so the frame cannot overwrite it.
  await React.act(async () => {
    resources.agents.write(() => ({ daemon: "reachable", agents: [{ alias: "master", state: "running" }], totals: null }) as never);
  });
  await settle();
  await React.act(async () => {
    streamMaster.turn = { state: "working", message: "turn-b", conversation: "home-thread" };
    streamMaster.queue = [];
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true,
      turn: { state: "working", message: "turn-b", conversation: "home-thread" }, queue: [] }));
  });
  await settle();
  assert(host.querySelector("[data-turn-bar]") === null, "a running turn paints no dock row");
  const stop = host.querySelector<HTMLButtonElement>('[aria-label="Stop this turn"]');
  assert(stop !== null, "a writable Home turn offers Stop in the composer");
  assert(host.querySelector(".app-chat-send") === stop, "Stop occupies the composer's send slot");
  await React.act(async () => { stop.click(); });
  await settle();
  assert(commands.at(-1)?.command === "stop" && commands.at(-1)?.message === "turn-b", "the composer's Stop posts the stop for the running turn's own message");

  // CAD-1881: one overflow sheet on a narrow header — the model/effort chips
  // and the About action live behind it, not in the header row.
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true,
      model: "claude-sonnet", model_label: "Claude Sonnet", effort: "medium", context: { tokens: 10, window: 200000, percent: 1 },
      turn: null, queue: [] }));
  });
  await settle();
  const menu = host.querySelector<HTMLButtonElement>(".assistant-menu-trigger");
  assert(menu !== null && menu.getAttribute("aria-expanded") === "false", "the header offers one overflow trigger");
  await React.act(async () => { menu!.click(); });
  await settle();
  const panel = host.querySelector('.assistant-menu-panel[role="dialog"]');
  assert(panel !== null, "the overflow trigger opens one sheet");
  assert(panel!.querySelector(".master-chips") !== null, "the sheet hosts the model and effort chips");
  assert(panel!.querySelector(".assistant-menu-about") !== null, "the sheet hosts the About action");
  assert(panel!.querySelector("[data-menu-scope]") !== null, "the sheet carries the scope line the header drops");
  await React.act(async () => { (panel!.querySelector(".assistant-menu-head button") as HTMLButtonElement).click(); });
  await settle();
  assert(host.querySelector(".assistant-menu-panel") === null, "Close dismisses the sheet");

  await render(true);
  // A fresh running message: the earlier turn-b already carries a failed
  // result, and a turn with an answer is not streaming.
  entries.push({ seq: 3, role: "operator", kind: "message", message: "turn-c", text: "Read-only turn C" });
  await React.act(async () => {
    resources.masterThread.write((state) => ({ ...state!, entries: [...entries] }));
    resources.masterState.write(() => ({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true,
      identity: { name: null, tone: null, look: "robot", revision: "fixture" },
      turn: { state: "working", message: "turn-c", conversation: "home-thread" }, queue: [] }));
  });
  await settle();
  assert(
    host.querySelector('[data-kind="typing"]') !== null
      && host.querySelector('[data-turn-bar][data-state="working"]') === null
      && !host.querySelector('[aria-label="Stop this turn"]'),
    "read-only Home observes its working turn through the inline bubble, without a dock row or a mutating Stop",
  );
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true,
      identity: { name: null, tone: null, look: "robot", revision: "fixture" },
      turn: { state: "working", message: "elsewhere", conversation: "other-home" }, queue: [{ message: "turn-b", conversation: "home-thread" }] }));
  });
  await settle();
  assert(!Array.from(host.querySelectorAll("[data-turn-bar] button")).some((button) => button.textContent === "Cancel"), "read-only Home cannot cancel a queued turn");
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] }));
    resources.masterThread.write((state) => ({ ...state!, pending: state!.pending.filter((row) => row.state === "failed") }));
  });
  await settle();
  assert(!Array.from(host.querySelectorAll("[data-turn-bar] button")).some((button) => button.textContent === "Retry"), "read-only Home cannot retry a stored failure");

  // CAD-2792: keyboard hint removed in favor of clean composer footer with model & effort pills.
  await render(false);
  assert(host.querySelector("[data-composer-hint]") === null, "the full composer omits the keyboard hint");
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] }));
    resources.masterThread.write((state) => ({
      ...state!,
      pending: [{ message: "ui-submitting", text: "While the POST is in flight", state: "sending", at: 2 }],
    }));
  });
  await settle();
  const typingRows = Array.from(host.querySelectorAll('[data-kind="typing"]')) as HTMLElement[];
  assert(typingRows.length === 1, `a submitting turn shows exactly one inline typing bubble (got ${typingRows.length})`);
  const typingTurn = typingRows[0].closest('[data-kind="turn"][data-turn-state="running"]');
  assert(typingTurn !== null && !typingRows[0].closest("[data-turn-bar]"), "the typing bubble paints inside the assistant turn, not the dock status bar");
  assert(
    host.querySelector("[data-chat-dock] [data-turn-bar]") === null,
    "a pending send leaves no status row above the Home composer",
  );
  assert(host.querySelector('[aria-label="Stop this turn"]') === null, "nothing is stoppable before the daemon runs the turn");
  const listItems = Array.from(host.querySelectorAll("ol[role='log'] > li")) as HTMLElement[];
  const typingIndex = listItems.findIndex((row) => row.contains(typingRows[0]));
  const operatorIndex = listItems.findIndex((row) => row.querySelector('[data-chat-message="user"]'));
  assert(typingIndex === listItems.length - 1 && operatorIndex >= 0 && typingIndex > operatorIndex, "the typing bubble trails the operator's message");
  const face = typingTurn.querySelector(".assistant-face");
  assert(face !== null, "the submitting turn paints one assistant face");
  const rowEl = typingTurn.parentElement;
  // The first stored step often has no message id, and the daemon's working
  // id is not the send id. The face has to stay mounted across that handoff.
  await React.act(async () => {
    streamMaster.turn = { state: "working", message: "server-id", conversation: "home-thread" };
    streamMaster.queue = [];
    resources.masterThread.write((state) => ({
      ...state!,
      pending: [],
      entries: [
        ...state!.entries,
        { seq: 20, role: "operator", kind: "message", message: "ui-submitting", text: "While the POST is in flight" },
        { seq: 21, role: "agent", kind: "thinking", message: null, text: "Reading the board." },
      ],
    }));
    resources.masterState.write(() => ({
      alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true,
      turn: { state: "working", message: "server-id", conversation: "home-thread" }, queue: [],
    }));
  });
  await settle();

  assert(face.isConnected && rowEl?.isConnected === true, "the assistant face stays mounted when the first untagged step lands");
  assert(host.querySelectorAll('[data-kind="typing"]').length === 1, "the first step does not paint a second typing bubble");
  assert(host.querySelectorAll('[data-kind="turn"][data-turn-state="running"]').length === 1, "the first step does not paint a second assistant head");
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [
        ...state!.entries,
        { seq: 22, role: "agent", kind: "tool_call", message: null, text: "Bash: cadence board read" },
      ],
    }));
  });
  await settle();
  assert(face.isConnected, "the assistant face stays mounted when the tool call lands");
  const bubble = host.querySelector('[data-kind="typing"]');
  assert(bubble !== null && bubble.textContent?.includes("Running bash") === true, "the typing bubble names the tool now running");
  const trace = host.querySelector('[data-kind="turn-trace"]');
  assert(
    trace !== null && (bubble.compareDocumentPosition(trace) & Node.DOCUMENT_POSITION_PRECEDING) !== 0,
    "the typing bubble follows the trace",
  );
  // Completion must not remount the turn either: the send identity comes
  // from the user row, not from the working state that just cleared.
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [
        ...state!.entries,
        { seq: 23, role: "agent", kind: "turn_result", message: null, text: "The board is clear." },
      ],
    }));
    streamMaster.turn = null;
    streamMaster.queue = [];
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] }));
  });
  await settle();
  assert(face.isConnected, "the assistant face stays mounted when the answer lands");
  assert(host.querySelectorAll('[data-kind="typing"]').length === 0, "a landed answer clears the typing bubble");
  // A mismatched working id with a send queued behind it: the placeholder
  // belongs to the send being worked, not to the newest queued row.
  streamMaster.turn = { state: "working", message: "server-a", conversation: "home-thread" };
  streamMaster.queue = [{ message: "ui-b", conversation: "home-thread" }];
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      pending: [],
      entries: [
        ...state!.entries,
        { seq: 30, role: "operator", kind: "message", message: "ui-a", text: "A ask" },
        { seq: 31, role: "operator", kind: "message", message: "ui-b", text: "B queued" },
      ],
    }));
  });
  await settle();
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: { state: "working", message: "server-a", conversation: "home-thread" }, queue: [{ message: "ui-b", conversation: "home-thread" }] }));
  });
  await settle();
  const queuedTurn = host.querySelector('[data-kind="typing"]')?.closest('[data-kind="turn"]') ?? null;
  assert(queuedTurn !== null, "the working placeholder paints with a queued send behind it");
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [
        ...state!.entries,
        { seq: 32, role: "agent", kind: "thinking", message: null, text: "A works" },
      ],
    }));
  });
  await settle();
  assert(host.querySelector('[data-kind="typing"]')?.closest('[data-kind="turn"]') === queuedTurn, "the placeholder updates in place when the first step lands behind a queued send");
  // A retained failed send stays visible for Retry but must not name the
  // working turn or steal its placeholder.
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      pending: [{ message: "ui-failed", text: "failed send", state: "failed", at: 2 }],
      entries: [
        ...state!.entries,
        { seq: 40, role: "operator", kind: "message", message: "ui-c", text: "C ask" },
      ],
    }));
  });
  await settle();
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: { state: "working", message: "server-fail", conversation: "home-thread" }, queue: [] }));
  });
  await settle();
  const failedTurn = host.querySelector('[data-kind="typing"]')?.closest('[data-kind="turn"]') ?? null;
  assert(failedTurn !== null, "the working placeholder paints past a failed send");
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [
        ...state!.entries,
        { seq: 41, role: "agent", kind: "thinking", message: null, text: "C works" },
      ],
    }));
  });
  await settle();
  assert(host.querySelector('[data-kind="typing"]')?.closest('[data-kind="turn"]') === failedTurn, "a failed send does not steal the working placeholder");
  // A fallback carried by a system row must still hoist into the stable
  // list, so the first step replaces it rather than remounting the head.
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      pending: [],
      entries: [
        ...state!.entries,
        { seq: 50, role: "operator", kind: "message", message: "ui-d", text: "D ask" },
        { seq: 51, role: "system", kind: "message", message: "m-system", text: "system note" },
      ],
    }));
  });
  await settle();
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: { state: "working", message: "server-d", conversation: "home-thread" }, queue: [] }));
  });
  await settle();
  const systemTurn = host.querySelector('[data-kind="typing"]')?.closest('[data-kind="turn"]') ?? null;
  assert(systemTurn !== null, "the working placeholder paints over a system carrier");
  assert(systemTurn.closest("li")?.querySelector('[data-kind="system"]') === null, "the fallback is not nested under the system row");
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [
        ...state!.entries,
        { seq: 52, role: "agent", kind: "thinking", message: null, text: "D works" },
      ],
    }));
  });
  await settle();
  assert(host.querySelector('[data-kind="typing"]')?.closest('[data-kind="turn"]') === systemTurn, "the first step replaces the system-carried placeholder in place");
  // An answered run between the working send and its own steps must not hide
  // the running send: the adopted id is the evidence, not the helper alone.
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      pending: [],
      entries: [
        ...state!.entries,
        { seq: 60, role: "operator", kind: "message", message: "ui-e", text: "E ask" },
        { seq: 61, role: "operator", kind: "message", message: "ui-f", text: "F ask" },
        { seq: 62, role: "agent", kind: "thinking", message: null, text: "E works" },
        { seq: 63, role: "agent", kind: "turn_result", message: null, text: "E answer" },
        { seq: 64, role: "agent", kind: "thinking", message: null, text: "F works" },
      ],
    }));
  });
  await settle();
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: { state: "working", message: "ui-f", conversation: "home-thread" }, queue: [] }));
  });
  await settle();
  const interleavedTyping = host.querySelector('[data-kind="typing"]')?.closest('[data-kind="turn"]') ?? null;
  assert(interleavedTyping !== null, "the working send paints typing behind an earlier answered run");
  assert(host.querySelectorAll('[data-kind="typing"]').length === 1, "one typing bubble for the interleaved send");
  assert(host.querySelectorAll('[data-chat-typing]').length === 0, "no standalone typing head behind an answered run");
  const eRun = host.querySelector('[data-item-key="e63"]')?.closest('[data-kind="turn"]') ?? null;
  assert(eRun !== null, "the answered run is painted");
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] }));
  });
  await settle();
  assert(host.querySelectorAll('[data-kind="turn"]').length > 0, "the transcript stays after the turn clears");
  const eRunAfter = host.querySelector('[data-item-key="e63"]')?.closest('[data-kind="turn"]') ?? null;
  assert(eRunAfter === eRun, "the earlier answered run keeps its identity after the working turn clears");
  await React.act(async () => {
    streamMaster.turn = null;
    streamMaster.queue = [];
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] }));
    resources.masterThread.write((state) => ({ ...state!, pending: state!.pending.filter((row) => row.state === "failed") }));
  });
  await settle();
  assert(host.querySelectorAll('[data-kind="typing"]').length === 0, "the typing bubble clears once the send settles out of submitting");

  // CAD-2011: a hosted board reports its running turn with no message id and
  // its agent rows carry none either. That is still one live turn: no second
  // head, no finished look, and no interim note promoted to an answer.
  const hostedState = (turn: unknown) => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "branded-hosted", endpoint_kind: "durable_object", live: true, turn, queue: [] }) as never;
  const hostedTurn = { state: "working", message: null, conversation: "home-thread", app_origin: null, summary: null, since: null };
  hostedStateRead = hostedState(null);
  await render(false, true);
  await React.act(async () => {
    resources.masterState.write(() => hostedState(null));
    resources.masterThread.write((state) => ({ ...state!, entries: [], pending: [] }));
  });
  await React.act(async () => {
    const result = await sendToMaster("Hosted ask", "ui-hosted", undefined, undefined, "home-thread");
    assert(result.ok, "the hosted send was accepted");
  });
  await settle();
  assert(host.querySelectorAll('[data-kind="typing"]').length === 1, "the accepted local send shows one in-thread activity");
  hostedStateRead = hostedState(hostedTurn);
  await React.act(async () => {
    resources.masterState.write(() => hostedState(hostedTurn));
    resources.masterThread.write((state) => ({
      ...state!,
      pending: [],
      entries: [{ seq: 70, role: "operator", kind: "message", message: "ui-hosted", text: "Hosted ask" }],
    }));
  });
  await settle();
  assert(host.querySelectorAll('[data-kind="typing"]').length === 1, "the unnamed hosted working turn keeps one activity before its first tool");
  assert(host.querySelector("[data-chat-dock] [data-turn-bar]") === null, "the hosted activity does not create a status dock");
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [
        ...state!.entries,
        { seq: 71, role: "agent", kind: "tool_call", text: "catalog_search: website" },
        { seq: 72, role: "agent", kind: "tool_result", text: "ok" },
        { seq: 73, role: "agent", kind: "assistant_text", text: "Checking drafts as well" },
      ],
    }));
  });
  await settle();
  assert(host.querySelectorAll(".turn-head").length === 1, `a hosted working turn paints one head (got ${host.querySelectorAll(".turn-head").length})`);
  const hostedHead = host.querySelector(".turn-head");
  assert(host.querySelector('[data-kind="turn"]')?.getAttribute("data-turn-state") === "running", "the hosted run paints as running, not finished");
  assert(host.querySelectorAll('[data-kind="typing"]').length === 1, "the hosted run has one typing row");
  assert(host.querySelector('[data-kind="answer"]') === null && !host.textContent?.includes("Earlier transcript"), "an interim note stays in the trace while the hosted turn runs");
  // The answer usually arrives a read before the working flag clears.
  hostedStateRead = hostedState(hostedTurn);
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [...state!.entries, { seq: 74, role: "agent", kind: "turn_result", text: "Hosted answer" }],
    }));
  });
  await settle();
  assert(host.querySelectorAll(".turn-head").length === 1, `no second head while the flag catches up (got ${host.querySelectorAll(".turn-head").length})`);
  hostedStateRead = null;
  await React.act(async () => { resources.masterState.write(() => hostedState(null)); });
  await settle();
  assert(host.querySelectorAll(".turn-head").length === 1 && host.querySelectorAll('[data-kind="answer"]').length === 1, "the answer lands under the one head");
  assert(host.querySelector(".turn-head") === hostedHead, "the answer lands under the same head, not a remounted one");
  // A global working flag for another conversation must not animate Home.
  hostedStateRead = hostedState({ ...hostedTurn, conversation: "foreign-thread" });
  await React.act(async () => {
    resources.masterState.write(() => hostedStateRead as never);
    resources.masterThread.write((state) => ({
      ...state!,
      pending: [],
      entries: [{ seq: 75, role: "operator", kind: "message", message: "old-home", text: "An older Home request" }],
    }));
  });
  await settle();
  assert(host.querySelectorAll('[data-kind="typing"]').length === 0, "a foreign working turn does not animate Home");

  // A staged tool takes over the live indicator, even if the end-of-turn
  // snapshot is late. The receipt replaces only the matching continuation.
  const actionId = "86753090000000000000000000000001";
  const action = {
    id: actionId, tool: "custom_mcp_effect", status: "pending", createdAt: Date.now(), decidedAt: null,
    input: {
      connection_id: "conn_fixture", connection_name: "Demo inventory",
      tool: "cart.add", snapshot_revision: 2, endpoint_host: "inventory.example.test", args: '{"sku":"DEMO-101"}',
    },
  };
  const stagedEntries: ThreadEntry[] = [
    { seq: 80, role: "operator", kind: "message", message: "here", text: "Update the demo cart" },
    { seq: 81, role: "agent", kind: "tool_call", text: "execute_tool: cart.add" },
    { seq: 82, role: "agent", kind: "tool_result", text: JSON.stringify({ action: actionId, sent: false, note: "Staged for approval" }) },
    { seq: 83, role: "operator", kind: "message", message: "same-request", text: "Add the same item again" },
    { seq: 84, role: "agent", kind: "tool_call", text: "execute_tool: cart.add" },
    { seq: 85, role: "agent", kind: "tool_result", text: JSON.stringify({ action: actionId, sent: false, note: "Staged for approval" }) },
  ];
  await render(false, true, true);
  stagedActions = [action];
  await React.act(async () => {
    hostedStateRead = hostedState(hostedTurn);
    resources.masterState.write(() => hostedState(hostedTurn));
    resources.masterThread.write((state) => ({ ...state!, entries: stagedEntries }));
  });
  await settle();
  assert(host.querySelector('[data-turn-state="awaiting-approval"] button') && host.textContent?.includes("Needs you"), `the thread and header show the staged request: ${host.textContent}`);
  assert(host.textContent?.includes("Review request") && !host.querySelector('[data-kind="typing"]'), "review takes over live activity");
  assert(host.querySelectorAll("[data-staged-review]").length === 1, "the same pending action only offers one review");
  assert(!host.querySelector("[data-tool-decision-receipt]"), "pending is not a recorded decision");
  await React.act(async () => {
    const namedRun = { ...hostedTurn, message: "same-request" };
    hostedStateRead = hostedState(namedRun);
    resources.masterState.write(() => hostedState(namedRun));
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [...stagedEntries, { seq: 86, role: "agent", kind: "tool_call", text: "catalog_search: demo products" }],
    }));
  });
  await settle();
  assert(host.querySelectorAll('[data-kind="typing"]').length === 1, "a newer independent tool call restores one live indicator");
  assert(host.querySelector('[aria-label="Stop this turn"]') && host.querySelectorAll("[data-staged-review]").length === 1, "Stop returns without dropping the pending review");
  await React.act(async () => {
    const entries = stagedEntries.slice(0, -2);
    const stageId = "staging-call";
    entries.push(
      // Write both calls, then only the staged request's result.
      { seq: 84, role: "agent", kind: "tool_call", text: "catalog_search: demo products", payload: { tool_use_id: "slow-lookup" } },
      { ...stagedEntries[4], seq: 85, payload: { tool_use_id: stageId } },
      { ...stagedEntries[5], seq: 86, payload: { tool_use_id: stageId } },
    );
    resources.masterThread.write((state) => ({ ...state!, entries }));
  });
  await settle();
  assert(host.querySelectorAll('[data-kind="typing"]').length === 1 && !host.querySelector('[data-kind="typing"]')?.textContent?.includes("Execute tool"), "the open sibling, not the staged tool, keeps the live indicator");
  assert(host.querySelector('[aria-label="Stop this turn"]') && host.querySelectorAll("[data-staged-review]").length === 1, "a staged result does not close a slow sibling call");
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [...state!.entries, { seq: 87, role: "agent", kind: "tool_result", text: "Lookup found one match", payload: { tool_use_id: "slow-lookup" } }],
    }));
  });
  await settle();
  assert(!host.querySelector('[data-kind="typing"], [aria-label="Stop this turn"]') && host.querySelector("[data-staged-review]"), "only the pending decision remains once the sibling finishes");
  // Wire example matches AgenticOS's decision-note-projection acceptance test.
  const marker: ThreadEntry = { seq: 90, role: "system", kind: "tool_approval_decision", message: `decision:${actionId}`, text: "Tool decision recorded." };
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [
        ...state!.entries,
        { seq: 88, role: "agent", kind: "tool_call", text: "catalog_search: other products", payload: { tool_use_id: "later-lookup" } },
        { seq: 89, role: "agent", kind: "tool_result", text: "No other matches", payload: { tool_use_id: "later-lookup" } },
        marker,
      ],
    }));
  });
  await settle();
  assert(!host.querySelector("[data-tool-decision-receipt]"), "the early continuation is not proof of the decision");
  assert(host.querySelectorAll('[data-chat-message="user"]').length === 2, "the early continuation does not paint a user bubble");
  const { homeDecisions } = require<typeof Decisions>("../src/features/decisions/useDecisions");
  const decided = { ...action, status: "denied", decidedAt: Date.now(), result: { ok: false, code: "uncertain", retry: true } };
  stagedActions = [decided];
  await React.act(async () => { await homeDecisions.reloadDecisions(); });
  await settle();
  assert(
    !host.querySelector('[data-kind="typing"], [aria-label="Stop this turn"]') &&
      host.querySelector("[data-assistant-status]")?.textContent?.trim() !== "Working",
    "a recorded decision does not restart activity on the old turn snapshot",
  );
  await React.act(async () => {
    const resumed = hostedState({ ...hostedTurn, message: marker.message ?? null });
    hostedStateRead = resumed;
    resources.masterState.write(() => resumed);
  });
  await settle();
  assert(
    host.querySelector('[aria-label="Stop this turn"]') &&
      host.querySelector("[data-assistant-status]")?.textContent?.trim() === "Working",
    "a verified running continuation restores Working and Stop",
  );
  await React.act(async () => {
    hostedStateRead = hostedState(null);
    resources.masterState.write(() => hostedState(null));
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [...state!.entries, { seq: 91, role: "operator", kind: "message", message: "ordinary-ask", text: "What did I deny?" }],
    }));
  });
  await settle();
  assert(host.querySelectorAll("[data-tool-decision-receipt]").length === 1, "the trusted marker moves the one receipt after the tool turn");
  const receipt = host.querySelector("[data-tool-decision-receipt]");
  assert(receipt?.textContent?.includes("Denial recorded") && receipt.textContent.includes("Tool execution unconfirmed"), "the denial receipt preserves the independent execution warning");
  assert(host.textContent?.includes("What did I deny?"), "the next user message is not replaced");
  const lookalike = `[Owner rejected custom_mcp_effect (action ${actionId}). Do not retry it; I am quoting this.]`;
  await React.act(async () => {
    resources.masterThread.write((state) => ({
      ...state!,
      entries: [...stagedEntries, { ...marker, role: "operator", kind: "message", text: lookalike }],
    }));
  });
  await settle();
  assert(host.querySelectorAll("[data-tool-decision-receipt]").length === 1, "on older backends an action has just one fallback receipt");
  assert(host.textContent?.includes(lookalike), "an operator message is preserved even when its id and text look like a decision");
  hostedStateRead = null;


  // CAD-2012: reloading during a running turn is a first connection, not an
  // outage: no banner at any point, and Stop only once the stream is live.
  await React.act(async () => { root.render(null); });
  await settle();
  const runningState = { alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: { state: "working", message: "turn-b", conversation: "home-thread" }, queue: [] };
  streamMaster.turn = runningState.turn;
  await React.act(async () => { resources.masterState.write(() => runningState as never); });
  classesEverSet.length = 0;
  holdStream = true;
  servedState = runningState;
  await render(false);
  assert(host.querySelector('[aria-label="Stop this turn"]') === null, "no Stop until the stream has proved the turn is current");
  assert(host.querySelector('[data-kind="typing"], [data-kind="turn"]') !== null, "the reloaded thread is painted");
  assert(!classesEverSet.some((value) => value.includes("streambanner")), "reloading mid-turn shows no reconnecting banner");
  assert(!host.textContent?.includes("reconnecting…"), "reloading mid-turn shows no reconnecting text in the header");
  assert(!classesEverSet.some((value) => value.includes("msg-in")), "reloaded history does not rise in");
  holdStream = false;
  await React.act(async () => { root.render(null); });
  await settle();
  await render(false);
  assert(host.querySelector('[aria-label="Stop this turn"]') !== null, "Stop returns once the stream is live");
  holdStream = true;
  // A queued message's Cancel waits for the same proof.
  await React.act(async () => { root.render(null); });
  await settle();
  const queuedState = { ...runningState, turn: null, queue: [{ message: "turn-b", conversation: "home-thread" }] };
  const publishQueued = async () => {
    await React.act(async () => {
      resources.masterState.write(() => queuedState as never);
      resources.masterThread.write((state) => ({ ...state!, entries: [...entries], pending: [] }));
    });
    await settle();
  };
  const cancelNow = () => Array.from(host.querySelectorAll<HTMLButtonElement>("button")).some((button) => button.textContent === "Cancel");
  streamMaster.turn = null;
  streamMaster.queue = queuedState.queue;
  servedState = queuedState;
  await render(false);
  await publishQueued();
  assert(host.querySelector('[data-turn-bar][data-state="queued"]') !== null, "the queued message is what the bar is showing");
  assert(!cancelNow(), "no Cancel until the stream has proved the queue is current");
  await React.act(async () => { root.render(null); });
  await settle();
  holdStream = false;
  await render(false);
  await publishQueued();
  assert(cancelNow(), "Cancel returns once the stream is live");

  await React.act(async () => { root.unmount(); });
  await win.happyDOM.close();
  console.log("Home turn Retry checks passed");
}
void main();
