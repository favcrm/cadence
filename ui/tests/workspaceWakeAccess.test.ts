/**
 * CAD-1351 / CAD-1452 independent acceptance, authored by AuthRefusalSpec
 * from the approved private-board acceptance and the original CAD-1351
 * safety contract. Implementation authors MUST NOT edit or weaken it.
 * This is independent test authorship, not product-code or merge approval.
 *
 * Mounts the real LocaleProvider + App, with only browser clocks, decorative
 * modules and HTTP responses supplied by the fixture. No admission predicate,
 * access hook, private cache or component is replaced by a test implementation.
 *
 * Preserved safety outcomes: proved operator positive control; unanswered,
 * failed and timed-out checks never confer visible private/read/write access
 * or pretend logout; the existing tab key survives; retry grants nothing
 * before proof; recovery restores only the authority actually proved.
 * CAD-1452 intentionally supersedes the old 60-second retry withholding and
 * page reload: recovery is immediately available and nondestructive instead.
 * No exact deadline duration, incidental heading or obsolete copy is pinned.
 */
import type * as ReactModule from "react";
import type * as ReactDOMModule from "react-dom/client";
import type * as AppModule from "../src/App";
import type * as LocaleModule from "../src/lib/locale";
import type * as SessionModule from "../src/lib/sessionKey";

declare function require(name: string): unknown;
type TestWindow = Pick<Window & typeof globalThis,
  "location" | "HTMLInputElement" | "getComputedStyle" | "addEventListener" | "removeEventListener"
