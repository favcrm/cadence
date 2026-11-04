/**
 * CAD-2103: the Browser tab in the agent canvas. The real drawer, panel and
 * store are mounted in happy-dom against a stateful fake of the board's
 * /api/browser routes, with a manual clock so polling is exact.
 */
declare function require(name: string): any;
declare const process: any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(a: unknown, e: unknown, why: string) {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/", settings: { disableIframePageLoading: true } });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLButtonElement", "HTMLTextAreaElement", "HTMLIFrameElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame", "getSelection"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};

// ---- manual clock ----
let now = Date.parse("2026-11-04T08:00:00Z");
Date.now = () => now;
const timers = new Map<number, { at: number; fn: () => void; every?: number }>();
let timerSeq = 0;
(globalThis as any).setTimeout = (fn: () => void, ms = 0) => {
  const id = ++timerSeq;
  timers.set(id, { at: now + ms, fn });
  return id;
};
(globalThis as any).clearTimeout = (id: number) => void timers.delete(id);
(globalThis as any).setInterval = (fn: () => void, ms = 0) => {
  const id = ++timerSeq;
  timers.set(id, { at: now + ms, fn, every: ms });
  return id;
};
(globalThis as any).clearInterval = (id: number) => void timers.delete(id);

const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const CanvasDrawer = require("../src/features/canvas/CanvasDrawer").default;
const { BrowserCard } = require("../src/features/canvas/BrowserPanel") as typeof import("../src/features/canvas/BrowserPanel");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const canvasStore = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");
const store = require("../src/features/canvas/browserStore") as typeof import("../src/features/canvas/browserStore");
const client = require("../src/features/canvas/browserClient") as typeof import("../src/features/canvas/browserClient");

const settle = () => React.act(async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); });
async function advance(ms: number) {
  const end = now + ms;
  for (;;) {
    let next: [number, { at: number; fn: () => void; every?: number }] | undefined;
    for (const e of timers) if (e[1].at <= end && (!next || e[1].at < next[1].at)) next = e;
    if (!next) break;
    now = Math.max(now, next[1].at);
    if (next[1].every) next[1].at = now + next[1].every;
    else timers.delete(next[0]);
    await React.act(async () => { next![1].fn(); });
    await settle();
  }
  now = end;
  await settle();
}

// ---- the board, stateful ----
const VIEWER = "https://live.viewer-example.test/session/TOKEN-9f3a1c?sig=SECRET-7d2e";
type Mode = "ok" | "unavailable" | "404" | "network" | "malformed";
const board = {
  mode: "ok" as Mode,
  body: { state: "none", signedIn: false, url: null, instructions: null } as Record<string, unknown>,
  gets: 0,
  views: 0,
  stops: 0,
  stopStatus: 200,
  viewStatus: 200,
  viewUrl: VIEWER,
  stopHold: false,
  release: null as null | (() => void),
  getHang: false,
  viewHang: false,
  getHold: false,
  releaseGet: null as null | (() => void),
  viewHold: false,
  releaseView: null as null | (() => void),
  late: false,
  conversation: "c1",
};
globalThis.fetch = (async (input: unknown, init?: { method?: string; signal?: AbortSignal }) => {
  const url = new URL(String(input), "http://localhost");
  const method = init?.method ?? "GET";
  if (url.pathname === "/api/browser" && method === "GET") {
    board.gets++;
    equal(url.searchParams.get("conversation"), board.conversation, "the read names the conversation in view");
    if (board.getHang) await new Promise<void>((_, reject) => init?.signal?.addEventListener("abort", () => reject(new Error("aborted"))));
    if (board.getHold) {
      // The answer is what the board said when it was asked, however late it lands.
      const snap = JSON.stringify(board.body);
      board.getHold = false;
      await new Promise<void>((resolve) => { board.releaseGet = resolve; });
      return new Response(snap, { status: 200 });
    }
    if (board.mode === "404") return new Response("{}", { status: 404 });
    if (board.mode === "network") throw new TypeError("network down");
    if (board.mode === "malformed") return new Response(JSON.stringify({ state: "weird" }), { status: 200 });
    if (board.mode === "unavailable") return new Response(JSON.stringify({ state: "unavailable" }), { status: 200 });
    return new Response(JSON.stringify(board.body), { status: 200 });
  }
  if (url.pathname === "/api/browser/view" && method === "POST") {
    board.views++;
    if (board.viewHang) await new Promise<void>((_, reject) => init?.signal?.addEventListener("abort", () => reject(new Error("aborted"))));
    if (board.viewHold) await new Promise<void>((resolve) => { board.releaseView = resolve; });
    if (board.viewStatus === 409) return new Response(JSON.stringify({ error: "not_in_handoff" }), { status: 409 });
    return new Response(JSON.stringify({ url: board.viewUrl }), { status: board.viewStatus });
  }
  if (url.pathname === "/api/browser/stop" && method === "POST") {
    board.stops++;
    if (board.stopHold) await new Promise<void>((resolve) => { board.release = resolve; });
    if (board.stopStatus === 503) return new Response(JSON.stringify({ error: "provider_unavailable" }), { status: 503 });
    board.body = { state: "ended", signedIn: false, url: null, instructions: null };
    return new Response(JSON.stringify({ status: "stopped" }), { status: 200 });
  }
  if (url.pathname === "/api/member-assistant/state") {
    return new Response(JSON.stringify({ conversation_id: "cm1", assistant: { org: "o", sub: "u", assistant_id: "a", alias: "assistant", conversation_id: "cm1", state: "ready", bootstrap_state: null } }), { status: 200 });
  }
  if (url.pathname === "/api/member-assistant/history") {
    const entries: Record<string, unknown>[] = [
      { seq: 1, role: "member", kind: "message", text: "compare the suppliers", message: "m1" },
      { seq: 2, role: "agent", kind: "artifact", text: "", message: "m1", payload: { id: "art-html1", version: 1, kind: "html", title: "Supplier comparison", bytes: 12 } },
      { seq: 3, role: "agent", kind: "turn_result", text: "Done.", message: "m1" },
    ];
    if (board.late) {
      // A second turn the person started, whose artifact arrives after the chat mounted.
      entries.push(
        { seq: 4, role: "member", kind: "message", text: "and the other one", message: "m2" },
        { seq: 5, role: "agent", kind: "artifact", text: "", message: "m2", payload: { id: "art-late1", version: 1, kind: "html", title: "Late artifact", bytes: 12 } },
        { seq: 6, role: "agent", kind: "turn_result", text: "Done again.", message: "m2" },
      );
    }
    return new Response(JSON.stringify({ conversation: { id: "cm1", alias: "assistant" }, entries, more_before: false }), { status: 200 });
  }
  if (url.pathname.startsWith("/api/artifacts/")) {
    return new Response("<p>hello</p>", { status: 200, headers: { "Content-Type": "text/html" } });
  }
  throw new Error(`Unexpected request ${method} ${url.pathname}`);
}) as typeof fetch;

const el = (root: ParentNode, sel: string) => root.querySelector(sel) as HTMLElement | null;
const all = (root: ParentNode, sel: string) => Array.from(root.querySelectorAll(sel)) as HTMLElement[];
const click = async (n: HTMLElement) => { await React.act(async () => { n.dispatchEvent(new MouseEvent("click", { bubbles: true })); }); await settle(); };
const esc = async (n: HTMLElement) => { await React.act(async () => { n.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })); }); await settle(); };

