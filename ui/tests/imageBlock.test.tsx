/**
 * CAD-2069: the image block in the thread and the image canvas. The real
 * components are mounted in happy-dom against a stateful fake of the host's
 * image-job and artifact routes, with a manual clock so polling is exact.
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
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLButtonElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"])
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

// ---- manual clock: timers only run when the test advances time ----
let now = Date.parse("2026-11-03T08:00:00Z");
Date.now = () => now;
const timers = new Map<number, { at: number; fn: () => void }>();
let timerSeq = 0;
(globalThis as any).setTimeout = (fn: () => void, ms = 0) => {
  const id = ++timerSeq;
  timers.set(id, { at: now + ms, fn });
  return id;
};
(globalThis as any).clearTimeout = (id: number) => void timers.delete(id);

const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const ImageBlock = require("../src/features/canvas/ImageBlock").default;
const CanvasDrawer = require("../src/features/canvas/CanvasDrawer").default;
const { ThreadTurn } = require("../src/features/home/ThreadView") as typeof import("../src/features/home/ThreadView");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const canvasStore = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");
const thread = require("../src/features/home/thread") as typeof import("../src/features/home/thread");
const { POLL_MS, LONG_WAIT_MS, GIVE_UP_MS } = require("../src/features/canvas/ImageBlock");
const { ImageConversation } = require("../src/features/canvas/imageConversation");

const settle = () => React.act(async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); });
async function advance(ms: number) {
  const end = now + ms;
  for (;;) {
    let next: [number, { at: number; fn: () => void }] | undefined;
    for (const e of timers) if (e[1].at <= end && (!next || e[1].at < next[1].at)) next = e;
    if (!next) break;
    now = Math.max(now, next[1].at);
    timers.delete(next[0]);
    await React.act(async () => { next![1].fn(); });
    await settle();
  }
  now = end;
  await settle();
}

// ---- the host, stateful ----
const JOB = "med_7b693b44d928437491a2c13e549f6e0b";
const JOB2 = "med_second0000000000000000000000000";
const SIZE = 2202009; // 2.1 MB
const host = {
  job: { state: "running" } as Record<string, unknown>,
  jobMode: "ok" as "ok" | "network" | "malformed" | "missing" | "unavailable" | "other" | "denied" | "forbidden",
  bytesMode: "ok" as "ok" | "fail" | "missing",
  html: false,
  statusReads: 0,
  byteReads: 0,
  versions: 1,
  expect: "c1",
  bytesMime: "image/png",
  mimeSize: 0,
  lastArtifact: "",
};
const finished = { state: "finished", artifact: { id: "art-img1", version: 1, kind: "image", title: "Red apple on a white surface", bytes: SIZE, digest: "a".repeat(64) } };
globalThis.fetch = (async (input: unknown) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname === `/api/image-jobs/${JOB2}`) {
    host.statusReads++;
    return new Response(JSON.stringify({ state: "finished", artifact: { id: "art-img2", version: 1, kind: "image", title: "Second", bytes: 1048576, digest: "d".repeat(64) } }), { status: 200 });
  }
  if (url.pathname === `/api/image-jobs/${JOB}`) {
    host.statusReads++;
    equal(url.searchParams.get("conversation"), host.expect, "the status read names the conversation in view");
    if (host.jobMode === "denied") return new Response("{}", { status: 401 });
    if (host.jobMode === "forbidden") return new Response(JSON.stringify({ error: "membership_changed" }), { status: 403 });
    if (host.jobMode === "network") throw new TypeError("network down");
    if (host.jobMode === "missing") return new Response("{}", { status: 404 });
    if (host.jobMode === "unavailable") return new Response(JSON.stringify({ error: "capability_unavailable" }), { status: 503 });
    if (host.jobMode === "other") return new Response("{}", { status: 500 });
    if (host.jobMode === "malformed") return new Response("<html>oops</html>", { status: 200 });
    return new Response(JSON.stringify(host.job), { status: 200, headers: { "Content-Type": "application/json" } });
  }
  if (url.pathname === "/api/member-assistant/state") {
    return new Response(JSON.stringify({ conversation_id: "cm1", assistant: { org: "o", sub: "u", assistant_id: "a", alias: "assistant", conversation_id: "cm1", state: "ready", bootstrap_state: null } }), { status: 200 });
  }
  if (url.pathname === "/api/member-assistant/history") {
    return new Response(JSON.stringify({ conversation: { id: "cm1", alias: "assistant" }, entries: [
      { seq: 1, role: "member", kind: "message", text: "make an apple", message: "m1" },
      { seq: 2, role: "agent", kind: "image_job", text: "", message: "m1", created: new Date(now - 5000).toISOString(), payload },
      { seq: 3, role: "agent", kind: "turn_result", text: "Started.", message: "m1" },
    ], more_before: false }), { status: 200 });
  }
  if (url.pathname.startsWith("/api/artifacts/")) {
    host.byteReads++;
    host.lastArtifact = url.pathname;
    equal(url.searchParams.get("conversation"), host.expect, "the bytes read names the conversation in view");
    if (host.bytesMode === "fail") return new Response("{}", { status: 500 });
    if (host.bytesMode === "missing") return new Response("{}", { status: 404 });
    if (url.pathname.startsWith("/api/artifacts/art-img2")) return new Response(new Uint8Array(1048576), { status: 200, headers: { "Content-Type": "image/png" } });
    if (url.pathname.startsWith("/api/artifacts/art-html1")) return new Response("<p>hello</p>", { status: 200, headers: { "Content-Type": "text/html" } });
    return new Response(new Uint8Array(SIZE), { status: 200, headers: { "Content-Type": host.bytesMime } });
  }
  throw new Error(`Unexpected request ${url.pathname}`);
}) as typeof fetch;

// IntersectionObserver the test drives.
const observers: { cb: (hits: { isIntersecting: boolean }[]) => void; disconnected: boolean }[] = [];
let withObserver = true;
class FakeObserver {
  rec: (typeof observers)[number];
  constructor(cb: (hits: { isIntersecting: boolean }[]) => void) {
    this.rec = { cb, disconnected: false };
    observers.push(this.rec);
  }
  observe() {}
  disconnect() { this.rec.disconnected = true; }
}
Object.defineProperty(globalThis, "IntersectionObserver", { get: () => (withObserver ? FakeObserver : undefined), configurable: true });
const enterViewport = async () => {
  await React.act(async () => { for (const o of observers) if (!o.disconnected) o.cb([{ isIntersecting: true }]); });
  await settle();
};

// Downloads.
const saved: { name: string; type: string; size: number }[] = [];
(URL as any).createObjectURL = (b: Blob) => { saved.push({ name: "", type: b.type, size: b.size }); return "blob:x"; };
(URL as any).revokeObjectURL = () => {};
const origAppend = document.body.appendChild.bind(document.body);
document.body.appendChild = ((n: any) => {
  if (n?.tagName === "A" && n.download) {
    saved[saved.length - 1].name = n.download;
    n.click = () => {};
  }
  return origAppend(n);
}) as typeof document.body.appendChild;

// Visibility.
let hidden = false;
Object.defineProperty(document, "visibilityState", { get: () => (hidden ? "hidden" : "visible"), configurable: true });
const setHidden = async (v: boolean) => {
  hidden = v;
  await React.act(async () => { document.dispatchEvent(new win.Event("visibilitychange")); });
  await settle();
};

const payload = { job_id: JOB, title: "Red apple on a white surface", aspect: "1:1" };
const entry = (over: Record<string, unknown> = {}, created: string | null | "absent" = new Date(now - 42_000).toISOString()) => ({
  seq: 7, role: "agent", kind: "image_job", text: "", message: "m1", ...(created === "absent" ? {} : { created }), payload: { ...payload, ...over },
});
const el = (root: ParentNode, sel: string) => root.querySelector(sel) as HTMLElement | null;
const COST = /\$|cost|price|charge|credit|balance|HKD|USD|pay|fee/i;

type Mounted = { box: HTMLElement; root: ReturnType<typeof createRoot>; unmount: () => Promise<void>; render: (node: any) => Promise<void> };
async function mount(node: any, drawer = false, conversation: string | null = "c1"): Promise<Mounted & { host: HTMLElement }> {
  const hostEl = document.createElement("div");
  document.body.append(hostEl);
  const root = createRoot(hostEl);
  const wrap = (n: any) => React.createElement(LocaleProvider, null, React.createElement(ImageConversation.Provider, { value: conversation }, n), drawer ? React.createElement(CanvasDrawer) : null);
  await React.act(async () => { root.render(wrap(node)); });
  await settle();
  return {
    host: hostEl,
    box: hostEl,
    root,
    render: async (n: any) => { await React.act(async () => { root.render(wrap(n)); }); await settle(); },
    unmount: async () => { await React.act(async () => { root.unmount(); }); hostEl.remove(); },
  };
}
function reset() {
  host.job = { state: "running" };
  host.jobMode = "ok";
  host.bytesMode = "ok";
  host.bytesMime = "image/png";
  host.expect = "c1";
  host.statusReads = 0;
  host.byteReads = 0;
  withObserver = false;
  observers.length = 0;
  saved.length = 0;
  hidden = false;
  timers.clear();
  canvasStore.resetCanvas("c1", null, "home");
}
const text = (m: { host: HTMLElement }) => m.host.textContent ?? "";
const block = (m: { host: HTMLElement }) => el(m.host, ".imgblock");
const box = (m: { host: HTMLElement }) => el(m.host, ".imgbox");

// ===== malformed entries render nothing =====
reset();
for (const bad of [
  { job_id: undefined }, { job_id: 7 }, { job_id: "job_abc" }, { job_id: "med_" }, { job_id: "med_a/b" },
  { title: undefined }, { title: 3 }, { aspect: undefined }, { aspect: "4:3" }, { aspect: 1 },
]) {
  const m = await mount(React.createElement(ImageBlock, { entry: entry(bad) }));
  equal(m.host.innerHTML, "", `malformed payload ${JSON.stringify(bad)} renders nothing`);
  await m.unmount();
}
{
  const m = await mount(React.createElement(ImageBlock, { entry: { ...entry(), payload: null } }));
  equal(m.host.innerHTML, "", "a null payload renders nothing");
  await m.unmount();
}
equal(host.statusReads, 0, "a malformed entry issues no request");

// ===== running to finished in the same box =====
reset();
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  const b = box(m)!;
  assert(b, "the box is there from the first paint");
  equal(block(m)!.getAttribute("data-shape"), "1:1", "the box carries the entry's shape");
  assert(text(m).includes("Creating your image"), "running reads Creating your image");
  assert(text(m).includes("0:42"), `the timer starts from the entry's time: ${text(m)}`);
  assert(text(m).includes("You can leave this page. It will appear here when it's ready."), "running says you can leave");
  assert(!el(m.host, "img"), "no image while running");
  assert(!b.hasAttribute("data-compact"), "the running box is the full shape");
  await advance(3000);
  assert(text(m).includes("0:45"), "the timer counts");
  equal(el(m.host, '[role="status"]')!.textContent, "", "nothing announced while it runs");

  host.job = finished;
  await advance(POLL_MS);
  assert(box(m) === b, "the same box node holds the image: nothing remounted");
  equal(block(m)!.getAttribute("data-shape"), "1:1", "the shape did not change");
  assert(!b.hasAttribute("data-compact"), "the finished box is still the full shape");
  const img = el(m.host, "img") as HTMLImageElement;
  assert(img && img.getAttribute("src")!.startsWith("data:image/png;base64,"), "the image is a data URL, never a blob");
  equal(img.getAttribute("alt"), "Red apple on a white surface", "the image has the title as alt");
  assert(!text(m).includes("Creating your image"), "the placeholder is gone");
  equal(el(m.host, '[role="status"]')!.textContent, "Your image is ready: Red apple on a white surface", "completion is announced");
  const overlay = el(m.host, ".imgover")!;
  assert(overlay.textContent!.includes("Red apple on a white surface"), "the overlay names the image");
  assert(overlay.textContent!.includes("1:1 · PNG · 2.1 MB"), `the overlay carries shape, format and size: ${overlay.textContent}`);
  const dl = el(overlay, "button")!;
  equal(dl.getAttribute("aria-label"), "Download", "Download has an accessible name");
  equal(dl.getAttribute("title"), "Download", "Download has a tooltip");
  assert(!el(m.host, ".imgact"), "no extra action on a finished image");
  assert(!/make another|ask to change|\bstop\b/i.test(text(m)), "no Make another, Ask to change or Stop");
  const reads = host.statusReads;
  await advance(60_000);
  equal(host.statusReads, reads, "polling stopped on the terminal state");
  // Download saves the real bytes.
  await React.act(async () => { dl.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  equal(saved.length, 1, "Download hands one file to the browser");
  equal(saved[0].type, "image/png", "with the real mime");
  equal(saved[0].size, SIZE, "and the real size");
  equal(saved[0].name, "Red apple on a white surface.png", "named after the title");
  await m.unmount();
}

// ===== no timer without created; longer than usual =====
reset();
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry({}, "absent") }));
  assert(text(m).includes("Creating your image"), "running without created still reads");
  assert(!el(m.host, ".imgclock"), "no timer without created");
  await advance(LONG_WAIT_MS + 10_000);
  assert(!text(m).includes("Taking longer"), "without created there is no honest basis for longer than usual");
  await m.unmount();
}
reset();
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry({}, new Date(now - LONG_WAIT_MS - 1000).toISOString()) }));
  assert(text(m).includes("Still creating your image"), "after the long wait it reads Still creating");
  assert(text(m).includes("Taking longer than usual."), "and says it is taking longer");
  assert(!text(m).includes("You can leave this page"), "the longer message replaces the first");
  await m.unmount();
}

// ===== unreadable answers never become a result =====
for (const mode of ["network", "malformed", "unavailable", "other"] as const) {
  reset();
  host.jobMode = mode;
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  const b = box(m)!;
  await advance(5000 * 6);
  assert(host.statusReads >= 2, `${mode}: it keeps asking`);
  assert(host.statusReads <= 6, `${mode}: with backoff, not every five seconds (${host.statusReads})`);
  assert(box(m) === b && !b.hasAttribute("data-compact"), `${mode}: the placeholder box stays`);
  assert(!/couldn't be created|can't confirm|isn't available/.test(text(m)) && !el(m.host, "img"), `${mode}: never failed, unclear or finished`);
  host.jobMode = "ok";
  host.job = finished;
  await advance(60_000);
  assert(el(m.host, "img"), `${mode}: recovers when the host answers`);
  await m.unmount();
}
// A finished answer without a stored image is malformed, not finished.
for (const body of [{ state: "finished" }, { state: "finished", artifact: { id: "art-img1", version: 1, kind: "md", title: "x", bytes: 1 } }, { state: "bogus" }]) {
  reset();
  host.job = body;
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  await advance(20_000);
  assert(!el(m.host, "img") && !/Your image|couldn't/.test(text(m)), `${JSON.stringify(body)} is not a result`);
  await m.unmount();
}

// ===== failed, unclear, unavailable =====
reset();
host.job = { state: "failed" };
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  assert(text(m).includes("This image couldn't be created"), "failed headline");
  assert(text(m).includes("The image service turned it down."), "failed reason");
  assert(!el(m.host, "button") && !/try again|make another/i.test(text(m)), "failed offers nothing to press");
  await advance(30_000);
  equal(host.statusReads, 1, "a failed job is not read again");
  await m.unmount();
}
reset();
host.job = { state: "unclear" };
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  assert(text(m).includes("We can't confirm this image yet"), "unclear headline");
  assert(text(m).includes("We lost contact with the image service. It may still finish."), "unclear reason");
  const btn = el(m.host, "button.imgact") as HTMLButtonElement;
  equal(btn.textContent, "Check again", "unclear offers Check again");
  await advance(30_000);
  equal(host.statusReads, 1, "an unclear job is only re-read on request");
  host.jobMode = "network";
  await React.act(async () => { btn.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  equal(host.statusReads, 2, "Check again reads once");
  assert(text(m).includes("We can't confirm this image yet") && !(el(m.host, "button.imgact") as HTMLButtonElement).disabled, "a failed re-read leaves the card and the button");
  host.jobMode = "ok";
  host.job = finished;
  await React.act(async () => { (el(m.host, "button.imgact") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  assert(el(m.host, "img"), "Check again finds the image");
  await m.unmount();
}
reset();
host.jobMode = "missing";
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  assert(text(m).includes("This image isn't available."), "a 404 reads as unavailable");
  assert(box(m) && !el(m.host, "img"), "the box stays, with no image");
  await advance(30_000);
  equal(host.statusReads, 1, "unavailable is not polled");
  await m.unmount();
}

// ===== polling: visibility, unmount =====
reset();
{
  await (async () => { hidden = true; })();
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  await advance(60_000);
  equal(host.statusReads, 0, "no polling while the page is hidden");
  await setHidden(false);
  equal(host.statusReads, 1, "coming back reads at once");
  await advance(POLL_MS * 2);
  equal(host.statusReads, 3, "then about every five seconds");
  await setHidden(true);
  const at = host.statusReads;
  await advance(60_000);
  equal(host.statusReads, at, "hiding the page stops the polling");
  await setHidden(false);
  await m.unmount();
  const after = host.statusReads;
  await advance(60_000);
  equal(host.statusReads, after, "unmounting stops the polling");
  equal(timers.size, 0, "and leaves no timer behind");
}

// ===== lazy bytes =====
reset();
withObserver = true;
host.job = finished;
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  equal(host.statusReads, 0, "no status read while the box is far from the viewport");
  equal(host.byteReads, 0, "no bytes while the box is far from the viewport");
  assert(!el(m.host, "img"), "no image yet");
  assert(box(m), "the box holds the place");
  await enterViewport();
  equal(host.byteReads, 1, "bytes load once the box is near");
  assert(el(m.host, "img"), "and the image appears");
  await m.unmount();
}
reset();
host.job = finished;
host.bytesMode = "fail";
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  assert(box(m) && !el(m.host, "img"), "no image without bytes, and the box stays");
  assert(!text(m).includes("isn't available"), "a temporary byte fault is not 'unavailable'");
  await advance(5000 * 4);
  assert(host.byteReads >= 2 && host.byteReads <= 4, `the byte read is retried with backoff (${host.byteReads})`);
  host.bytesMode = "ok";
  await advance(60_000);
  assert(el(m.host, "img"), "the image appears when the bytes come back");
  await m.unmount();
}
for (const [what, setup] of [
  ["a 404 on the bytes", () => { host.bytesMode = "missing"; }],
  ["bytes that are not an image", () => { host.bytesMime = "text/html"; }],
] as const) {
  reset();
  host.job = finished;
  setup();
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  assert(box(m) && !el(m.host, "img"), `${what}: no image, and the box stays`);
  assert(text(m).includes("This image isn't available."), `${what}: a quiet line says so`);
  assert(!el(m.host, ".imgover"), `${what}: no overlay without a picture`);
  const reads = host.byteReads;
  await advance(60_000);
  equal(host.byteReads, reads, `${what}: it is final, not retried`);
  await m.unmount();
}

// ===== lost access stops the polling =====
for (const mode of ["denied", "forbidden"] as const) {
  reset();
  host.jobMode = mode;
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  assert(text(m).includes("This image isn't available."), `${mode}: reads unavailable`);
  await advance(5 * 60_000);
  equal(host.statusReads, 1, `${mode}: no further reads`);
  await m.unmount();
}

// ===== a running job past 24 hours is unconfirmed =====
reset();
{
  const old = new Date(now - GIVE_UP_MS - 60_000).toISOString();
  const m = await mount(React.createElement(ImageBlock, { entry: entry({}, old) }));
  assert(text(m).includes("We can't confirm this image yet"), "running after 24 h reads unconfirmed");
  assert(!text(m).includes("Still creating"), "not Still creating forever");
  assert(el(m.host, "button.imgact"), "with Check again");
  await advance(5 * 60_000);
  equal(host.statusReads, 1, "and polling has stopped");
  host.job = finished;
  await React.act(async () => { (el(m.host, "button.imgact") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  assert(el(m.host, "img"), "Check again finds a saved image");
  await m.unmount();
  reset();
  host.job = finished;
  const m2 = await mount(React.createElement(ImageBlock, { entry: entry({}, old) }));
  assert(el(m2.host, "img") && !text(m2).includes("We can't confirm"), "a saved image older than 24 h is unaffected");
  await m2.unmount();
  reset();
  const m3 = await mount(React.createElement(ImageBlock, { entry: entry({}, new Date(now - GIVE_UP_MS + 120_000).toISOString()) }));
  assert(text(m3).includes("Still creating"), "just inside 24 h it still runs");
  await m3.unmount();
}

// ===== a block never carries one job to another =====
reset();
host.job = finished;
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }));
  const first = (el(m.host, "img") as HTMLImageElement).getAttribute("src");
  assert(first, "job one shows its picture");
  await m.render(React.createElement(ImageBlock, { entry: entry({ job_id: JOB2, title: "Second" }) }));
  await advance(POLL_MS);
  equal(block(m)!.getAttribute("data-job"), JOB2, "the block now names job two");
  assert(host.lastArtifact.includes("art-img2"), `job two's bytes were read: ${host.lastArtifact}`);
  const second = (el(m.host, "img") as HTMLImageElement).getAttribute("src");
  assert(second && second !== first, "and its own picture shows, not job one's");
  assert(el(m.host, ".imgover")!.textContent!.includes("1.0 MB"), "with its own size");
  await m.unmount();
}

// ===== no conversation, no request =====
reset();
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }), false, null);
  await advance(30_000);
  equal(host.statusReads, 0, "outside a host nothing is read");
  assert(box(m), "the box still holds its place");
  await m.unmount();
}
// The conversation comes from the host, never from the canvas store.
reset();
canvasStore.resetCanvas("another", null, "home");
host.job = finished;
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }), false, "c1");
  assert(el(m.host, "img"), "a stale canvas conversation does not matter");
  await m.unmount();
}
reset();
canvasStore.resetCanvas(null, null, "home");
host.job = finished;
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }), true, "c1");
  assert(el(m.host, "img"), "a host that feeds no canvas still loads its images");
  await React.act(async () => { (el(m.host, "button.imgopen") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  assert(el(m.host, "[data-canvas-drawer]"), "and its click still opens the canvas");
  await m.unmount();
}

// ===== no cost text in any state =====
for (const [name, job, mode] of [
  ["running", { state: "running" }, "ok"], ["long", { state: "running" }, "ok"], ["finished", finished, "ok"],
  ["failed", { state: "failed" }, "ok"], ["unclear", { state: "unclear" }, "ok"], ["missing", {}, "missing"], ["pending", {}, "network"],
] as const) {
  reset();
  host.job = job as Record<string, unknown>;
  host.jobMode = mode;
  const m = await mount(React.createElement(ImageBlock, { entry: entry({}, new Date(now - (name === "long" ? LONG_WAIT_MS + 5000 : 5000)).toISOString()) }));
  const labels = Array.from(m.host.querySelectorAll("[aria-label],[title],[alt]")).map((n) => `${n.getAttribute("aria-label")} ${n.getAttribute("title")} ${n.getAttribute("alt")}`).join(" ");
  assert(!COST.test(`${text(m)} ${labels}`.replace(/Red apple on a white surface/g, "")), `${name}: no cost, price or payment wording: ${text(m)} ${labels}`);
  await m.unmount();
}

// ===== opening the canvas, keyboard, focus return =====
reset();
host.job = finished;
{
  const m = await mount(React.createElement(ImageBlock, { entry: entry() }), true);
  const open = el(m.host, "button.imgopen") as HTMLButtonElement;
  assert(open && open.tagName === "BUTTON" && open.getAttribute("type") === "button", "the picture is a real button, so Enter and Space open it");
  equal(open.getAttribute("aria-label"), "Open Red apple on a white surface", "it names what it opens");
  open.focus();
  assert(document.activeElement === open, "the picture takes keyboard focus");
  assert(!el(m.host, "[data-canvas-drawer]"), "the canvas is closed first");
  await React.act(async () => { open.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  const drawer = el(m.host, "[data-canvas-drawer]");
  assert(drawer, "activating the picture opens the canvas");
  const cv = canvasStore.canvasSnapshot();
  equal(cv.tabs[0].id, "art-img1", "on the stored image");
  equal(cv.tabs[0].aspect, "1:1", "carrying its shape");
  assert(el(drawer!, '[data-artifact-view="image"] img'), "the canvas shows the image");
  assert(el(drawer!, "[data-canvas-image] .imgover"), "with the same overlay");
  // The operator moves into the canvas and closes it with the keyboard.
  const close = el(drawer!, '[aria-label="Close canvas"]') as HTMLElement;
  close.focus();
  await React.act(async () => { close.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  assert(!el(m.host, "[data-canvas-drawer]"), "closing removes the canvas");
  assert(document.activeElement === el(m.host, "button.imgopen"), `focus returns to the picture, not ${document.activeElement?.tagName}`);
  await m.unmount();
}

// ===== the canvas: image against html =====
async function canvasWith(kind: "image" | "html", opts: { sheet?: boolean; versions?: number } = {}) {
  reset();
  (win as any).innerWidth = opts.sheet ? 400 : 1600;
  const m = await mount(React.createElement("div"), true);
  await React.act(async () => {
    canvasStore.openArtifact(kind === "image"
      ? { id: "art-img1", version: 1, kind: "image", title: "Red apple", bytes: SIZE, digest: "b".repeat(64), aspect: "16:9" }
      : { id: "art-html1", version: 1, kind: "html", title: "Page", bytes: 12, digest: "c".repeat(64) });
    for (let v = 2; v <= (opts.versions ?? 1); v++) {
      canvasStore.openArtifact(kind === "image"
        ? { id: "art-img1", version: v, kind: "image", title: "Red apple", bytes: SIZE }
        : { id: "art-html1", version: v, kind: "html", title: "Page", bytes: 12 });
    }
  });
  await settle();
  const drawer = el(m.host, "[data-canvas-drawer]")!;
  assert(drawer, `${kind} canvas opens`);
  return { m, drawer };
}
const names = (root: HTMLElement) => Array.from(root.querySelectorAll("button")).map((b) => b.getAttribute("aria-label") ?? b.textContent);
for (const sheet of [false, true]) {
  const layout = sheet ? "sheet" : "desktop";
  {
    const { m, drawer } = await canvasWith("image", { sheet });
    equal(drawer.getAttribute("data-mode") === "sheet", sheet, `${layout}: the layout is as asked`);
    assert(el(drawer, '[data-artifact-view="image"] img'), `${layout}: image shows`);
    assert(!/Ask to change/.test(drawer.textContent!), `${layout}: an image has no Ask to change`);
    equal(Array.from(drawer.querySelectorAll("button")).filter((b) => b.textContent === "Download").length, 0, `${layout}: no header Download button`);
    equal(names(drawer).filter((n) => n === "Download").length, 1, `${layout}: exactly one Download route, in the overlay`);
    assert(el(drawer, ".imgover button[aria-label=Download]"), `${layout}: and it is the overlay's`);
    assert(!el(drawer, "footer.cv-foot") && !drawer.textContent!.includes("sha256"), `${layout}: no footer and no hash`);
    assert(!el(drawer, ".vers"), `${layout}: no version picker for one version`);
    assert(el(drawer, ".imgover")!.textContent!.includes("16:9 · PNG · 2.1 MB"), `${layout}: the overlay reads the real mime and size`);
    // The overlay's Download works from the canvas.
    saved.length = 0;
    await React.act(async () => { el(drawer, ".imgover button")!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
    equal(saved.length, 1, `${layout}: Download from the canvas saves a file`);
    assert(el(drawer, '[aria-label="Back to chat"], [aria-label="Close canvas"]'), `${layout}: the canvas can still be left`);
    await m.unmount();
  }
  {
    const { m, drawer } = await canvasWith("image", { sheet, versions: 2 });
    assert(el(drawer, ".vers"), `${layout}: two versions keep the picker`);
    assert(!/Ask to change/.test(drawer.textContent!), `${layout}: still no Ask to change with two versions`);
    await m.unmount();
  }
  {
    const { m, drawer } = await canvasWith("html", { sheet });
    assert(Array.from(drawer.querySelectorAll("button")).some((b) => b.textContent === "Ask to change"), `${layout}: html keeps Ask to change`);
    assert(Array.from(drawer.querySelectorAll("button")).some((b) => b.textContent === "Download"), `${layout}: html keeps its Download`);
    assert(el(drawer, "footer.cv-foot")!.textContent!.includes("sha256:"), `${layout}: html keeps its footer hash`);
    assert(el(drawer, ".vers"), `${layout}: html keeps its version picker`);
    assert(!el(drawer, ".imgover"), `${layout}: html has no image overlay`);
    await m.unmount();
  }
}

// ===== the thread =====
{
  const entries = [
    { seq: 1, role: "operator", kind: "message", text: "make an apple", message: "m1" },
    { seq: 2, role: "agent", kind: "tool_call", text: "media generate", message: "m1" },
    { seq: 3, role: "agent", kind: "image_job", text: "", message: "m1", payload },
    { seq: 4, role: "agent", kind: "tool_result", text: "ok", message: "m1" },
    { seq: 5, role: "agent", kind: "turn_result", text: "Started.", message: "m1" },
  ];
  const items = thread.threadItems({ entries, pending: [] } as any);
  equal(items.map((i) => i.type), ["operator", "tools", "image", "answer"], "an image entry is held until the tool run ends, then is its own item");
  const runs = thread.threadRuns(items);
  equal(runs.map((r) => (r.turn ? r.items.map((i) => i.type) : r.item.type)), ["operator", ["tools", "image", "answer"]], "the image belongs to its turn");
  equal(thread.threadRuns(thread.threadItems({ entries: [entries[2]], pending: [] } as any)).map((r) => r.turn), [false], "an image with no agent text is not a turn");
  // A board that never receives an image entry groups exactly as before.
  const plain = thread.threadItems({ entries: entries.filter((e) => e.kind !== "image_job"), pending: [] } as any);
  equal(plain.map((i) => i.type), ["operator", "tools", "answer"], "without image entries nothing changes");

  reset();
  host.job = finished;
  const turn = runs[1];
  assert(turn.turn, "a turn");
  const m = await mount(React.createElement(ThreadTurn, { items: turn.turn ? turn.items : [] }));
  assert(el(m.host, ".turn-artifacts .turn-image .imgblock"), "the turn paints the image under its answer");
  assert(el(m.host, "img"), "and it resolves to the picture");
  await m.unmount();
}

// ===== the hosted member chat feeds no canvas, and still loads its image =====
reset();
canvasStore.resetCanvas(null, null, "home");
host.expect = "cm1";
host.job = finished;
{
  const MemberChat = require("../src/features/home/MemberChat").default;
  const m = await mount(React.createElement(MemberChat, { scope: "s1:u1", readOnly: false, onOpenIssue: () => undefined }), true);
  for (let i = 0; i < 6; i++) await advance(1000);
  assert(el(m.host, ".imgblock"), "the member chat renders the image block");
  assert(host.statusReads >= 1, "and reads the job");
  assert(el(m.host, "img"), `and shows the picture: ${text(m).slice(0, 200)}`);
  await React.act(async () => { (el(m.host, "button.imgopen") as HTMLElement).dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  assert(el(m.host, "[data-canvas-drawer]"), "and a click opens the canvas");
  await m.unmount();
}

// ===== the table =====
{
  // Run from ui/, like the rest of the suite.
  const css: string = require("fs").readFileSync(require("path").join(process.cwd(), "src", "styles.css"), "utf8")
    .replace(/\/\*[\s\S]*?\*\//g, "");
  const rules = Array.from(css.matchAll(/([^{}]+)\{([^{}]*)\}/g) as Iterable<RegExpMatchArray>).map((r) => ({ sel: r[1].split(",").map((x) => x.trim()), body: r[2] }));
  const last = (selector: string, prop: string): string | null => {
    let v: string | null = null;
    for (const r of rules) {
      if (!r.sel.includes(selector)) continue;
      const hit = new RegExp(`(?:^|[;\\s])${prop}\\s*:\\s*([^;]+)`).exec(r.body);
      if (hit) v = hit[1].trim();
    }
    return v;
  };
  for (const sel of [".answer .issue-reader td:first-child", ".answer .issue-reader th:first-child", ".chat-bubble.issue-reader td:first-child"]) {
    equal(last(sel, "min-width"), "6rem", `${sel}: the first column keeps room for a word`);
    if (sel.includes("td")) equal(last(sel, "overflow-wrap"), "anywhere", `${sel}: break-word would let a long value widen the whole chat`);
    assert(last(sel, "white-space") !== "nowrap", `${sel}: it may wrap`);
  }
  equal(last(".answer .issue-reader td", "overflow-wrap"), "anywhere", "a later column still wraps a long hash");
  equal(last(".answer .issue-reader table", "width"), "100%", "the table fills the column and no more");
  equal(last(".answer .issue-reader table", "max-inline-size"), "100%", "and never wider than it");
  equal(last(".answer .issue-reader table", "display"), "table", "as a table, not a scroller");
}

console.log("image block tests passed");
}
main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