> & {
  setTimeout: (callback: (...args: unknown[]) => void, delay?: number, ...args: unknown[]) => number;
  clearTimeout: (id: number) => void;
  happyDOM: { close: () => Promise<void> };
};
// The DOM fixture supplies browser-compatible objects at this in-process seam.
const { Window } = require("happy-dom") as { Window: new (options: { url: string }) => TestWindow };
const win = new Window({ url: "http://localhost/projects/cadence/issues?view=kanban" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "MutationObserver", "Event", "MouseEvent", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : Reflect.get(win, name), configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"] as const)
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
// Follow the App-test convention of disabling ambient recurring intervals.
// Window timeouts remain driveable, including reconnect/poll callbacks: an
// unavailable boundary must not launch private work when those become due.
Object.defineProperty(globalThis, "setInterval", { value: () => 1, configurable: true });
Object.defineProperty(globalThis, "clearInterval", { value: () => undefined, configurable: true });
const realTimeout = win.setTimeout.bind(win);
const realClearTimeout = win.clearTimeout.bind(win);
let clock = 0, timerId = 1_000_000;
const timers = new Map<number, { due: number; run: () => void }>();
win.setTimeout = (run: (...args: unknown[]) => void, delay = 0, ...args: unknown[]) => {
  if (delay < 1_000) return realTimeout(run, delay, ...args);
  const id = ++timerId;
  timers.set(id, { due: clock + delay, run: () => run(...args) });
  return id;
};
win.clearTimeout = (id: number) => {
  if (!timers.delete(id)) realClearTimeout(id);
};
let reloads = 0, legacyStreamStarts = 0;
Object.defineProperty(win.location, "reload", { value: () => { reloads++; }, configurable: true });
Object.defineProperty(globalThis, "EventSource", { value: class {
  readyState = 0;
  constructor() { legacyStreamStarts++; }
  addEventListener() {}
  close() {}
}, configurable: true });
type ModuleLoader = { prototype: { require: (this: unknown, id: string) => unknown } };
const loader = require("module") as ModuleLoader;
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  if (id === "react-markdown") return { __esModule: true, default: () => null };
  if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
  return originalRequire.call(this, id);
};
const React = require("react") as typeof ReactModule;
const { createRoot } = require("react-dom/client") as typeof ReactDOMModule;
const AppView = (require("../src/App") as typeof AppModule).default;
const { LocaleProvider } = require("../src/lib/locale") as typeof LocaleModule;
const { sessionKey, SESSION_HEADER } = require("../src/lib/sessionKey") as typeof SessionModule;
const App = () => React.createElement(LocaleProvider, null, React.createElement(AppView));
const KEY = "independent-workspace-wake-tab";
const PRIVATE_RECORD = "Independent private recovery record";
const PRIVATE_DRAFT = "Unsent same-tab recovery draft";
const proof = {
  hosted: false, anonymous_reads: false, read_only: false,
  signed_in: true, operator: true, actor: "operator",
  session: {
    id: "independent-existing-session", origin: "loopback",
    created: 1700000000, last_used: 1700000000,
    idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "acceptance",
    user: { sub: "operator", handle: "operator", name: "Operator", email: "operator@example.test", role: "operator" },
  },
};
const json = (data: unknown) => new Response(JSON.stringify(data), { headers: { "Content-Type": "application/json" } });
type Call = { path: string; method: string; key: string | null };
type Probe = { done: boolean; answer: (response: Response) => void; fail: (error: Error) => void };
const calls: Call[] = [];
const probes: Probe[] = [];
const streams: { signal: AbortSignal | null; closed: boolean }[] = [];
globalThis.fetch = async (input, init) => {
  const path = new URL(String(input), "http://localhost").pathname;
  calls.push({ path, method: init?.method ?? "GET", key: new Headers(init?.headers).get(SESSION_HEADER) });
  // The repository test target uses ES2023, before Promise.withResolvers.
  if (path === "/api/meta") return new Promise<Response>((resolve, reject) => {
    const probe: Probe = {
      done: false,
      answer(response) { if (!probe.done) { probe.done = true; resolve(response); } },
      fail(error) { if (!probe.done) { probe.done = true; reject(error); } },
    };
    probes.push(probe);
    init?.signal?.addEventListener("abort", () => probe.fail(new Error("fixture metadata request aborted")), { once: true });
  });
  if (path.endsWith("/stream")) {
    const stream = { signal: init?.signal ?? null, closed: false };
    streams.push(stream);
    const body = new ReadableStream({
      start(controller) {
        const abort = () => { stream.closed = true; controller.error(new Error("fixture stream aborted")); };
        if (stream.signal?.aborted) abort();
        else stream.signal?.addEventListener("abort", abort, { once: true });
      },
      cancel() { stream.closed = true; },
    });
    return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
  }
  if (path === "/api/health") return json({ daemon: "reachable" });
  if (path === "/api/issues") return json({ issues: [{
    id: "CAD-1", project: "cadence", title: PRIVATE_RECORD, status: "backlog", status_source: "file", priority: "P2",
    blocked_by: [], relates: [], refs: [], container: false, ready: true, blocked: false,
    created: "2026-10-11", rev: "fixture", counts: { comments: 0, artifacts: 0, refs: 0 }, checks: { done: 0, total: 0 },
  }] });
  if (path === "/api/projects") return json({ projects: [{ key: "cadence", prefix: "CAD", components: [], repos: [], issues: 1 }] });
  if (path === "/api/agents") return json({ agents: [], by_issue: {}, daemon: "reachable" });
  if (path === "/api/update/banner") return json(null);
  return json({});
};
function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = async () => {
  for (let i = 0; i < 3; i++) await React.act(async () => {
    await Promise.resolve();
  });
};
const visible = (element: Element) => {
  for (let node: Element | null = element; node; node = node.parentElement) {
    const style = win.getComputedStyle(node);
    if (node.hasAttribute("hidden") || style.display === "none" || style.visibility === "hidden") return false;
  }
  return true;
};
const visibleElements = (selector: string) => Array.from(host.querySelectorAll(selector)).filter(visible);
const buttons = (label: RegExp) => visibleElements("button").filter((button) => label.test(button.textContent?.trim() ?? "")) as HTMLButtonElement[];
const canCreate = () => buttons(/^New issue$/).length > 0;
const privateCalls = () => calls.filter((call) => call.path !== "/api/meta" && call.path !== "/api/health" && call.path !== "/api/version");
const pending = () => {
  const active = probes.filter((probe) => !probe.done);
  assert(active.length === 1, `one active verification request is expected (got ${active.length})`);
  return active[0];
};
const focus = async () => {
  await React.act(async () => window.dispatchEvent(new Event("focus")));
  await flush();
};
const respond = async (data: unknown) => {
  const probe = pending();
  await React.act(async () => probe.answer(json(data)));
  await flush();
};
const fail = async () => {
  const probe = pending();
  await React.act(async () => probe.fail(new Error("independent temporary relay outage")));
  await flush();
};
const unknown = (when: string) => {
  assert(visibleElements("[data-auth-state='checking'] h1, [data-auth-state='unavailable'] h1").length === 1, `${when}: access is visibly checking/unavailable`);
  assert(visibleElements("[data-auth-state='signedout'], [data-auth-state='expired'], [data-auth-state='refused']").length === 0, `${when}: uncertainty is not a confirmed sign-out or refusal`);
  assert(visibleElements("[data-app-shell], aside, article, form.board-new-issue").length === 0, `${when}: private navigation, records and drafts stay concealed`);
  assert(!canCreate(), `${when}: no issue creation authority is offered`);
  assert(visibleElements('[aria-label="Read-only. Sign in to make changes"]').length === 0, `${when}: no signed-out pill`);
  assert(buttons(/sign in|continue with AgenticOS/i).length === 0, `${when}: no false sign-in action`);
  assert(sessionKey() === KEY && sessionStorage.getItem("cadence.sessionKey") === KEY, `${when}: the existing same-tab credential survives`);
};
const retry = async () => {
  const controls = buttons(/retry/i);
  assert(controls.length === 1 && !controls[0].disabled, "verification recovery is immediately available without waiting for a cooldown");
  const before = calls.length;
  await React.act(async () => controls[0].click());
  await flush();
  const made = calls.slice(before);
  assert(made.length === 1 && made[0].path === "/api/meta" && made[0].method === "GET", `retry starts one verification, no private read/write: ${JSON.stringify(made)}`);
  assert(reloads === 0, "retry must not reload away the tab's in-memory state");
  unknown("retry awaiting proof");
};
async function advance(milliseconds: number) {
  const until = clock + milliseconds;
  for (;;) {
    const next = Array.from(timers.entries()).filter(([, timer]) => timer.due <= until).sort((a, b) => a[1].due - b[1].due)[0];
    if (!next) break;
    const [id, timer] = next;
    timers.delete(id);
    clock = timer.due;
    await React.act(async () => timer.run());
    await flush();
  }
  clock = until;
}
async function run() {
  // Seed the browser store, as for a same-tab reload. No login event and no
  // fake setter is used to manufacture admission before the first probe.
  sessionStorage.setItem("cadence.sessionKey", KEY);
  try {
    await React.act(async () => root.render(React.createElement(App)));
    await flush();
    pending();
    unknown("initial existing-session verification");
    assert(!host.querySelector("[data-app-shell], aside, article, form.board-new-issue"), "initial unknown access never mounts the private workspace");
    assert(privateCalls().length === 0 && legacyStreamStarts === 0, "initial pending proof starts no private read or subscription");

    await respond(proof);
    assert(canCreate(), "positive control: a proved writable operator sees New issue");
    assert(visibleElements("article").some((article) => article.textContent?.includes(PRIVATE_RECORD)), "positive control: actual private HTTP fixture data reaches the real board");
    assert(privateCalls().some((call) => call.path === "/api/issues"), "positive control: actual App starts admitted private reads");
    assert(streams.length > 0, "positive control: actual App starts an authenticated stream");

    await React.act(async () => buttons(/^New issue$/)[0].click());
    const title = host.querySelector('form.board-new-issue input[name="title"]') as HTMLInputElement | null;
    assert(title, "positive control: real issue draft opens");
    await React.act(async () => {
      Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(title, PRIVATE_DRAFT);
      title.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const submit = host.querySelector<HTMLButtonElement>('form.board-new-issue button[type="submit"]');
    assert(submit && visible(submit) && !submit.disabled, "positive control: the populated real draft has enabled creation capability");

    await focus();
    await fail();
    unknown("transport failure after proved access");
    assert(streams.every((stream) => stream.closed || stream.signal?.aborted), "unknown access closes/aborts previously admitted private streams");
    assert(title.value === PRIVATE_DRAFT && !visible(title), "temporary failure preserves but conceals the unsent same-session draft");
    const readsAtOutage = privateCalls().length, legacyAtOutage = legacyStreamStarts;

    await retry();
    assert(privateCalls().length === readsAtOutage, "pending retry starts no private read or stream");
    // Any supported bounded verification must finish by this generous outer
    // bound; its exact duration is intentionally not an acceptance detail.
    assert(timers.size > 0, "pending verification arms a bounded deadline");
    await advance(60_000);
    unknown("verification deadline elapsed");
    assert(visibleElements("[data-auth-state='unavailable'] h1").length === 1, "timed-out verification remains unavailable, never manufactured sign-out");
    assert(privateCalls().length === readsAtOutage && legacyStreamStarts === legacyAtOutage, "timeouts and queued callbacks grant no reads or subscriptions");

    await retry();
    // A response can itself say 'unknown'. Old positive operator hints are
    // not an authentication grant and must not resurrect the previous UI.
    await respond({ ...proof, signed_in: null, operator: true });
    unknown("answered but indeterminate session");
    assert(privateCalls().length === readsAtOutage, "indeterminate metadata does not resume workspace reads");

    await retry();
    await respond(proof);
    assert(visible(submit) && !submit.disabled, "answered operator recovery restores the preserved form's proved submit capability");
    assert(visible(title) && title.value === PRIVATE_DRAFT, "same-session recovery restores the original in-memory draft without reload");
    assert(privateCalls().length > readsAtOutage, "proved recovery resumes private reads");

    // A second outage must not retain the earlier writable grant when the
    // next successful answer proves only authenticated read-only access.
    await focus();
    await fail();
    unknown("second transport failure");
    const readsBeforeReadOnlyProof = privateCalls().length;
    await retry();
    assert(privateCalls().length === readsBeforeReadOnlyProof, "the next retry still grants nothing before its answer");
    await respond({ ...proof, read_only: true, operator: false });
    assert(visibleElements("[data-app-shell]").length === 1, "proved authenticated read-only access opens the private workspace");
    assert(!canCreate(), "read-only recovery cannot inherit the earlier operator's creation control");
    assert(visible(submit) && submit.disabled, "read-only recovery disables the preserved draft's submit capability");
    assert(visibleElements("article").some((article) => article.textContent?.includes(PRIVATE_RECORD)), "read-only recovery restores proved read access, not a blanket denial");
    assert(sessionKey() === KEY && reloads === 0, "all recovery paths retain the original tab credential without reload");
    assert(calls.filter((call) => call.path === "/api/meta").every((call) => call.key === KEY), "every verification reuses the existing tab key");
    assert(calls.every((call) => call.method === "GET"), "checking, retries and local draft editing never issue a write or mint a session");
    console.log("workspaceWakeAccess: independent App unknown-access, bounded retry and proved recovery acceptance passed");
  } finally {
    await React.act(async () => root.unmount());
    loader.prototype.require = originalRequire;
    await win.happyDOM.close();
  }
}
run().catch((error) => { console.error(error); throw error; });
export {};