async function mount() {
  const hostEl = document.createElement("div");
  document.body.append(hostEl);
  const root = createRoot(hostEl);
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement("div", { "data-thread": "" }, React.createElement(BrowserCard)), React.createElement(CanvasDrawer)));
  });
  await settle();
  return { host: hostEl, unmount: async () => { await React.act(async () => { root.unmount(); }); hostEl.remove(); } };
}
function reset(over: Partial<typeof board> = {}) {
  timers.clear();
  Object.assign(board, { mode: "ok", body: { state: "none", signedIn: false, url: null, instructions: null }, gets: 0, views: 0, stops: 0, stopStatus: 200, stopHold: false, release: null, late: false, getHang: false, viewHang: false, getHold: false, releaseGet: null, viewHold: false, releaseView: null, viewStatus: 200, viewUrl: VIEWER, conversation: "c1" }, over);
  store.resetBrowser();
  canvasStore.resetCanvas("c1", "home", "home");
  (win as any).innerWidth = 1600;
}
async function start(body: Record<string, unknown>, mode: Mode = "ok") {
  reset({ body, mode });
  const m = await mount();
  await React.act(async () => { store.feedBrowser("c1"); });
  await settle();
  return m;
}
const tab = (m: { host: HTMLElement }) => el(m.host, "#bz-tab");
const panel = (m: { host: HTMLElement }) => el(m.host, "#bz-panel");
const openTab = async (_m: { host: HTMLElement }) => {
  await React.act(async () => { store.openBrowser(); });
  await settle();
};
const drawer = (m: { host: HTMLElement }) => el(m.host, "[data-canvas-drawer]");
const text = (n: HTMLElement | null) => n?.textContent ?? "";
const btn = (m: { host: HTMLElement }, act: string) => el(m.host, `[data-bz-act="${act}"]`);

// ===== the contract's shapes =====
equal(client.parseBrowserRead({ state: "unavailable" }), { kind: "unavailable" }, "unavailable parses");
equal(client.parseBrowserRead({ state: "handoff", signedIn: true, url: "https://a.test/", instructions: "x" }), { kind: "session", session: { state: "handoff", signedIn: true, url: "https://a.test/", instructions: "x" } }, "a handoff parses");
for (const bad of [null, "x", { state: "open" }, { state: "active", url: 3 }, { state: "active", signedIn: "yes" }, {}]) {
  equal(client.parseBrowserRead(bad), null, `malformed ${JSON.stringify(bad)} is refused, not invented`);
}

// ===== a board with no browser offers no tab at all =====
for (const mode of ["unavailable", "404", "network", "malformed"] as const) {
  const m = await start({ state: "active", url: "https://a.test/" }, mode);
  equal(store.tabVisible(store.browserSnapshot()), false, `${mode}: no tab`);
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  assert(drawer(m), `${mode}: the drawer still opens for an artifact`);
  assert(!tab(m) && !el(m.host, ".cv-tabs"), `${mode}: with no Browser tab and one artifact there is no strip`);
  assert(!el(m.host, ".bz-card"), `${mode}: no thread card`);
  equal(board.gets, 1, `${mode}: one read`);
  await advance(60_000);
  if (mode === "network" || mode === "malformed") {
    assert(board.gets >= 2, `${mode}: a board that has not answered is tried again, slowly (${board.gets} reads)`);
    assert(board.gets <= 3, `${mode}: and not quickly (${board.gets} reads a minute on)`);
  } else {
    equal(board.gets, 1, `${mode}: a definite answer is not asked again`);
  }
  await m.unmount();
}

// ===== none and ended keep the tab, with their empty states, and do not poll =====
for (const [state, words] of [["none", "No browser session"], ["ended", "Session ended"]] as const) {
  const m = await start({ state, url: null });
  assert(store.tabVisible(store.browserSnapshot()), `${state}: the tab stays`);
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  assert(tab(m), `${state}: the Browser tab is in the strip`);
  await click(tab(m)!);
  assert(text(panel(m)).includes(words), `${state}: says "${words}": ${text(panel(m))}`);
  assert(!el(m.host, "[data-bz-dock]"), `${state}: no actions`);
  await advance(60_000);
  equal(board.gets, 1, `${state}: no polling`);
  await m.unmount();
}
// The thread card is absent for `none` and present once there is a session.
{
  const m = await start({ state: "none" });
  assert(!el(m.host, ".bz-card"), "no card for an empty tab");
  board.body = { state: "active", signedIn: false, url: "https://portal.example.test/orders", instructions: null };
  await React.act(async () => { await store.refreshBrowser(); });
  await settle();
  assert(el(m.host, ".bz-card"), "a session brings the card");
  await m.unmount();
}

// ===== polling starts with a live session and stops when it ends =====
{
  assert(store.pollDelay({ ...store.browserSnapshot(), conversation: "c1", availability: "available", session: { state: "none", signedIn: false, url: null, instructions: null }, stop: "idle" }) === null, "none does not poll");
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders", instructions: null });
  equal(board.gets, 1, "the first read");
  await advance(3000);
  equal(board.gets, 2, "an active session is read again");
  await advance(6000);
  equal(board.gets, 4, "and keeps being read");
  board.body = { state: "ended", signedIn: false, url: null, instructions: null };
  await advance(3000);
  equal(board.gets, 5, "the read that finds it ended is the last");
  await advance(30_000);
  equal(board.gets, 5, "an ended session is not polled");
  await m.unmount();
}

// ===== a takeover: the board notices the live view's Done control =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Please sign in." });
  await openTab(m);
  assert(text(panel(m)).includes("Needs you"), "handoff reads Needs you");
  const before = board.gets;
  await advance(2000);
  assert(board.gets === before + 1, "a takeover is polled at its own pace");
  assert(!text(panel(m)).includes("Finish"), "there is no Finish button: the live view's own Done control ends a takeover");
  assert(!all(m.host, "button").some((b) => /finish|request/i.test(b.textContent ?? "")), "and no control to request a takeover");
  // Open the live view, then the provider reports the takeover over.
  await click(btn(m, "view")!);
  const frame = el(m.host, "iframe[data-bz-frame]") as HTMLIFrameElement;
  assert(frame && frame.getAttribute("src") === VIEWER, "the live view frames exactly the URL the board returned");
  await React.act(async () => { frame.dispatchEvent(new win.Event("load")); });
  await settle();
  assert(!text(panel(m)).includes("in control"), "a loaded cross-origin frame proves nothing about control, and the panel does not claim it");
  assert(text(el(m.host, ".bz-viewer-bar")).includes("Live view"), "it says Live view");
  assert(btn(m, "view") && text(btn(m, "view")).includes("Open live view again"), "and Open live view again stays available while a frame shows");
  board.body = { state: "active", signedIn: true, url: "https://portal.example.test/orders", instructions: null };
  await advance(2000);
  assert(!el(m.host, "iframe"), "when the board sees the takeover over, the frame is torn down");
  assert(text(panel(m)).includes("Handed back. The assistant is reading again."), "and the transition is shown");
  assert(text(panel(m)).includes("Reading"), "now reading");
  await advance(3000);
  assert(board.gets > before, "an active session carries on being polled");
  await m.unmount();
}

