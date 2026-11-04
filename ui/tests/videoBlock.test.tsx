/**
 * CAD-2133: the video block in the thread. The real component is mounted in
 * happy-dom against a fake of the host's video-job routes, with a manual clock.
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

let now = Date.parse("2026-11-04T08:00:00Z");
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
const VideoBlock = require("../src/features/canvas/VideoBlock").default;
const { VIDEO_LONG_WAIT_MS } = require("../src/features/canvas/VideoBlock");
const { POLL_MS } = require("../src/features/canvas/ImageBlock");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const thread = require("../src/features/home/thread") as typeof import("../src/features/home/thread");
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

const JOB = "med_vid0000000000000000000000000001";
const host = { job: { state: "running" } as Record<string, unknown>, statusReads: 0, byteReads: 0, bytesStatus: 200, bodyLen: 16, bodyMime: "video/mp4" };
const finished = { state: "finished", bytes: 16, mime: "video/mp4", digest: "a".repeat(64), duration: 12 };
globalThis.fetch = (async (input: unknown) => {
  const url = new URL(String(input), "http://localhost");
  equal(url.searchParams.get("conversation"), "c1", "every read names the conversation in view");
  if (url.pathname === `/api/video-jobs/${JOB}`) {
    host.statusReads++;
    return new Response(JSON.stringify(host.job), { status: 200, headers: { "Content-Type": "application/json" } });
  }
  if (url.pathname === `/api/video-jobs/${JOB}/bytes`) {
    host.byteReads++;
    if (host.bytesStatus !== 200) return new Response("{}", { status: host.bytesStatus });
    return new Response(new Uint8Array(host.bodyLen), { status: 200, headers: { "Content-Type": host.bodyMime } });
  }
  throw new Error(`Unexpected request ${url.pathname}`);
}) as typeof fetch;

const made: Blob[] = [];
(URL as any).createObjectURL = (b: Blob) => { made.push(b); return "blob:video-1"; };
(URL as any).revokeObjectURL = () => {};
const downloads: string[] = [];
const origAppend = document.body.appendChild.bind(document.body);
document.body.appendChild = ((n: any) => {
  if (n?.tagName === "A" && n.download) { downloads.push(n.download + "|" + n.href); n.click = () => {}; }
  return origAppend(n);
}) as typeof document.body.appendChild;
Object.defineProperty(globalThis, "IntersectionObserver", { get: () => undefined, configurable: true });

const entry = (over: Record<string, unknown> = {}) => ({
  seq: 7, role: "agent", kind: "video_job", text: "", message: "m1", created: new Date(now - 42_000).toISOString(),
  payload: { job_id: JOB, title: "Storefront", aspect: "16:9", duration: 12, ...over },
});
async function mount(node: any) {
  const hostEl = document.createElement("div");
  document.body.append(hostEl);
  const root = createRoot(hostEl);
  await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(ImageConversation.Provider, { value: "c1" }, node))); });
  await settle();
  return { host: hostEl, unmount: async () => { await React.act(async () => { root.unmount(); }); hostEl.remove(); } };
}
const el = (root: ParentNode, sel: string) => root.querySelector(sel) as HTMLElement | null;
const reset = () => { host.job = { state: "running" }; host.statusReads = 0; host.byteReads = 0; host.bytesStatus = 200; host.bodyLen = 16; host.bodyMime = "video/mp4"; made.length = 0; timers.clear(); };

// malformed entries render nothing and ask nothing
reset();
for (const bad of [{ job_id: "job_x" }, { title: 1 }, { aspect: "3:2" }, { duration: 0 }, { duration: "12" }]) {
  const m = await mount(React.createElement(VideoBlock, { entry: entry(bad) }));
  equal(m.host.innerHTML, "", `malformed ${JSON.stringify(bad)} renders nothing`);
  await m.unmount();
}
equal(host.statusReads, 0, "a malformed entry issues no request");

// the thread turns a video_job into a video block, not a system divider
{
  const items = thread.threadItems({ entries: [entry() as any] } as any);
  equal(items.map((i) => i.type), ["video"], "a video_job entry is a video item");
}

// running, then the long wait, then finished: bytes only on play
reset();
{
  const m = await mount(React.createElement(VideoBlock, { entry: entry() }));
  assert(m.host.textContent!.includes("Creating your video"), "running reads Creating your video");
  equal(el(m.host, ".imgblock")!.getAttribute("data-shape"), "16:9", "the box carries the shape");
  await advance(VIDEO_LONG_WAIT_MS);
  assert(m.host.textContent!.includes("Still creating your video"), "after about ten minutes it reads as a long wait");
  host.job = finished;
  await advance(POLL_MS);
  assert(el(m.host, ".vidopen"), "finished shows the poster with a play button");
  assert(m.host.textContent!.includes("0:12"), "and the duration");
  assert(m.host.textContent!.includes("16:9 · MP4 · 0:12 · 16 B"), `the caption carries shape, format, duration, size: ${m.host.textContent}`);
  equal(host.byteReads, 0, "no bytes are fetched on load");
  await React.act(async () => { el(m.host, ".vidopen")!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
  equal(host.byteReads, 1, "play fetches the bytes once");
  const v = el(m.host, "video")!;
  assert(v && v.hasAttribute("controls") && v.getAttribute("src") === "blob:video-1", "and plays inline from a blob with native controls");
  equal(made[0].type, "video/mp4", "the blob carries the video type");
  await m.unmount();
}

// too large is its own card with no Check again; unclear keeps it
reset();
{
  host.job = { state: "too_large" };
  const m = await mount(React.createElement(VideoBlock, { entry: entry() }));
  assert(m.host.textContent!.includes("too large"), "too large says so");
  assert(m.host.textContent!.includes("shorter or lower-resolution"), "and what to ask for");
  assert(!m.host.textContent!.includes("Check again"), "with no Check again");
  await m.unmount();
  host.job = { state: "unclear" };
  const u = await mount(React.createElement(VideoBlock, { entry: entry() }));
  assert(u.host.textContent!.includes("Check again"), "unclear offers Check again");
  await u.unmount();
}

// a finished answer that is not a playable video reads as not-done, never success
reset();
{
  host.job = { state: "finished", bytes: 5, mime: "text/html", duration: 12 };
  const m = await mount(React.createElement(VideoBlock, { entry: entry() }));
  assert(!el(m.host, ".vidopen"), "a malformed finished answer shows no player");
  await m.unmount();
}

const click = async (m: { host: HTMLElement }, sel: string) => {
  await React.act(async () => { el(m.host, sel)!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await settle();
};
const LOAD_ERROR = "Couldn't load the video. Try again.";

// download fetches once and saves a named file
reset();
{
  host.job = finished;
  const m = await mount(React.createElement(VideoBlock, { entry: entry() }));
  await click(m, ".imgtool");
  equal(host.byteReads, 1, "download fetches the bytes");
  equal(downloads, ["Storefront.mp4|blob:video-1"], "and saves a named mp4");
  await click(m, ".vidopen");
  equal(host.byteReads, 1, "play after download reuses the bytes");
  await m.unmount();
}

// a temporary fault shows the line and the next click tries again
reset();
{
  host.job = finished;
  host.bytesStatus = 503;
  const m = await mount(React.createElement(VideoBlock, { entry: entry() }));
  await click(m, ".vidopen");
  assert(m.host.textContent!.includes(LOAD_ERROR), "a temporary fault shows the load-error line");
  assert(!el(m.host, "video"), "and no player");
  host.bytesStatus = 200;
  await click(m, ".vidopen");
  equal(host.byteReads, 2, "the next click asks again");
  assert(el(m.host, "video"), "and plays");
  await m.unmount();
}

// bytes that are not the video: wrong type is final, empty or short is retryable
reset();
{
  host.job = finished;
  host.bodyMime = "text/html";
  const m = await mount(React.createElement(VideoBlock, { entry: entry() }));
  await click(m, ".vidopen");
  assert(m.host.textContent!.includes("This video isn't available."), "a mime mismatch reads as unavailable");
  assert(!el(m.host, "video"), "with no player");
  await m.unmount();
  host.bodyMime = "video/mp4";
  host.bodyLen = 0;
  const e = await mount(React.createElement(VideoBlock, { entry: entry() }));
  await click(e, ".vidopen");
  assert(e.host.textContent!.includes(LOAD_ERROR), "empty bytes show the load-error line");
  assert(!el(e.host, "video"), "and no dead player");
  await e.unmount();
}

// bytes the browser cannot decode: the player's error brings the line back
reset();
{
  host.job = finished;
  const m = await mount(React.createElement(VideoBlock, { entry: entry() }));
  await click(m, ".vidopen");
  await React.act(async () => { el(m.host, "video")!.dispatchEvent(new win.Event("error")); });
  await settle();
  assert(!el(m.host, "video"), "an undecodable video drops the player");
  assert(m.host.textContent!.includes(LOAD_ERROR), "and shows the load-error line");
  await click(m, ".vidopen");
  equal(host.byteReads, 2, "and a click fetches afresh");
  await m.unmount();
}

// give-up and cap
reset();
{
  const old = entry();
  old.created = new Date(now - 25 * 3600_000).toISOString();
  const m = await mount(React.createElement(VideoBlock, { entry: old }));
  assert(m.host.textContent!.includes("We can't confirm this video yet"), "a job running past 24 h reads as unconfirmed");
  await m.unmount();
  host.job = { ...finished, bytes: 100 * 1024 * 1024 + 1 };
  const c = await mount(React.createElement(VideoBlock, { entry: entry() }));
  assert(c.host.textContent!.includes("too large"), "a finished video over the cap reads as too large");
  assert(!el(c.host, ".vidopen"), "with no player");
  await c.unmount();
}

// a finished answer without a valid digest is malformed
reset();
{
  for (const bad of [undefined, "abc", "A".repeat(64)]) {
    host.job = { ...finished, digest: bad };
    const m = await mount(React.createElement(VideoBlock, { entry: entry() }));
    assert(!el(m.host, ".vidopen"), `digest ${String(bad).slice(0, 6)} is not finished`);
    await m.unmount();
  }
}

console.log("video block tests passed");
}
main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
