/** CAD-1853: real Threads/Home consumers, deterministic page and SSE transport only. */
import type * as HappyDOM from "happy-dom";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as Threads from "../src/features/threads/ThreadsPage";
import type * as HomeModule from "../src/features/home/Home";
import type * as Stores from "../src/lib/resources";
import type * as Location from "../src/lib/useLocation";
import type * as ThreadsClient from "../src/features/threads/threadsClient";
import type * as ThreadModel from "../src/features/home/thread";
import type { ThreadEntry } from "../src/features/home/thread";
declare function require<T = any>(name: string): T;
const { Window } = require<typeof HappyDOM>("happy-dom");
const win = new Window({ url: "http://localhost/threads?c=conv-1" });
for (const key of ["window", "document", "navigator", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "Event", "MouseEvent", "MutationObserver", "ResizeObserver", "location", "history", "localStorage", "sessionStorage"] as const)
  Object.defineProperty(globalThis, key, { value: key === "window" ? win : win[key], configurable: true });
for (const key of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"] as const)
  Object.defineProperty(globalThis, key, { value: win[key].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  if (id === "react-markdown") return { __esModule: true, default: ({ children }: { children: ReactModule.ReactNode }) => children };
  if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
  return originalRequire.apply(this, arguments);
};
const React = require<typeof ReactModule>("react");
const { createRoot } = require<typeof ReactDOM>("react-dom/client");
const { LocaleProvider } = require<typeof Locale>("../src/lib/locale");
const ThreadsPage = require<typeof Threads>("../src/features/threads/ThreadsPage").default;
const Home = require<typeof HomeModule>("../src/features/home/Home").default;
const { resources } = require<typeof Stores>("../src/lib/resources");
const { navigate } = require<typeof Location>("../src/lib/useLocation");
const { noteThreadsChanged } = require<typeof ThreadsClient>("../src/features/threads/threadsClient");
const { addPending, settlePending } = require<typeof ThreadModel>("../src/features/home/thread");
const { setImmediate: nextTurn } = require<{ setImmediate: () => Promise<void> }>("node:timers/promises");
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
const row = (seq: number, text: string, payload: Record<string, unknown> | null = null): ThreadEntry => ({ seq, text, payload, role: "operator", kind: "message" });
const cloud = row(10, "Private cloud copy", { cloud_agent: { operation: "op-1" } });
const request = row(11, "Private cloud request", { source: "cloud_agent" });
const ordinary = row(20, "Ordinary stored history", { delivery: { state: "completed" } });
const thread = { id: "conv-1", alias: "local-worker", created: "", updated: "" };
const requests: URL[] = [];
let dmRead: (url: URL) => Promise<Response> = async () => json({ thread, entries: [], cursor: 150, scanned_before: 101, more_before: true, cloud_withheld: "revoked" });
const homePage: Record<string, unknown> = { thread: { ...thread, id: "home-1", alias: "master" }, entries: [cloud, request, ordinary], cursor: 100, scanned_before: 10, more_before: true, more_after: false, reply_seq: 80 };
let homeEarlier: () => Promise<Response> = async () => json({ thread: homePage.thread, entries: [], more_before: false });
const streams: ReadableStreamDefaultController<Uint8Array>[] = [];
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "http://localhost");
  requests.push(url);
  if ((init?.method ?? "GET") !== "GET") throw new Error(`Unexpected write ${url.pathname}`);
  switch (url.pathname) {
    case "/api/threads/local-worker": return dmRead(url);
    case "/api/threads/master": return url.searchParams.has("before") ? homeEarlier() : json(homePage);
    case "/api/threads/master/stream": {
      const body = new ReadableStream<Uint8Array>({ start(controller) {
        streams.push(controller);
        controller.enqueue(new TextEncoder().encode('event: master\ndata: {"alias":"master","live":true,"turn":null,"queue":[]}\n\n'));
        init?.signal?.addEventListener("abort", () => controller.close(), { once: true });
      } });
      return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
    }
    case "/api/meta": return json({ operator: true, read_only: false });
    case "/api/agents": return json({ daemon: "reachable", agents: [{ alias: "master", state: "idle" }], totals: null, by_issue: {} });
    case "/api/issues": return json({ issues: [] });
    case "/api/master/state": return json({ alias: "master", provider: "fixture", endpoint_kind: "fixture", live: true, turn: null, queue: [] });
    case "/api/human-threads/me": return json({ human: null });
    case "/api/work-conversations": return json({ conversations: [{ id: "conv-1", kind: "operator", participants: [
      { uid: "operator", alias: null, kind: "operator", removed: false },
      { uid: "agt-local", alias: "local-worker", kind: "agent", removed: false },
    ], last: null, last_activity: 0 }] });
    default: throw new Error(`Unexpected read ${url.pathname}`);
  }
};
async function main() {
  const host = document.createElement("div"); document.body.append(host);
  const root = createRoot(host);
  const settle = async () => { for (let i = 0; i < 12; i++) await React.act(async () => { await nextTurn(); }); };
  const mountThreads = async () => {
    await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(ThreadsPage, { operator: true }))); });
    await settle();
  };
  const click = async (label: string) => {
    const button = Array.from(host.querySelectorAll<HTMLButtonElement>("button")).find((b) => b.textContent === label);
    assert(button, `missing button ${label}`);
    await React.act(async () => { button.click(); });
    await settle();
  };
  const tick = async () => { await React.act(async () => { noteThreadsChanged(); }); await settle(); };
  const timeline = () => host.querySelector(".threads-messages")!;
  const unavailable = (node: Element) => Array.from(node.querySelectorAll('[role="status"]')).some((n) => n.textContent === "Cloud content unavailable");
  await mountThreads();
  assert(unavailable(timeline()) && !timeline().textContent?.includes("No messages yet"), "a fully withheld DM has an accessible notice, not a complete-empty claim");
  dmRead = async (url) => {
    assert(url.searchParams.get("before") === "101", "earlier uses the raw denied tail boundary");
    return json({ thread, entries: [], cursor: 100, scanned_before: 51, more_before: true, cloud_withheld: "revoked" });
  };
  await click("Load earlier");
  let releaseOld: ((response: Response) => void) | undefined;
  dmRead = (url) => {
    assert(url.searchParams.get("after") === "50", "empty refresh continues from the backward scan");
    return new Promise((resolve) => { releaseOld = resolve; });
  };
  await tick();
  assert(releaseOld, "old-scope read is in flight");
  dmRead = async (url) => {
    assert(url.searchParams.get("tail") === "1" && !url.searchParams.has("after"), "scope reset opens a fresh tail, never the old empty-window cursor");
    return json({ thread, entries: [row(190, "Other scope ordinary history")], cursor: 250, scanned_before: 201, more_before: true });
  };
  await React.act(async () => { navigate("/threads?c=conv-1&project=other"); });
  await settle();
  await React.act(async () => { releaseOld!(json({ thread, entries: [row(999, "Old scope cloud", { cloud_agent: {} })], cursor: 999, cloud_withheld: "revoked" })); });
  await settle();
  assert(!unavailable(timeline()) && timeline().textContent?.includes("Other scope ordinary history") && !timeline().textContent?.includes("Old scope cloud"), "stale scope data and status cannot return after reset");

  await React.act(async () => { root.render(null); navigate("/threads?c=conv-1"); });
  dmRead = async () => json({ thread, entries: [cloud, request, ordinary], cursor: 100, more_before: false, more_after: false });
  await mountThreads();
  assert(timeline().textContent?.includes(cloud.text) && timeline().textContent?.includes(request.text), "both provenance shapes are visible before denial");
  dmRead = async () => json({ thread, entries: [{ ...ordinary, text: "Updated ordinary history", payload: { delivery: { state: "running" } } }], cursor: 150, more_after: false, cloud_withheld: "revoked" });
  await tick();
  assert(unavailable(timeline()) && !timeline().textContent?.includes(cloud.text) && !timeline().textContent?.includes(request.text), "denied refresh evicts cached cloud entries rather than merge-retaining them");
  assert(timeline().textContent?.includes("Updated ordinary history") && timeline().textContent?.includes("Running"), "ordinary history and delivery updates survive denial");
  dmRead = async (url) => {
    assert(url.searchParams.get("after") === "9", "restoration refresh retains the pre-eviction window start");
    return json({ thread, entries: [cloud, request, ordinary], cursor: 150, more_after: false });
  };
  await tick();
  assert(!unavailable(timeline()) && timeline().querySelectorAll("article").length === 3 && timeline().textContent?.includes(cloud.text), "clean pages restore cloud rows without duplicates and clear the notice");

  await React.act(async () => { root.render(null); navigate("/"); resources.masterThread.reset(); });
  await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(Home, {
    readOnly: false, hosted: false, overview: resources.overview.get(), onOpenIssue: () => undefined,
    onOpenDecision: () => undefined, overviewHref: "/overview", permissionsHref: "/permissions", resolveAppPath: () => null,
  }))); });
  await settle();
  assert(streams.length === 1 && host.textContent?.includes(cloud.text), "actual Home loads cached cloud history and opens its real stream consumer");
  await React.act(async () => { resources.masterThread.write((s) => settlePending(addPending(s, "pending", "Ordinary pending write", 1), "pending", { ok: false, error: "offline" })); });
  const emit = async (data: string) => {
    await React.act(async () => { streams.at(-1)!.enqueue(new TextEncoder().encode(`event: cloud_withheld\ndata: ${data}\n\n`)); });
    await settle();
  };
  await emit('{"cloud_withheld":"revoked"}');
  assert(unavailable(host) && !host.textContent?.includes(cloud.text) && !host.textContent?.includes(request.text), "status-only SSE evicts cloud rows on the actual Home surface");
  assert(host.textContent?.includes(ordinary.text) && host.textContent?.includes("Ordinary pending write"), "Home retains ordinary history and optimistic pending writes");
  await emit("{}");
  assert(unavailable(host), "malformed SSE cannot clear Home's unavailable state");
  await emit('{"cloud_withheld":null}');
  assert(!unavailable(host) && host.textContent?.includes(cloud.text) && host.textContent?.includes("Ordinary pending write"), "null SSE restores through a fresh page without erasing pending writes");
  assert(requests.filter((u) => u.pathname === "/api/threads/master" && u.searchParams.get("tail") === "1").length >= 2, "Home restoration actually rereads the tail below the stream resume cursor");

  await React.act(async () => { resources.masterThread.write((s) => ({ ...s!, entries: [], pending: [], older: 51, moreBefore: true })); });
  await emit('{"cloud_withheld":"revoked"}');
  assert(unavailable(host) && host.querySelector(".chat-empty") === null, "empty Home history keeps the notice instead of normal empty-history examples");
  let releaseEarlier: ((response: Response) => void) | undefined;
  let earlierReads = 0;
  // Tests target ES2023, before Promise.withResolvers is in the configured lib.
  homeEarlier = () => {
    earlierReads++;
    return new Promise((resolve) => { releaseEarlier = resolve; });
  };
  await click("Load earlier messages");
  assert(releaseEarlier, "empty Home still offers real backward navigation");
  await emit('{"cloud_withheld":"revoked"}');
  await React.act(async () => { releaseEarlier!(json({ thread: homePage.thread, entries: [row(1, "Earlier ordinary history"), row(2, "Stale private cloud", { cloud_agent: {} })], scanned_before: 1, more_before: false })); });
  await settle();
  assert(unavailable(host) && host.textContent?.includes("Earlier ordinary history") && !host.textContent?.includes("Stale private cloud"), "a newer status frame wins over an in-flight clean earlier page while its ordinary rows merge");
  await React.act(async () => {
    resources.masterThread.write((s) => ({ ...s!, entries: [row(60, "Old Home scope")], older: 51, moreBefore: true }));
  });
  await click("Load earlier messages");
  assert(earlierReads === 2, "the old Home scope has a new earlier read in flight");
  await React.act(async () => {
    resources.masterThread.write((s) => ({ ...s!, conversation: "home-2", cloudWithheld: null, entries: [row(200, "New Home scope")], moreBefore: false }));
    releaseEarlier!(json({ thread: homePage.thread, entries: [row(2, "Old Home cloud", { cloud_agent: {} })], cloud_withheld: "revoked", scanned_before: 1, more_before: false }));
  });
  await settle();
  assert(!unavailable(host) && host.textContent?.includes("New Home scope") && !host.textContent?.includes("Old Home cloud"), "old Home scope replies cannot reapply data or unavailable status");
  assert(resources.masterThread.get().data?.replySeq === 80, "withholding and restoration preserve the complete reply watermark");
  await React.act(async () => { root.unmount(); });
  loader.prototype.require = originalRequire;
  await win.happyDOM.close();
  console.log("cloud withholding consumer checks passed");
}
void main();