// ===== Stop: confirms only when signed in or during a takeover =====
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await openTab(m);
  board.stopHold = true; // hold the request: a hung page
  await click(btn(m, "stop")!);
  equal(board.stops, 1, "unsigned active: Stop goes straight to the board, no confirm");
  assert(!btn(m, "go"), "no confirm shown");
  assert(text(el(m.host, "[data-bz-dock]")).includes("Stopping…"), "it shows stopping in progress while the request hangs");
  await advance(65_000);
  assert(/65 s/.test(text(el(m.host, "[data-bz-dock]"))), `with the time so far: ${text(el(m.host, "[data-bz-dock]"))}`);
  assert(board.gets > 2, "a stop in progress is polled");
  await React.act(async () => { board.release!(); });
  await settle();
  assert(text(panel(m)).includes("Session ended"), "stopped: the session ended");
  await m.unmount();
}
for (const body of [
  { state: "active", signedIn: true, url: "https://portal.example.test/orders" },
  { state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." },
]) {
  const m = await start(body);
  await openTab(m);
  await click(btn(m, "stop")!);
  equal(board.stops, 0, `${body.state}/${body.signedIn}: Stop asks first`);
  assert(btn(m, "go") && btn(m, "keep"), "with Stop session and Keep it running");
  // Esc cancels the confirm and does not close the canvas.
  await esc(btn(m, "keep")!);
  assert(!btn(m, "go"), "Esc cancels the confirm");
  assert(drawer(m), "and the first Esc leaves the canvas open");
  await click(btn(m, "stop")!);
  await click(btn(m, "keep")!);
  equal(board.stops, 0, "Keep it running stops nothing");
  // 503: not stopped.
  board.stopStatus = 503;
  await click(btn(m, "stop")!);
  await click(btn(m, "go")!);
  equal(board.stops, 1, "confirmed: the board is asked");
  assert(text(panel(m)).includes("Couldn't stop the session. It may still be running."), "a 503 is shown as not stopped");
  assert(!text(panel(m)).includes("Session ended"), "and not as ended");
  await m.unmount();
}

// ===== closing the panel or hiding the tab never ends the session =====
{
  const m = await start({ state: "active", signedIn: true, url: "https://portal.example.test/orders" });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  await click(tab(m)!);
  assert(!el(m.host, "[data-canvas-drawer] .cv-pane:not([hidden])"), "the Browser tab is in front");
  await click(el(m.host, '[aria-label="Close canvas"]')!);
  assert(!drawer(m), "the drawer closes");
  equal(board.stops, 0, "closing stops nothing");
  equal(store.browserSnapshot().session?.state, "active", "the session is still active");
  await click(el(m.host, ".bz-card")!);
  assert(drawer(m) && tab(m), "the thread card reopens it on the Browser tab");
  await click(el(m.host, '[aria-label="Hide Browser tab. The session keeps running."]')!);
  assert(!tab(m), "the x hides the tab");
  equal(board.stops, 0, "hiding stops nothing");
  const reads = board.gets;
  await advance(3000);
  assert(board.gets > reads, "the hidden session is still watched");
  await click(el(m.host, ".bz-card")!);
  assert(tab(m), "the card brings the tab back");
  await m.unmount();
}

// ===== artifact state survives switching to the Browser tab and back =====
{
  const m = await start({ state: "none" });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  const pane = el(m.host, ".cv-pane")!;
  const source = all(m.host, ".cv-view button")[1];
  await click(source);
  assert(source.getAttribute("aria-pressed") === "true", "HTML source view chosen");
  await click(tab(m)!);
  assert(pane.hidden, "the artifact pane is hidden, not unmounted");
  await click(all(m.host, '[role="tab"]')[0]);
  assert(el(m.host, ".cv-pane") === pane && !pane.hidden, "the same pane comes back");
  assert(all(m.host, ".cv-view button")[1].getAttribute("aria-pressed") === "true", "with the HTML view as left");
  await m.unmount();
}

// ===== the viewer URL is a bearer credential =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  equal(board.views, 0, "no viewer URL is fetched until the person asks");
  await advance(10_000);
  equal(board.views, 0, "not by polling either");
  await click(btn(m, "view")!);
  equal(board.views, 1, "fetched once, on the click");
  const frame = el(m.host, "iframe[data-bz-frame]")!;
  assert(frame.getAttribute("src") === VIEWER, "the frame holds it");
  assert(frame.getAttribute("sandbox") === "allow-scripts allow-same-origin allow-forms", "in a sandboxed frame");
  const where = [
    JSON.stringify(store.browserSnapshot()),
    JSON.stringify(canvasStore.canvasSnapshot()),
    win.location.href,
    JSON.stringify({ ...win.localStorage }),
    JSON.stringify({ ...win.sessionStorage }),
    text(el(m.host, "[data-canvas-drawer]")),
    text(el(m.host, "[data-thread]")),
    all(m.host, "[title],[aria-label],[data-artifact],[href]").map((n) => `${n.getAttribute("title")}${n.getAttribute("aria-label")}${n.getAttribute("href")}`).join(""),
  ].join("\n");
  assert(!where.includes("TOKEN-9f3a1c") && !where.includes("SECRET-7d2e"), "the URL is in no store, storage, address bar, text or label");
  assert(!canvasStore.canvasSnapshot().tabs.some((t) => JSON.stringify(t).includes("viewer-example")), "nor in an artifact identity");
  // Close and reopen: the frame is gone and nothing was kept to reopen it with.
  await click(el(m.host, '[aria-label="Close canvas"]')!);
  assert(!el(m.host, "iframe"), "closing tears the frame down");
  await React.act(async () => { store.openBrowser(); });
  await settle();
  assert(!el(m.host, "iframe"), "reopening does not bring the URL back");
  equal(board.views, 1, "and nothing fetched it again");
  await click(btn(m, "view")!);
  equal(board.views, 2, "a new one is fetched only on a new click");
  await m.unmount();
}
// A frame is only ever pointed at an https page, and a refusal is not retried.
for (const bad of ["javascript:alert(1)", "http://live.test/x", "data:text/html,x", "not a url"]) {
  board.viewUrl = bad;
  let failed = false;
  try { await client.viewBrowser("c1"); } catch { failed = true; }
  assert(failed, `${bad} is refused`);
}
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  board.viewStatus = 409;
  await click(btn(m, "view")!);
  assert(text(panel(m)).includes("isn't asking for a takeover any more"), "409 not_in_handoff is said plainly");
  assert(!el(m.host, "iframe"), "and opens nothing");
  await m.unmount();
}

// ===== the assistant's words are plain text; the address is the real one =====
{
  const hostile = "<img src=x onerror=alert(1)> Go to <a href=\"https://evil.example.test\">paypal.com</a> and https://evil.example.test/now";
  const m = await start({ state: "handoff", signedIn: false, url: "https://xn--80ak6aa92e.test/sign-in?x=1", instructions: hostile });
  await openTab(m);
  const says = el(m.host, "#bz-says-t")!;
  equal(says.textContent, hostile, "the words show exactly as written");
  equal(says.children.length, 0, "as one text node: no element came out of them");
  assert(!el(panel(m)!, "img[src='x']") && !all(panel(m)!, "a").length, "no image, no link anywhere in the panel");
  const host = el(m.host, ".bz-url .bz-host")!;
  equal(host.tagName, "B", "the host is bold");
  equal(host.textContent, "xn--80ak6aa92e.test", "and is the real host, punycode as it is");
  assert(!text(panel(m)).includes("Warning") && !el(m.host, ".bz-warn"), "no warning chip: that is the board's judgement, not the UI's");
  await m.unmount();
}


const raf = async () => { for (let i = 0; i < 3; i++) await new Promise<void>((resolve) => win.requestAnimationFrame(() => resolve())); await settle(); };
const violation = (directive: string, blocked: string, disposition = "enforce") => {
  const ev = new win.Event("securitypolicyviolation", { bubbles: true }) as any;
  Object.defineProperty(ev, "effectiveDirective", { value: directive });
  Object.defineProperty(ev, "blockedURI", { value: blocked });
  Object.defineProperty(ev, "disposition", { value: disposition });
  document.dispatchEvent(ev);
};

