/** Replayed entries cannot enable actions against the cached previous turn. */
import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as Client from "../src/features/app-shell/conversationClient";
import type * as Engine from "../src/features/app-shell/chat/conversationThread";
import type * as Bar from "../src/features/app-shell/chat/TurnBar";
import type * as States from "../src/features/app-shell/chat/turnBarState";
import type * as Queries from "../src/lib/useResource";
import type * as Resources from "../src/lib/resources";
import type { MasterState } from "../src/lib/types";
import type { ThreadEntry } from "../src/features/home/thread";
import type { SseErrorState } from "../src/lib/sse";
declare function require<T>(name: string): T;
const { Window } = require<typeof HappyDOM>("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "navigator", "Node", "HTMLElement", "Event", "MouseEvent", "location", "history", "localStorage", "sessionStorage"] as const)
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const { setImmediate: nextTurn } = require<{ setImmediate: () => Promise<void> }>("node:timers/promises");
const React = require<typeof ReactModule>("react");
const { createRoot } = require<typeof ReactDOM>("react-dom/client");
const { LocaleProvider } = require<typeof Locale>("../src/lib/locale");
const { conversationThread } = require<typeof Client>("../src/features/app-shell/conversationClient");
const { useThreadStream } = require<typeof Engine>("../src/features/app-shell/chat/conversationThread");
const { TurnBar } = require<typeof Bar>("../src/features/app-shell/chat/TurnBar");
const { turnBarState } = require<typeof States>("../src/features/app-shell/chat/turnBarState");
const { useResource } = require<typeof Queries>("../src/lib/useResource");
const { resources } = require<typeof Resources>("../src/lib/resources");
const streams: { url: string; controller: ReadableStreamDefaultController<Uint8Array> }[] = [];
globalThis.fetch = async (input, init) => {
  const url = String(input);
  if (!url.includes("/api/threads/master/stream")) throw new Error(`Unexpected request: ${url}`);
  const body = new ReadableStream<Uint8Array>({ start(controller) {
    streams.push({ url, controller });
    init?.signal?.addEventListener("abort", () => controller.close(), { once: true });
  } });
  return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
};
const snapshot = (message: string): MasterState => ({
  alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true,
  turn: { state: "working", message, conversation: "conv-stream", position: null }, queue: [],
});
const thread = conversationThread("conv-stream");
thread.write(() => ({ exists: true, conversation: "conv-stream", entries: [{ seq: 10, role: "operator", kind: "message", text: "Request", message: "m-old" }], pending: [] }));
resources.masterState.write(() => snapshot("m-old"));
function Harness() {
  const data = useResource(thread);
  const master = useResource(resources.masterState);
  const [link, setLink] = React.useState<"live" | SseErrorState>("reconnecting");
  useThreadStream(thread, "/api/threads/master/stream?conversation=conv-stream", true, setLink);
  const state = turnBarState({ master: master.data, entries: data.data?.entries ?? [], pendingMessages: [], awaitingApproval: false, reconnecting: link !== "live" }, "conv-stream");
  return React.createElement("main", { "data-link": link }, React.createElement(TurnBar, {
    state, since: null, reconnecting: link !== "live",
    actions: { onStop: () => undefined, onCancel: () => undefined, onReview: () => undefined },
  }));
}
function assert(value: unknown, reason: string): asserts value { if (!value) throw new Error(reason); }
// A finished turn unmounts the bar: `done` is not a state the operator needs
// a lingering row for, and a 3 s banner sat over the composer's own space.
{
  const { renderToStaticMarkup } = require<typeof import("react-dom/server")>("react-dom/server");
  const bar = (state: Parameters<typeof TurnBar>[0]["state"], reconnecting = false) =>
    renderToStaticMarkup(React.createElement(TurnBar, {
      state, since: null, reconnecting,
      actions: { onStop: () => undefined, onCancel: () => undefined, onReview: () => undefined },
    }));
  assert(bar({ kind: "done", seconds: 6 }) === "", "a finished turn unmounts the bar instead of lingering");
  assert(bar({ kind: "idle" }) === "", "an idle conversation has no bar");
  assert(bar({ kind: "working", step: "planning", since: 1000, stopMessage: "m-1" }).includes('data-state="working"'), "a working turn still paints its bar");
  assert(bar({ kind: "working", step: "planning", since: 1000, stopMessage: "m-1" }, true).includes('data-stale="true"'), "a reconnect marks the bar stale");
}
async function main() {
  const unthreaded = snapshot("m-unthreaded");
  unthreaded.turn!.conversation = null;
  const draftInput = { master: unthreaded, entries: [], pendingMessages: [], awaitingApproval: false, reconnecting: false };
  assert(turnBarState(draftInput, null).kind === "idle", "an unresolved draft cannot claim an unthreaded running message");
  const ownPending = turnBarState({ ...draftInput, pendingMessages: ["m-unthreaded"] }, null);
  assert(ownPending.kind === "working" && ownPending.stopMessage === "m-unthreaded", "an unresolved draft retains controls for its own pending message");
  const host = document.createElement("div"); document.body.append(host);
  const root = createRoot(host);
  const settle = async () => { for (let i = 0; i < 6; i++) await React.act(async () => { await nextTurn(); }); };
  const mount = async () => {
    await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(Harness))); });
    await settle();
  };
  const emit = (text: string) => streams.at(-1)!.controller.enqueue(new TextEncoder().encode(text));
  const entry = (seq: number): ThreadEntry => ({ seq, role: "agent", kind: "tool_result", text: `Step ${seq}`, message: "m-new" });
  const observed: { seq: number; master: string | null }[] = [];
  const off = thread.subscribe(() => {
    const seq = thread.get().data?.entries.at(-1)?.seq ?? 0;
    if (seq > 10) observed.push({ seq, master: resources.masterState.get().data?.turn?.message ?? null });
  });
  await mount();
  assert(streams.length === 1 && streams[0].url.includes("after=10"), "the actual stream resumes from its stored entry cursor");
  assert(host.querySelector("main")?.getAttribute("data-link") !== "live" && !host.querySelector('[aria-label="Stop this turn"]'), "opening the socket cannot enable cached-turn actions before the current-state snapshot");
  await React.act(async () => {
    emit(`event: master\ndata: ${JSON.stringify(snapshot("m-new"))}\n\nid: 11\nevent: entry\ndata: ${JSON.stringify(entry(11))}\n\n`);
  });
  await settle();
  assert(host.querySelector("main")?.getAttribute("data-link") === "live" && host.querySelector('[aria-label="Stop this turn"]'), "the authoritative snapshot enables the correct current turn");
  assert(observed.some((row) => row.seq === 11) && observed.filter((row) => row.seq === 11).every((row) => row.master === "m-new"), "entry replay observes current master state, never the cached old turn");
  await React.act(async () => { root.render(null); });
  await mount();
  const resumed = streams.at(-1);
  assert(resumed && resumed !== streams[0] && resumed.url.includes("after=11"), "reconnecting resumes after the last applied entry");
  assert(host.querySelector("main")?.getAttribute("data-link") !== "live" && !host.querySelector('[aria-label="Stop this turn"]'), "reconnect remains last-known until its new snapshot arrives");
  await React.act(async () => {
    emit(`event: master\ndata: ${JSON.stringify(snapshot("m-next"))}\n\nid: 11\nevent: entry\ndata: ${JSON.stringify(entry(11))}\n\nid: 12\nevent: entry\ndata: ${JSON.stringify(entry(12))}\n\n`);
  });
  await settle();
  assert(JSON.stringify(thread.get().data?.entries.map((row) => row.seq)) === "[10,11,12]", "resume has no gap or duplicate despite overlapping replay");
  assert(observed.some((row) => row.seq === 12) && observed.filter((row) => row.seq === 12).every((row) => row.master === "m-next"), "reconnected replay follows the new current-state snapshot");
  off();
  await React.act(async () => { root.unmount(); });
  await win.happyDOM.close();
  console.log("conversation stream state checks passed");
}
void main();
