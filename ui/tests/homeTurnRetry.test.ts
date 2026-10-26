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
const refs = [{ kind: "issue", id: "CAD-B" }];
const attachments = [{ id: "file-b" }];
const entries: ThreadEntry[] = [{ seq: 1, role: "operator", kind: "message", message: "turn-b", text: "Retry turn B", payload: { refs, attachments, conversation: "home-thread" } }];
const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname.endsWith("/stream")) return streamFixture(url.href, undefined, init?.signal);
  if (url.pathname === "/api/threads/master/messages") {
    posts.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
    return json({ message: posts.at(-1)?.message, state: "queued", duplicate: false });
  }
  if (url.pathname === "/api/threads/master") return json({ thread: { id: "home-thread", alias: "master" }, entries, more_before: false });
  if (url.pathname === "/api/master/state") return json({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] });
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
  await render(true);
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true,
      turn: { state: "working", message: "turn-b", conversation: "home-thread" }, queue: [] }));
  });
  await settle();
  assert(host.querySelector("[data-turn-bar]") && !host.querySelector('[aria-label="Stop this turn"]'), "read-only Home observes its working turn without a mutating Stop");
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true,
      turn: { state: "working", message: "elsewhere", conversation: "other-home" }, queue: [{ message: "turn-b", conversation: "home-thread" }] }));
  });
  await settle();
  assert(!Array.from(host.querySelectorAll("[data-turn-bar] button")).some((button) => button.textContent === "Cancel"), "read-only Home cannot cancel a queued turn");
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] }));
    resources.masterThread.write((state) => ({ ...state!, pending: state!.pending.filter((row) => row.state === "failed") }));
  });
  await settle();
  assert(!Array.from(host.querySelectorAll("[data-turn-bar] button")).some((button) => button.textContent === "Retry"), "read-only Home cannot retry a stored failure");

  // CAD-1859: a send that has not reached the daemon yet still shows the
  // inline typing bubble. `submitting` carries no message id, so the bubble
  // rides the operator's own row rather than a stored entry of this turn.
  await render(false);
  await React.act(async () => {
    resources.masterState.write(() => ({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] }));
    resources.masterThread.write((state) => ({
      ...state!,
      pending: [{ message: "ui-submitting", text: "While the POST is in flight", state: "sending", at: 2 }],
    }));
  });
  await settle();
  const typingRows = Array.from(host.querySelectorAll('[data-kind="typing"]')) as HTMLElement[];
  assert(typingRows.length === 1, `a submitting turn shows exactly one inline typing bubble (got ${typingRows.length})`);
  const typingTurn = typingRows[0].closest('li, [data-kit="turn"]') as HTMLElement | null;
  assert(typingTurn !== null && typingTurn.textContent?.includes("Assistant"), "the typing bubble paints as an assistant turn, not a status bar");
  assert(
    host.querySelector('[data-turn-bar][data-state="sending"]') !== null,
    "the dock bar reports the send as sending, not as a working banner",
  );
  assert(host.querySelector('[data-turn-bar] [aria-label="Stop this turn"]') === null, "nothing is stoppable before the daemon runs the turn");
  const listItems = Array.from(host.querySelectorAll("ol[role='log'] > li")) as HTMLElement[];
  const typingIndex = listItems.findIndex((row) => row.contains(typingRows[0]));
  const operatorIndex = listItems.findIndex((row) => row.querySelector('[data-chat-message="user"]'));
  assert(typingIndex === listItems.length - 1 && operatorIndex >= 0 && typingIndex > operatorIndex, "the typing bubble trails the operator's message");
  await React.act(async () => {
    resources.masterThread.write((state) => ({ ...state!, pending: [] }));
  });
  await settle();
  assert(host.querySelectorAll('[data-kind="typing"]').length === 0, "the typing bubble clears once the send settles out of submitting");

  await React.act(async () => { root.unmount(); });
  await win.happyDOM.close();
  console.log("Home turn Retry checks passed");
}
void main();