// ===== a frame the board's CSP refuses is an error now, and the retry is fresh =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  assert(el(m.host, "iframe[data-bz-frame]"), "the frame is up, still loading");
  await React.act(async () => {
    violation("frame-src", VIEWER);
  });
  await settle();
  assert(!el(m.host, "iframe"), "a frame-src violation tears the frame down without waiting for a load");
  assert(text(panel(m)).includes("Couldn't open the live view"), "and says so");
  assert(btn(m, "view") && text(btn(m, "view")).includes("Open live view again"), "with a retry");
  await click(btn(m, "view")!);
  equal(board.views, 2, "the retry fetches a fresh URL; the refused one is not reused");
  await m.unmount();
}

// ===== a failed read during a takeover keeps the frame, and nothing remounts it =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  const frame = el(m.host, "iframe[data-bz-frame]")!;
  board.mode = "network";
  await advance(2000);
  assert(el(m.host, "iframe[data-bz-frame]") === frame, "one failed poll leaves the very same frame in place");
  assert(text(panel(m)).includes("Couldn't load the browser state"), "and shows the read error as a note");
  assert(btn(m, "stop"), "Stop is still there");
  board.mode = "ok";
  await advance(2000);
  assert(el(m.host, "iframe[data-bz-frame]") === frame, "recovery does not remount it either");
  equal(board.views, 1, "no second viewer URL was fetched without a click");
  assert(!text(panel(m)).includes("Couldn't load the browser state"), "the note clears");
  await m.unmount();
}

// ===== once a session has been live, unavailable and 404 are failed reads =====
for (const mode of ["404", "unavailable"] as const) {
  const m = await start({ state: "active", signedIn: true, url: "https://portal.example.test/orders" });
  await openTab(m);
  board.mode = mode;
  await advance(3000);
  assert(store.tabVisible(store.browserSnapshot()), `${mode} after a live session: the tab stays`);
  assert(btn(m, "stop"), `${mode}: so does Stop`);
  assert(text(panel(m)).includes("Couldn't load the browser state"), `${mode}: with a retry note`);
  const reads = board.gets;
  await advance(6000);
  assert(board.gets > reads, `${mode}: and polling carries on`);
  board.mode = "ok";
  await advance(3000);
  assert(!text(panel(m)).includes("Couldn't load"), `${mode}: recovers`);
  await m.unmount();
}

// ===== a 503 on Stop during a takeover keeps the normal actions, and clears with the phase =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  board.stopStatus = 503;
  await click(btn(m, "stop")!);
  await click(btn(m, "go")!);
  const dock = el(m.host, "[data-bz-dock]")!;
  assert(text(dock).includes("Couldn't stop the session"), "the failure is shown");
  assert(btn(m, "view"), "above Open live view, which is still offered");
  assert(el(m.host, "iframe[data-bz-frame]"), "and the live view was not destroyed by the failed stop");
  assert(text(btn(m, "stop")).includes("Try stopping again"), "with a way to try again");
  board.body = { state: "active", signedIn: false, url: "https://portal.example.test/orders", instructions: null };
  await advance(2000);
  assert(!text(el(m.host, "[data-bz-dock]")).includes("Couldn't stop"), "a phase change clears the old failure");
  await m.unmount();
}

// ===== a read that was already in flight cannot undo a confirmed stop =====
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await openTab(m);
  board.getHold = true;
  await advance(3000); // the poll asks; the board answers "active", late
  board.stopHold = true;
  await click(btn(m, "stop")!);
  board.body = { state: "ended", signedIn: false, url: null, instructions: null };
  await React.act(async () => { board.release!(); });
  await settle();
  assert(text(panel(m)).includes("Session ended"), "the stop landed");
  await React.act(async () => { board.releaseGet!(); });
  await settle();
  assert(text(panel(m)).includes("Session ended") && !btn(m, "stop"), "the late 'active' answer does not bring the session or Stop back");
  await m.unmount();
}
// ... nor can an answer for the conversation that was just left.
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  board.getHold = true;
  await advance(3000);
  board.conversation = "c2";
  board.body = { state: "none", signedIn: false, url: null, instructions: null };
  await React.act(async () => { store.feedBrowser("c2"); });
  await settle();
  await React.act(async () => { board.releaseGet!(); });
  await settle();
  equal(store.browserSnapshot().session?.state, "none", "the old conversation's late answer is dropped");
  await m.unmount();
}

// ===== a view request overtaken by the end of the takeover opens nothing =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  board.viewHold = true;
  await click(btn(m, "view")!);
  board.body = { state: "active", signedIn: true, url: "https://portal.example.test/orders", instructions: null };
  await advance(2000);
  // ... and a second takeover begins before the old request answers.
  board.body = { state: "handoff", signedIn: false, url: "https://login.supplier-example.test/again", instructions: "Again." };
  await advance(3000);
  equal(store.browserSnapshot().session?.state, "handoff", "the board is asking again");
  await React.act(async () => { board.releaseView!(); });
  await settle();
  assert(!el(m.host, "iframe[data-bz-frame]"), "the URL that arrives after its takeover ended is never framed, even into the next one");
  await m.unmount();
}

// ===== hiding the tab does not close the drawer over an artifact =====
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); canvasStore.closeDrawer(); });
  await settle();
  await React.act(async () => { store.openBrowser(); });
  await settle();
  assert(drawer(m) && !canvasStore.canvasSnapshot().open, "the Browser tab alone opened the drawer");
  await click(el(m.host, '[aria-label="Hide Browser tab. The session keeps running."]')!);
  assert(drawer(m), "hiding it leaves the artifact on screen");
  await m.unmount();
}

// ===== focus goes to the tab after a stop, and a long instruction keeps its caution =====
{
  const long = "Please sign in. ".repeat(40);
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: long });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  await click(tab(m)!);
  assert(el(m.host, "#bz-says-t")!.getAttribute("data-clamp") === "full", "a long instruction is clamped, with Show all");
  assert(text(panel(m)).includes("check where you are"), "and the caution is outside the clamp");
  assert(text(panel(m)).includes("Last page the assistant read") && !text(panel(m)).includes("You will be on this page"), "the address is the last page the assistant read, not a promise of where the person will be");
  await click(btn(m, "view")!);
  assert(el(m.host, "#bz-says-t")!.getAttribute("data-clamp") === "compact", "in the live view the clamp is compact");
  assert(text(panel(m)).includes("check where you are"), "and the caution is still shown there");
  await m.unmount();
}
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  await click(tab(m)!);
  board.stopHold = true;
  const stopBtn = btn(m, "stop")!;
  await React.act(async () => { stopBtn.focus(); });
  await click(stopBtn);
  await React.act(async () => { board.release!(); });
  await settle();
  equal(document.activeElement?.id, "bz-tab", "after a stop the focus is on the tab, not the page");
  await m.unmount();
}

// ===== the frame is never the board's own origin =====
{
  const real = (globalThis as any).location;
  Object.defineProperty(globalThis, "location", { value: { origin: "https://board.test" }, configurable: true, writable: true });
  board.viewUrl = "https://board.test/anything";
  let refused = false;
  try { await client.viewBrowser("c1"); } catch { refused = true; }
  Object.defineProperty(globalThis, "location", { value: real, configurable: true, writable: true });
  board.viewUrl = VIEWER;
  assert(refused, "a viewer URL on the board's own origin is refused");
}

