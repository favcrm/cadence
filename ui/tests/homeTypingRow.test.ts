// CAD-2161: one typing row from send to reply. Recorded on a hosted board: the
// stored entry replaced the pending row while the state read still predated the
// turn, so the row left for one round trip and came back as a new element.
import { streamFixture } from "./streamFixture";
import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as HomeModule from "../src/features/home/Home";
import type * as Locale from "../src/lib/locale";
import type * as Stores from "../src/lib/resources";
import type * as Send from "../src/features/home/send";
import type * as Thread from "../src/features/home/thread";
import type { ThreadEntry } from "../src/features/home/thread";
declare function require<T>(name: string): T;
const { Window } = require<typeof HappyDOM>("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const key of ["window", "document", "navigator", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLButtonElement", "HTMLFormElement", "MutationObserver", "getComputedStyle", "localStorage", "sessionStorage"] as const)
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
const { mergeEntries } = require<typeof Thread>("../src/features/home/thread");
const { setImmediate: nextTurn } = require<{ setImmediate: () => Promise<void> }>("node:timers/promises");

const identity = { name: null, tone: null, look: "robot", revision: "fixture" };
const stateWith = (turn: unknown) => ({ alias: "master", identity, provider: "fixture", endpoint_kind: "fixture", live: true, turn, queue: [] });
let answer: unknown = stateWith(null);
let held: (() => void)[] | null = null;
const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname.endsWith("/stream")) return streamFixture(url.href, stateWith(null) as never, init?.signal);
  if (url.pathname === "/api/threads/master/messages") return json({ message: (JSON.parse(String(init?.body)) as { message: string }).message, state: "queued", duplicate: false });
  if (url.pathname === "/api/threads/master") return json({ thread: { id: "home-thread", alias: "master" }, entries: [], more_before: false });
  if (url.pathname === "/api/master/state") {
    // The answer is read when the request is let go: a held read predates nothing.
    if (held) await new Promise<void>((resolve) => held!.push(resolve));
    return json(answer);
  }
  if (url.pathname === "/api/agents") return json({ daemon: "reachable", agents: [{ alias: "master", state: "running" }], totals: null });
  if (url.pathname === "/api/meta") return json({ operator: true, read_only: false });
  throw new Error(`Unexpected request ${url.pathname}`);
};
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const settle = async () => { for (let i = 0; i < 8; i++) await React.act(async () => { await nextTurn(); }); };
const entry = (seq: number, role: string, kind: string, message: string, text: string): ThreadEntry =>
  ({ seq, role, kind, message, text, created: new Date().toISOString() }) as ThreadEntry;
const land = async (...rows: ThreadEntry[]) => {
  await React.act(async () => { resources.masterThread.write((s) => mergeEntries(s, rows)); });
  await settle();
};
const hold = () => { held = []; };
const letGo = async (next: unknown) => {
  answer = next;
  const waiting = held ?? [];
  held = null;
  await React.act(async () => { waiting.forEach((go) => go()); });
  await settle();
};

async function main() {
  const host = document.createElement("div");
  document.body.append(host);
  const mount = async () => {
    const root = createRoot(host);
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(Home, {
        readOnly: false, hosted: true, overview: resources.overview.get(),
        onOpenIssue: () => undefined, onOpenDecision: () => undefined,
        overviewHref: "/overview", permissionsHref: "/permissions", resolveAppPath: () => null,
      })));
    });
    await settle();
    return root;
  };
  const typing = () => host.querySelectorAll('[data-kind="typing"]');
  const chip = () => host.querySelector(".master-heading .chip")?.textContent ?? "";
  const send = async (text: string, message: string) => {
    await React.act(async () => { void sendToMaster(text, message); });
    await settle();
  };
  const root = await mount();
  // App drives this resource; Home only subscribes.
  await React.act(async () => {
    resources.agents.write(() => ({ daemon: "reachable", agents: [{ alias: "master", state: "running" }], totals: null }) as never);
  });
  await settle();
  assert(typing().length === 0, "nothing types before a send");
  assert(chip() === "Available", "the header rests before a send");

  // A named turn, then a hosted turn the state names with `message: null`.
  let seq = 0;
  for (const [message, running] of [["m-named", "m-named"], ["m-hosted", null]] as const) {
    hold();
    await send("hello", message);
    const row = typing()[0];
    assert(row, `${message}: the row shows on send`);
    await land(entry(++seq, "operator", "message", message, "hello"));
    assert(typing().length === 1 && typing()[0] === row, `${message}: the row stays while the state read still predates the turn`);
    assert(chip() !== "Available", `${message}: the header does not rest while the turn is starting`);
    await letGo(stateWith({ state: "working", message: running, conversation: "home-thread", since: 1 }));
    assert(typing().length === 1 && typing()[0] === row, `${message}: the verified turn keeps the same element`);
    assert(chip() === "Working", `${message}: the header reads Working`);
    await land(entry(++seq, "agent", "turn_result", message, "done"));
    answer = stateWith(null);
    await React.act(async () => { await resources.masterState.refresh(); });
    await settle();
    assert(typing().length === 0, `${message}: the reply ends the row`);
    assert(chip() === "Available", `${message}: the header rests again`);
  }

  // A row of the turn that lands first is the turn, not a second assistant.
  hold();
  await send("again", "m-early");
  const turnsBefore = host.querySelectorAll('[data-kind="turn"]').length;
  await land(entry(++seq, "operator", "message", "m-early", "again"), entry(++seq, "agent", "thinking", "m-early", "Reading."));
  assert(host.querySelectorAll('[data-kind="turn"]').length === turnsBefore, "an early row joins the typing turn");
  assert(typing().length === 1, "and still types once");
  await letGo(stateWith({ state: "working", message: "m-early", conversation: "home-thread", since: 1 }));
  await land(entry(++seq, "agent", "turn_result", "m-early", "done"));
  answer = stateWith(null);
  await React.act(async () => { await resources.masterState.refresh(); });
  await settle();

  // A read answered after the entry landed that reports no turn is the truth.
  hold();
  await send("lost", "m-lost");
  await land(entry(++seq, "operator", "message", "m-lost", "lost"));
  assert(typing().length === 1, "held while the read is out");
  await letGo(stateWith(null));
  assert(typing().length === 0, "a later read with no turn clears the row");

  // Another visit did not send it: an unanswered stored message shows no typing.
  await React.act(async () => { root.unmount(); });
  const again = await mount();
  assert(typing().length === 0, "an unanswered message from before this visit shows no typing");
  await React.act(async () => { again.unmount(); });
  await win.happyDOM.close();
  console.log("Home typing row checks passed");
}
void main();
