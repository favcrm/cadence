import { streamFixture } from "./streamFixture";
import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as HomeModule from "../src/features/home/Home";
import type * as Locale from "../src/lib/locale";
import type * as Stores from "../src/lib/resources";
import type { ThreadEntry } from "../src/features/home/thread";
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
const { setImmediate: nextTurn } = require<{ setImmediate: () => Promise<void> }>("node:timers/promises");
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
  if (url.pathname.endsWith("/stream")) return streamFixture(url.href, streamMaster as never, init?.signal);
  if (url.pathname === "/api/threads/master/messages") {
    posts.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
    return json({ message: posts.at(-1)?.message, state: "queued", duplicate: false });
  }
  if (url.pathname === "/api/master/command") {
    commands.push(JSON.parse(String(init?.body)) as { command?: string; message?: string });
    return json({ ok: true, state: "stopped" });
  }
  if (url.pathname === "/api/threads/master") return json({ thread: { id: "home-thread", alias: "master" }, entries, more_before: false });
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
  const render = async (readOnly: boolean) => {
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(Home, {
        readOnly, hosted: false, overview: resources.overview.get(),
        onOpenIssue: () => undefined, onOpenDecision: () => undefined,
        overviewHref: "/overview", permissionsHref: "/permissions", resolveAppPath: () => null,
      })));
    });
    await settle();
  };
  await render(false);
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

  // CAD-1859: a send that has not reached the daemon yet still shows the
  // inline typing bubble. `submitting` carries no message id, so the bubble
  // rides the operator's own row rather than a stored entry of this turn.
  await render(false);
  assert(host.querySelector("[data-composer-hint]") !== null, "the full composer carries the keyboard hint");
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
    host.querySelector('[data-turn-bar][data-state="sending"]') !== null,
    "the dock bar reports the send as sending, not as a working banner",
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
  assert(bubble !== null && bubble.textContent?.includes("Running Bash: cadence board read") === true, "the typing bubble names the tool now running");
  const trace = host.querySelector('[data-kind="turn-trace"]');
  assert(
    trace !== null && (bubble.compareDocumentPosition(trace) & Node.DOCUMENT_POSITION_PRECEDING) !== 0,
    "the typing bubble follows the trace",
  );
  await React.act(async () => {
    streamMaster.turn = null;
    streamMaster.queue = [];
    resources.masterState.write(() => ({ alias: "master", identity: { name: null, tone: null, look: "robot", revision: "fixture" }, provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] }));
    resources.masterThread.write((state) => ({ ...state!, pending: state!.pending.filter((row) => row.state === "failed") }));
  });
  await settle();
  assert(host.querySelectorAll('[data-kind="typing"]').length === 0, "the typing bubble clears once the send settles out of submitting");


  await React.act(async () => { root.unmount(); });
  await win.happyDOM.close();
  console.log("Home turn Retry checks passed");
}
void main();