// ===== a confirmed identity change drops the tabs and the session =====
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Private title", bytes: 12 }); });
  await settle();
  const { resetPrivateSession, allowPrivateReads } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  await React.act(async () => { resetPrivateSession(); allowPrivateReads(); });
  await settle();
  equal(canvasStore.canvasSnapshot().tabs.length, 0, "artifact tabs and their titles are gone");
  equal(store.browserSnapshot().conversation, null, "and the Browser tab's conversation");
  await m.unmount();
}


// ===== only an enforced frame-src violation for the viewer's origin counts =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  const frame = el(m.host, "iframe[data-bz-frame]")!;
  for (const [what, ...args] of [
    ["another directive", "script-src", VIEWER, "enforce"],
    ["another origin", "frame-src", "https://elsewhere.example.test/x", "enforce"],
    ["a report-only policy", "frame-src", VIEWER, "report"],
    ["no blocked URI", "frame-src", "inline", "enforce"],
  ] as const) {
    await React.act(async () => { violation(args[0], args[1], args[2]); });
    await settle();
    assert(el(m.host, "iframe[data-bz-frame]") === frame, `${what} does not tear the frame down`);
  }
  await React.act(async () => { violation("frame-src", VIEWER, "enforce"); });
  await settle();
  assert(!el(m.host, "iframe"), "an enforced frame-src violation for the viewer does");
  await m.unmount();
}

// ===== neutral wording; a finished load does not cancel the frame timeout; reopen keeps the old frame =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  const first = el(m.host, "iframe[data-bz-frame]")!;
  await React.act(async () => { first.dispatchEvent(new win.Event("load")); });
  await settle();
  await advance(30_000);
  assert(el(m.host, "iframe[data-bz-frame]") === first, "a frame that loaded is not reported as failed 20 s later");
  assert(text(panel(m)).includes("If it stays blank, open it again"), "the standing hint says what to do with a blank view");
  assert(!/Done|control/.test(text(el(m.host, "[data-bz-dock]"))), "and promises no Done control");
  // A failed reopen keeps what the person has.
  board.viewStatus = 500;
  await click(btn(m, "view")!);
  assert(el(m.host, "iframe[data-bz-frame]") === first, "a failed reopen leaves the old frame in place");
  assert(text(panel(m)).includes("The current view is unchanged"), "and says so");
  // A reopen in flight keeps it too, and the new URL replaces it.
  board.viewStatus = 200;
  board.viewHold = true;
  await click(btn(m, "view")!);
  assert(el(m.host, "iframe[data-bz-frame]") === first, "while the new URL is fetched the old frame stays");
  await React.act(async () => { board.releaseView!(); });
  await settle();
  const second = el(m.host, "iframe[data-bz-frame]")!;
  assert(second && second !== first, "the new URL gets a fresh frame");
  await m.unmount();
}

// ===== Enter on Open live view can never become Stop =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  const viewBtn = btn(m, "view")!;
  await React.act(async () => { viewBtn.focus(); });
  board.body = { state: "active", signedIn: true, url: "https://portal.example.test/orders", instructions: null };
  await advance(2000);
  const stopNow = btn(m, "stop")!;
  assert(stopNow && stopNow !== viewBtn, "Stop is a different node from the Open live view the person was on");
  assert(document.activeElement !== stopNow, "focus never carries onto Stop");
  await React.act(async () => { (document.activeElement as HTMLElement)?.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  equal(board.stops, 0, "an Enter or Space that lands after the takeover ended stops nothing");
  await m.unmount();
}

// ===== focus: rescue only a removed control, once, never from chat =====
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  await click(tab(m)!);
  const composer = document.createElement("textarea");
  document.body.append(composer);
  const stopBtn = btn(m, "stop")!;
  await React.act(async () => { stopBtn.focus(); });
  await React.act(async () => { composer.focus(); });
  board.stopHold = true;
  await click(stopBtn);
  await React.act(async () => { board.release!(); });
  await settle();
  assert(document.activeElement === composer, "focus that moved to the chat is left there when the control goes");
  await advance(9000);
  assert(document.activeElement === composer, "and stays there through every later poll");
  // The person clicks bare page, and the panel re-renders for some other reason:
  // the control they once held is not a claim on focus any more.
  await React.act(async () => { composer.blur(); });
  assert(document.activeElement === document.body, "focus is on the bare page");
  board.body = { state: "none", signedIn: false, url: null, instructions: null };
  await React.act(async () => { await store.refreshBrowser(); });
  await settle();
  assert(document.activeElement === document.body, "a later re-render does not pull it into the panel");
  composer.remove();
  await m.unmount();
}
{
  // A control removed while focus is elsewhere, and the platform said nothing
  // about focus leaving (no blur, no focusout): the focus that is somewhere is left alone.
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  await click(tab(m)!);
  const composer = document.createElement("textarea");
  document.body.append(composer);
  const stopBtn = btn(m, "stop")!;
  await React.act(async () => { stopBtn.focus(); });
  const mute = (e: Event) => e.stopImmediatePropagation();
  for (const type of ["blur", "focusout"]) win.document.addEventListener(type, mute, true);
  await React.act(async () => { composer.focus(); });
  for (const type of ["blur", "focusout"]) win.document.removeEventListener(type, mute, true);
  // The stop fails: the dock swaps its controls and a retry would be the rescue's target.
  board.stopHold = true;
  board.stopStatus = 503;
  await React.act(async () => { store.requestStop(); });
  await React.act(async () => { board.release!(); });
  await settle();
  assert(!stopBtn.isConnected && btn(m, "stop"), "the Stop button was replaced");
  assert(document.activeElement === composer, "and focus stayed where the person had put it, not on the retry");
  composer.remove();
  await m.unmount();
}
{
  // The same control removed with nowhere else to go: the tab, or with no strip the heading.
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await openTab(m);
  assert(!tab(m), "this path has no tab strip (the thread-card path)");
  const stopBtn = btn(m, "stop")!;
  await React.act(async () => { stopBtn.focus(); });
  board.stopHold = true;
  await click(stopBtn);
  await React.act(async () => { board.release!(); });
  await settle();
  equal(document.activeElement?.id, "bz-heading", "with no tab the panel's heading takes the focus, not the page");
  equal(el(m.host, "#bz-panel")!.getAttribute("aria-labelledby"), "bz-heading", "and the panel is labelled by something that exists");
  await m.unmount();
}
{
  // After the removal the claim is spent: a later poll does not pull focus back.
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await openTab(m);
  const stopBtn = btn(m, "stop")!;
  await React.act(async () => { stopBtn.focus(); });
  board.stopHold = false;
  await click(stopBtn);
  await settle();
  equal(document.activeElement?.id, "bz-heading", "rescued once");
  const sink = document.createElement("button");
  document.body.append(sink);
  await React.act(async () => { sink.focus(); });
  await React.act(async () => { sink.blur(); });
  board.body = { state: "none", signedIn: false, url: null, instructions: null };
  await advance(9000);
  assert(document.activeElement !== el(m.host, "#bz-heading"), "a spent claim never takes focus again");
  sink.remove();
  await m.unmount();
}

// ===== the retry is a rescue target only for focus that was lost =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  const frame = el(m.host, "iframe[data-bz-frame]")!;
  const composer = document.createElement("textarea");
  document.body.append(composer);
  await React.act(async () => { frame.focus(); });
  const mute = (e: Event) => e.stopImmediatePropagation();
  for (const type of ["blur", "focusout"]) win.document.addEventListener(type, mute, true);
  await React.act(async () => { composer.focus(); });
  for (const type of ["blur", "focusout"]) win.document.removeEventListener(type, mute, true);
  await React.act(async () => { violation("frame-src", VIEWER, "enforce"); });
  await settle();
  assert(!frame.isConnected && btn(m, "view"), "the refused frame is gone and a retry is offered");
  assert(document.activeElement === composer, "focus that was elsewhere is not pulled onto the retry");
  composer.remove();
  await m.unmount();
}
{
  // ... and focus that was on the frame itself is.
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  const frame = el(m.host, "iframe[data-bz-frame]")!;
  await React.act(async () => { frame.focus(); });
  await React.act(async () => { violation("frame-src", VIEWER, "enforce"); });
  await settle();
  assert(document.activeElement === btn(m, "view"), "focus lost with the frame goes to the retry");
  await m.unmount();
}

// ===== closing the last artifact keeps the Browser tab, and a live view in it =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  await click(tab(m)!);
  await click(btn(m, "view")!);
  const frame = el(m.host, "iframe[data-bz-frame]")!;
  await click(tab(m)!.closest(".cv-tab")!.previousElementSibling!.querySelector(".cv-tab-close") as HTMLElement);
  equal(canvasStore.canvasSnapshot().tabs.length, 0, "the artifact tab is closed");
  assert(drawer(m), "the drawer stays");
  assert(el(m.host, "iframe[data-bz-frame]") === frame, "with the live view undisturbed");
  await raf();
  assert(document.activeElement !== document.body, "and focus did not fall to the page when the tab it was on went");
  await m.unmount();
}
// ... but with no session there is nothing to keep it open for.
{
  const m = await start({ state: "none" });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  await click(el(m.host, ".cv-tab-close")!);
  assert(!drawer(m), "closing the last artifact with no session closes the drawer; it does not leave an empty Browser panel");
  await m.unmount();
}
// ... also when the Browser tab was behind the artifact.
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  assert(!store.browserSnapshot().focused, "the artifact is in front");
  await click(el(m.host, ".cv-tab-close")!);
  assert(drawer(m) && panel(m) && !panel(m)!.hidden, "closing it leaves the Browser tab showing");
  await m.unmount();
}

// ===== the thread card names only the real host =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://accounts.google.com@evil.example.test/signin", instructions: "Sign in." });
  const card = el(m.host, ".bz-card")!;
  assert(card && !text(card).includes("google"), `the card does not show the userinfo: ${text(card)}`);
  assert(text(card).includes("evil.example.test"), "it shows the host the page is really on");
  await React.act(async () => { board.body = { state: "handoff", signedIn: false, url: `https://${"a".repeat(60)}.example.test/x`, instructions: "Sign in." }; await store.refreshBrowser(); });
  await settle();
  const hostEl = el(m.host, ".bz-card .bz-card-host")!;
  equal(text(hostEl), `${"a".repeat(60)}.example.test`, "the card carries the whole host, on a line of its own");
  equal(hostEl.getAttribute("title"), `${"a".repeat(60)}.example.test`, "with the full host as its title");
  assert(!text(el(m.host, ".bz-card .artmeta")!).includes("example"), "and the words before it never share its line (a real browser checks the width)");
  await openTab(m);
  assert(!text(panel(m)).includes("google"), "nor does the panel");
  await m.unmount();
}

// ===== a takeover being stopped no longer asks anything of the person, and its frame is inert =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  board.stopHold = true;
  await click(btn(m, "stop")!);
  await click(btn(m, "go")!);
  assert(!text(panel(m)).includes("asking you to take over") && text(panel(m)).includes("Closing the session"), "no frame: the panel no longer asks for a takeover");
  await React.act(async () => { board.release!(); });
  await settle();
  await m.unmount();
}
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  board.stopHold = true;
  board.stopStatus = 503;
  await click(btn(m, "stop")!);
  await click(btn(m, "go")!);
  const viewer = el(m.host, ".bz-viewer")!;
  assert(viewer.hasAttribute("inert"), "while stopping, the frame is inert");
  assert(text(viewer.querySelector(".bz-viewer-bar b") as HTMLElement).includes("Stopping"), "and its bar says so");
  await React.act(async () => { board.release!(); });
  await settle();
  assert(!el(m.host, ".bz-viewer")!.hasAttribute("inert"), "a failed stop restores it");
  await m.unmount();
}

// ===== a hung request fails instead of stopping the polling for good =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  board.getHang = true;
  await advance(2000);
  const reads = board.gets;
  await advance(10_000);
  assert(text(panel(m)).includes("Couldn't load the browser state"), "a GET that never answers is reported as a failed read");
  board.getHang = false;
  board.body = { state: "active", signedIn: true, url: "https://portal.example.test/orders", instructions: null };
  await advance(4000);
  assert(board.gets > reads, "and polling goes on");
  assert(text(panel(m)).includes("Handed back"), "so the handback is noticed after all");
  await m.unmount();
}
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  board.viewHang = true;
  await click(btn(m, "view")!);
  await advance(15_000);
  assert(text(panel(m)).includes("Couldn't open the live view"), "a view request that never answers ends as an error");
  await m.unmount();
}

// ===== an unchanged answer changes nothing =====
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  const before = store.browserSnapshot();
  await advance(9000);
  assert(store.browserSnapshot() === before, "polls that find nothing new leave the store's state as it was");
  board.body = { state: "active", signedIn: true, url: "https://portal.example.test/orders", instructions: null };
  await advance(3000);
  assert(store.browserSnapshot() !== before && store.browserSnapshot().session?.signedIn === true, "a real change still lands");
  await m.unmount();
}

// ===== Esc outside the panel does not leave a confirm waiting =====
{
  const m = await start({ state: "active", signedIn: true, url: "https://portal.example.test/orders" });
  await openTab(m);
  await click(btn(m, "stop")!);
  equal(store.browserSnapshot().stop, "confirm", "confirm is up");
  await React.act(async () => { document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })); });
  await settle();
  assert(!drawer(m), "Esc with focus outside the panel closes the drawer");
  equal(store.browserSnapshot().stop, "idle", "and the confirm does not wait for the next opening");
  await m.unmount();
}

// ===== the tab, the card and the feed =====
{
  // A takeover brings a hidden tab back.
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await openTab(m);
  await React.act(async () => { store.hideBrowser(); });
  assert(!store.tabVisible(store.browserSnapshot()), "hidden");
  board.body = { state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." };
  await advance(3000);
  assert(store.tabVisible(store.browserSnapshot()), "a new takeover request unhides the tab");
  await m.unmount();
}
{
  // The artifact card says "in canvas" only when the artifact is what the canvas shows.
  const ArtifactCard = require("../src/features/canvas/ArtifactCard").default;
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  const entry = { seq: 2, role: "agent", kind: "artifact", text: "", message: "m1", payload: { id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 } };
  const cardHost = document.createElement("div");
  document.body.append(cardHost);
  const cardRoot = createRoot(cardHost);
  await React.act(async () => { cardRoot.render(React.createElement(LocaleProvider, null, React.createElement(ArtifactCard, { entry }))); });
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  assert(text(cardHost).includes("in canvas"), "the artifact in front reads in canvas");
  await click(tab(m)!);
  assert(!text(cardHost).includes("in canvas"), "with the Browser tab in front it does not");
  await React.act(async () => { cardRoot.unmount(); });
  cardHost.remove();
  await m.unmount();
}
{
  // The feed's auto-open takes the front from the Browser tab, except during a takeover.
  const live = (id: string, turn: string) => ({ key: `${id}@1`, ref: { id, version: 1, kind: "html" as const, title: id, bytes: 1 }, live: true, operatorTurn: true, turn });
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await openTab(m);
  assert(store.browserSnapshot().focused, "the Browser tab is in front");
  await React.act(async () => { canvasStore.feedCanvas({ conversation: "c1", surface: "home", composerKey: "home", cards: [live("art-aaa", "t1")], working: null, turn: "t1" }); });
  await settle();
  assert(!store.browserSnapshot().focused, "a new artifact auto-opening takes the front while the assistant is reading");
  await m.unmount();
}
{
  // ... but not off a takeover: the person is mid-sign-in and the frame stays up.
  const live = (id: string, turn: string) => ({ key: `${id}@1`, ref: { id, version: 1, kind: "html" as const, title: id, bytes: 1 }, live: true, operatorTurn: true, turn });
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  const frame = el(m.host, "iframe[data-bz-frame]")!;
  await React.act(async () => { canvasStore.feedCanvas({ conversation: "c1", surface: "home", composerKey: "home", cards: [live("art-bbb", "t2")], working: null, turn: "t2" }); });
  await settle();
  assert(store.browserSnapshot().focused && el(m.host, "iframe[data-bz-frame]") === frame && !panel(m)!.hidden, "an artifact that arrives mid-takeover does not hide the live view");
  assert(canvasStore.canvasSnapshot().tabs.some((t) => t.id === "art-bbb"), "it is still there as a tab");
  await click(el(m.host, '[role="tab"]')!);
  assert(!store.browserSnapshot().focused, "and a person's click on it does switch");
  await m.unmount();
}
{
  // everLive belongs to a conversation: another one that says 404 has no browser.
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  assert(store.browserSnapshot().everLive, "this conversation has had a live session");
  board.conversation = "c2";
  board.mode = "404";
  await React.act(async () => { store.feedBrowser("c2"); });
  await settle();
  assert(!store.tabVisible(store.browserSnapshot()) && !store.browserSnapshot().everLive, "the next conversation starts with none of it");
  await m.unmount();
}

// ===== the real Home path: MemberChat -> feedBrowser + useCanvasFeed -> drawer =====
{
  reset({ body: { state: "active", signedIn: false, url: "https://portal.example.test/orders", instructions: null }, conversation: "cm1" });
  const MemberChat = require("../src/features/home/MemberChat").default;
  const hostEl = document.createElement("div");
  document.body.append(hostEl);
  const root = createRoot(hostEl);
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(MemberChat, { scope: "s1:u1", readOnly: false, onOpenIssue: () => undefined }), React.createElement(CanvasDrawer)));
  });
  for (let i = 0; i < 6; i++) await advance(1000);
  assert(board.gets >= 1, "the member chat reads the browser for its conversation (cm1)");
  equal(store.browserSnapshot().conversation, "cm1", "through the explicit wiring");
  equal(canvasStore.canvasSnapshot().conversation, "cm1", "and feeds the canvas the same conversation");
  const bzCard = el(hostEl, ".bz-card");
  assert(bzCard, "the thread has the Browser card");
  const artCard = el(hostEl, '[data-kind="artifact"]');
  assert(artCard, "and the artifact card");
  await click(artCard as HTMLElement);
  assert(drawer({ host: hostEl }) && canvasStore.canvasSnapshot().tabs.length === 1, "an artifact card opens the canvas in the member chat now");
  assert(el(hostEl, "#bz-tab"), "with the Browser tab beside it");
  await click(el(hostEl, "#bz-tab")!);
  assert(!el(hostEl, "#bz-panel")!.hasAttribute("hidden"), "which shows the session");
  const polls = board.gets;
  await advance(9000);
  assert(board.gets > polls, "an active session is polled while the chat is on screen");
  await React.act(async () => { root.unmount(); });
  hostEl.remove();
  const after = board.gets;
  await advance(30_000);
  equal(board.gets, after, "leaving the chat stops the polling");
}


// ===== the bearer URL is loaded once: Stop, a 503 and a failed read never remount the frame =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  const frame = el(m.host, "iframe[data-bz-frame]")!;
  const same = (why: string) => assert(el(m.host, "iframe[data-bz-frame]") === frame && frame.isConnected, `the very same frame node ${why}`);
  await click(btn(m, "stop")!);
  same("while the stop confirm is up");
  board.stopHold = true;
  board.stopStatus = 503;
  await click(btn(m, "go")!);
  same("while the stop is in flight");
  assert(frame.parentElement!.hasAttribute("inert"), "inert while it is");
  board.mode = "network";
  await advance(2000);
  same("through a failed read during the stop");
  board.mode = "ok";
  await React.act(async () => { board.release!(); });
  await settle();
  same("after the 503");
  assert(!frame.parentElement!.hasAttribute("inert"), "and live again");
  equal(board.views, 1, "one viewer URL fetched for all of it");
  await m.unmount();
}

// ===== the member chat does not auto-open the canvas =====
{
  reset({ body: { state: "none" }, conversation: "cm1" });
  const MemberChat = require("../src/features/home/MemberChat").default;
  const hostEl = document.createElement("div");
  document.body.append(hostEl);
  const root = createRoot(hostEl);
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(MemberChat, { scope: "s1:u1", readOnly: false, onOpenIssue: () => undefined }), React.createElement(CanvasDrawer)));
  });
  for (let i = 0; i < 4; i++) await advance(1000);
  assert(el(hostEl, '[data-kind="artifact"]'), "the first turn's artifact card is in the thread (history)");
  board.late = true;
  for (let i = 0; i < 8; i++) await advance(1000);
  assert(text(hostEl).includes("Late artifact"), "the late artifact's card arrives live, in an operator's turn");
  assert(!drawer({ host: hostEl }) && canvasStore.canvasSnapshot().tabs.length === 0, "and nothing opened the canvas by itself");
  await React.act(async () => { root.unmount(); });
  hostEl.remove();
}
{
  // The same feed with auto-open left on does open it: the option is what holds it shut.
  reset();
  const live = (id: string, turn: string) => ({ key: `${id}@1`, ref: { id, version: 1, kind: "html" as const, title: id, bytes: 1 }, live: true, operatorTurn: true, turn });
  await React.act(async () => { canvasStore.feedCanvas({ conversation: "c1", surface: "home", composerKey: "home", cards: [live("art-ccc", "t9")], working: null, turn: "t9", autoOpen: false }); });
  assert(!canvasStore.canvasSnapshot().open, "autoOpen: false keeps it shut");
  await React.act(async () => { canvasStore.feedCanvas({ conversation: "c1", surface: "home", composerKey: "home", cards: [live("art-ddd", "t10")], working: null, turn: "t10" }); });
  assert(canvasStore.canvasSnapshot().open, "and left on it opens");
}

// ===== a late live-view URL never takes focus from the chat draft =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  const composer = document.createElement("textarea");
  document.body.append(composer);
  board.viewHold = true;
  await click(btn(m, "view")!);
  await React.act(async () => { composer.focus(); });
  await React.act(async () => { board.releaseView!(); });
  await settle();
  await raf();
  assert(el(m.host, "iframe[data-bz-frame]"), "the live view opened");
  assert(document.activeElement === composer, "and the person's typing still goes to the chat");
  composer.remove();
  await m.unmount();
}
{
  // ... while focus that is still in the panel does move into the view.
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  await React.act(async () => { btn(m, "view")!.focus(); });
  await click(btn(m, "view")!);
  await raf();
  assert(document.activeElement === el(m.host, "iframe[data-bz-frame]"), "focus in the panel follows into the view");
  await m.unmount();
}

// ===== focus survives a failed Open live view and a fast 503 =====
for (const [what, setup] of [["a 500", () => { board.viewStatus = 500; }], ["a hang", () => { board.viewHang = true; }]] as const) {
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  setup();
  const viewBtn = btn(m, "view")!;
  await React.act(async () => { viewBtn.focus(); });
  await click(viewBtn);
  assert(!viewBtn.hasAttribute("disabled"), `${what}: the button is never disabled, which would drop the focus`);
  if (board.viewHang) {
    assert(viewBtn.getAttribute("aria-disabled") === "true", `${what}: waiting is aria-disabled`);
    const before = board.views;
    await click(viewBtn);
    equal(board.views, before, `${what}: and a second click while waiting asks for nothing`);
  }
  await advance(16_000);
  await raf();
  assert(text(panel(m)).includes("Couldn't open the live view"), `${what}: it failed`);
  assert(document.activeElement === btn(m, "view"), `${what}: focus is still on Open live view, so Enter retries`);
  await m.unmount();
}
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await openTab(m);
  board.stopStatus = 503;
  const stopBtn = btn(m, "stop")!;
  await React.act(async () => { stopBtn.focus(); });
  await click(stopBtn);
  await raf();
  assert(text(panel(m)).includes("Couldn't stop the session"), "the stop failed at once");
  assert(document.activeElement === btn(m, "stop") && text(btn(m, "stop")).includes("Try stopping again"), "focus is on Try stopping again");
  await m.unmount();
}

// ===== a reload mid-takeover whose first read fails recovers on its own =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." }, "network");
  assert(!store.tabVisible(store.browserSnapshot()), "no tab yet: the board has not answered");
  board.mode = "ok";
  await advance(31_000);
  assert(store.tabVisible(store.browserSnapshot()) && store.browserSnapshot().session?.state === "handoff", "the next slow try finds the takeover, with its tab and Stop");
  await m.unmount();
}
// ... and a read asked for while one is in flight is not lost.
{
  const m = await start({ state: "none" });
  board.getHold = true;
  const reads = board.gets;
  const first = store.refreshBrowser();
  board.body = { state: "active", signedIn: false, url: "https://portal.example.test/orders", instructions: null };
  await React.act(async () => { void store.refreshBrowser(); });
  await React.act(async () => { board.releaseGet!(); await first; });
  await settle();
  equal(board.gets, reads + 2, "the second ask ran once, after the first ended");
  equal(store.browserSnapshot().session?.state, "active", "and saw the new session");
  await m.unmount();
}

// ===== what the copy says is only what is known =====
{
  const m = await start({ state: "handoff", signedIn: true, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  assert(!/signed in/i.test(text(el(m.host, ".bz-addr"))), "a handoff does not claim the person signed in");
  assert(!text(el(m.host, ".bz-addr")).includes("You had control"), "nor that a person had control: the server sets that when the takeover is requested");
  await click(btn(m, "stop")!);
  const words = text(el(m.host, "[data-bz-dock]"));
  assert(!/signed in here/.test(words) && words.includes("Anything signed in to this session is lost"), `the confirm says what is true: ${words}`);
  await click(btn(m, "keep")!);
  board.body = { state: "active", signedIn: true, url: "https://portal.example.test/orders", instructions: null };
  await advance(2000);
  assert(text(el(m.host, ".bz-addr")).includes("You had control"), "after the handback, with the person's control recorded, it says so");
  await m.unmount();
}

// ===== the viewer bar names no site; the last-read address and the caution stay with the frame =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://stale-page.example.test/orders", instructions: "Sign in." });
  await openTab(m);
  await click(btn(m, "view")!);
  const bar = text(el(m.host, ".bz-viewer-bar"));
  assert(!bar.includes("stale-page"), `the bar does not name the (possibly stale) host: ${bar}`);
  assert(text(panel(m)).includes("Last page the assistant read") && text(panel(m)).includes("check where you are"), "the label and the caution are still on screen while the frame is open");
  assert(!/live view itself/.test(text(panel(m))), "and the caution does not assume the viewer has its own address bar");
  await m.unmount();
}

// ===== a late URL with the stop confirm open takes no focus =====
{
  const m = await start({ state: "handoff", signedIn: false, url: "https://login.supplier-example.test/sign-in", instructions: "Sign in." });
  await openTab(m);
  board.viewHold = true;
  await React.act(async () => { btn(m, "view")!.focus(); });
  await click(btn(m, "view")!);
  await click(btn(m, "stop")!);
  equal(store.browserSnapshot().stop, "confirm", "the stop confirm is up");
  await raf();
  // Even with focus nowhere (the page), an open confirm keeps it from the frame.
  await React.act(async () => { (document.activeElement as HTMLElement).blur(); });
  assert(document.activeElement === document.body, "focus is on the page");
  await React.act(async () => { board.releaseView!(); });
  await settle();
  await raf();
  assert(el(m.host, "iframe[data-bz-frame]"), "the live view opened behind it");
  assert(document.activeElement !== el(m.host, "iframe[data-bz-frame]"), "focus did not jump into the provider's page");
  await m.unmount();
}

// ===== closing the last artifact with no session hands focus to the composer =====
{
  const m = await start({ state: "none" });
  const composer = document.createElement("textarea");
  composer.id = "app-shell-chat-box";
  document.body.append(composer);
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  const closeBtn = el(m.host, ".cv-tab-close")!;
  await React.act(async () => { closeBtn.focus(); });
  await click(closeBtn);
  await raf();
  assert(!drawer(m), "the drawer closed");
  assert(document.activeElement === composer, "and focus went to the composer, not the page");
  // Esc and the header close, from inside the drawer, do the same.
  await React.act(async () => { canvasStore.openArtifact({ id: "art-html1", version: 1, kind: "html", title: "Doc", bytes: 12 }); });
  await settle();
  await React.act(async () => { (el(m.host, '[aria-label="Close canvas"]') as HTMLElement).focus(); });
  await click(el(m.host, '[aria-label="Close canvas"]')!);
  await raf();
  assert(document.activeElement === composer, "the header close too");
  composer.remove();
  await m.unmount();
}

// ===== closing the drawer leaves no stale front tab =====
{
  const m = await start({ state: "active", signedIn: false, url: "https://portal.example.test/orders" });
  await openTab(m);
  assert(store.browserSnapshot().focused, "in front");
  await React.act(async () => { store.closeBrowserDrawer(); });
  assert(!store.browserSnapshot().focused && !store.browserSnapshot().open, "closed: not in front either");
  await m.unmount();
}

// ===== Home wiring: a conversation change resets, and leaving stops the polling =====
{
  const m = await start({ state: "active", url: "https://portal.example.test/orders" });
  const gets = board.gets;
  board.conversation = "c2";
  await React.act(async () => { store.feedBrowser("c2"); });
  await settle();
  equal(store.browserSnapshot().conversation, "c2", "the new conversation is read");
  assert(board.gets > gets, "with a fresh read");
  await React.act(async () => { store.feedBrowser(null); });
  await settle();
  const after = board.gets;
  await advance(30_000);
  equal(board.gets, after, "leaving Home stops the polling");
  assert(!tab(m), "and the tab goes with it");
  await m.unmount();
}

console.log("browser canvas tests passed");
}
main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
