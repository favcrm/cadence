import {
  FORBIDDEN_BODY_KEYS,
  hostActions,
} from "../src/features/app-shell/hostActions";
import { ApiError } from "../src/lib/api";

export {};
/** CAD-802 corrections: grammar + single-pane shell + scoped links. */
declare function require(name: string): any;

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
async function rejected(action: () => Promise<unknown>, check: (error: unknown) => boolean, why: string) {
  try {
    await action();
  } catch (error) {
    assert(check(error), why);
    return;
  }
  throw new Error(`${why}: expected a refusal`);
}

async function main() {
// The host client derives identity from the trusted route/scope only,
// and its bodies carry exactly the operator-checked grammar.
equal(
  hostActions.paths.listPath({ installId: "install-a", contextId: "ctx-b" }),
  "/api/app-installations/install-a/contexts/ctx-b/records",
  "list path binds the URL scope",
);
equal(
  hostActions.paths.recordPath({ installId: "install-a", contextId: "ctx-b" }, "rec-1"),
  "/api/app-installations/install-a/contexts/ctx-b/records/rec-1",
  "record path binds install, context and record",
);
equal(
  hostActions.paths.updatePath({ installId: "install-a", contextId: "ctx-b" }, "rec-1"),
  "/api/app-installations/install-a/contexts/ctx-b/records/rec-1/update",
  "update path binds install, context and record",
);
for (const scope of [
  { installId: "../x", contextId: "ctx-b" },
  { installId: "install-a", contextId: "a/b" },
  { installId: "", contextId: "ctx-b" },
]) {
  let threw = false;
  try {
    hostActions.paths.listPath(scope);
  } catch {
    threw = true;
  }
  assert(threw, `scope escapes refused: ${JSON.stringify(scope)}`);
}
// Forged authority fields never serialize: the client throws before fetch.
for (const key of FORBIDDEN_BODY_KEYS) {
  const allowed = key === "record_id" ? ["expected_revision", "profile"] : ["record_id", "profile"];
  let threw = false;
  try {
    hostActions.guards.assertCleanBody({ record_id: "r", profile: {}, [key]: "forged" } as Record<string, unknown>, allowed);
  } catch {
    threw = true;
  }
  assert(threw, `forged body field refused client-side: ${key}`);
}
hostActions.guards.assertCleanBody({ record_id: "r", profile: {} }, ["record_id", "profile"]);
hostActions.guards.assertCleanBody({ expected_revision: 3, profile: {} }, ["expected_revision", "profile"]);

// Wire proof: every byte the client sends carries only the allowed keys.
type Call = { path: string; init: RequestInit };
const calls: Call[] = [];
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  calls.push({ path: String(input), init: init ?? {} });
  const path = String(input);
  if (path === "/api/app-installations/install-a/contexts/ctx-b/records" && !init?.body) {
    return new Response(JSON.stringify({ records: [] }), { status: 200 });
  }
  const body = init?.body ? JSON.parse(String(init.body)) : null;
  return new Response(JSON.stringify({ record: { id: "rec-1", install_id: "install-a", context_id: "ctx-b", kind: "customer", revision: 1, digest: "d1", profile: body?.profile ?? null } }), { status: 200 });
}) as typeof fetch;
await hostActions.list({ installId: "install-a", contextId: "ctx-b" });
await hostActions.create({ installId: "install-a", contextId: "ctx-b" }, "rec-1", { name: "Ada" });
await hostActions.update({ installId: "install-a", contextId: "ctx-b" }, "rec-1", 2, { name: "Ada B" });
equal(calls.length, 3, "three wire calls");
equal(Object.keys(JSON.parse(String(calls[1].init.body))).sort(), ["profile", "record_id"], "create body grammar is exact");
equal(Object.keys(JSON.parse(String(calls[2].init.body))).sort(), ["expected_revision", "profile"], "update body grammar is exact");
const NEVER_ON_WIRE = ["by", "actor", "workspace", "project", "project_link", "sql", "path", "secret", "install_id", "context_id"];
for (const call of calls) {
  const body = call.init.body ? JSON.parse(String(call.init.body)) : {};
  for (const key of Object.keys(body)) {
    assert(!NEVER_ON_WIRE.includes(key), `wire body carries no forged key: ${key}`);
  }
}
await rejected(
  () => hostActions.update({ installId: "install-a", contextId: "ctx-b" }, "rec-1", 0, {}),
  (e) => e instanceof ApiError,
  "stale/zero revision refused before the wire",
);
await rejected(
  () => hostActions.show({ installId: "install-a", contextId: "ctx-b" }, "../escape"),
  (e) => e instanceof ApiError,
  "record path escape refused before the wire",
);

// Mounted shell: one chat pane/stream, scoped links, switch clearing,
// single context owner, verified send binding, neutral outlet copy.
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-shell" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
let eventSources = 0;
Object.defineProperty(globalThis, "EventSource", {
  configurable: true,
  value: class {
    onopen: null = null;
    onerror: null = null;
    constructor() { eventSources += 1; }
    addEventListener() {}
    removeEventListener() {}
    close() {}
  },
});
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const AppShellModule = (require("../src/features/app-shell/AppShell") as typeof import("../src/features/app-shell/AppShell"));
const AppShell = AppShellModule.default;
const entryApp = AppShellModule.entryApp;
// Context moves ride real URL writes — the same `navigate` the shell
// uses, so tests exercise adoption exactly like a pasted link.
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
// Only a daemon-stamped binding renders: browser claims never do.
// The stamp trims to ids — revision/digest stay server-side.
equal(
  entryApp({ app: { install_id: "i", context_id: "c", verified: true, context_revision: 2, context_digest: "d" } }),
  { install_id: "i", context_id: "c" },
  "daemon stamp renders trimmed",
);
equal(entryApp({ source: "operator" }), null, "unstamped entries render no chip");
equal(entryApp({ app: { install_id: "i", context_id: "c", verified: "yes" } }), null, "string verified renders nothing");
equal(entryApp({ app: { install_id: "i", context_id: "c" } }), null, "missing verified renders nothing");
equal(entryApp(null), null, "missing payload renders nothing");

const generic = {
  install_id: "install-shell", title: "Reports", name: "reports", version: "0.1.0",
  digest: "install-digest", catalog_generation: "gen-1", approved: true,
  storage_kind: "workspace", files: [], capabilities: null, connection_slots: [],
};
const social = { ...generic, install_id: "install-social", title: "Social Content", name: "social-content" };
const second = { ...generic, install_id: "install-second", title: "Second", name: "reports" };
const contextsFor: Record<string, unknown> = {
  "install-shell": { contexts: [
    { id: "ctx-a", install_id: "install-shell", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
    { id: "ctx-b", install_id: "install-shell", revision: 2, state: "active", digest: "cb", config: { schema: 1, label: "Beta", input_defaults: {} } },
  ] },
  "install-second": { contexts: [
    { id: "ctx-only", install_id: "install-second", revision: 1, state: "active", digest: "co", config: { schema: 1, label: "Only", input_defaults: {} } },
  ] },
  "install-social": { contexts: [
    { id: "ctx-brand", install_id: "install-social", revision: 1, state: "active", digest: "cs", config: { schema: 1, label: "Brand", input_defaults: {} } },
    { id: "ctx-beta", install_id: "install-social", revision: 1, state: "active", digest: "cb2", config: { schema: 1, label: "Beta brand", input_defaults: {} } },
  ] },
};
const thread = {
  entries: [
    { seq: 1, role: "operator", kind: "message", message: "m1", text: "Operator question", created: "2026-09-29T07:00:00Z", payload: { source: "operator" } },
    { seq: 2, role: "agent", kind: "turn_result", message: "m2", text: "Master answer stays connected", created: "2026-09-29T07:01:00Z", payload: { source: "master" } },
    { seq: 3, role: "operator", kind: "message", message: "m3", text: "Scoped follow-up", created: "2026-09-29T07:02:00Z",
      payload: { source: "operator", app: { install_id: "install-shell", context_id: "ctx-a", verified: true, context_revision: 1, context_digest: "ca" } } },
  ],
  more_before: false,
};
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
const posts: { path: string; body: any }[] = [];
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const path = String(input);
  if (init?.method === "POST") {
    posts.push({ path, body: JSON.parse(String(init.body)) });
    return json({ ok: true });
  }
  if (path === "/api/app-installations/install-shell") return json(generic);
  if (path === "/api/app-installations/install-second") return json(second);
  if (path === "/api/app-installations/install-social") return json(social);
  if (path === "/api/app-installations/install-shell/contexts") return json(contextsFor["install-shell"]);
  if (path === "/api/app-installations/install-second/contexts") return json(contextsFor["install-second"]);
  if (path === "/api/app-installations/install-social/contexts") return json(contextsFor["install-social"]);
  if (path.startsWith("/api/threads/master")) return json(thread);
  throw new Error(`Unexpected read ${path}`);
}) as typeof fetch;

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
const text = () => host.textContent ?? "";
async function click(element: Element | undefined | null) {
  assert(element, "click target exists");
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await flush();
}
async function fill(selector: string, value: string) {
  const element = host.querySelector(selector) as HTMLTextAreaElement | HTMLInputElement;
  assert(element, `field exists: ${selector}`);
  const proto = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  await React.act(async () => {
    Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
}
const panes = () => host.querySelectorAll("[data-chat-pane]").length;
const boxes = () => host.querySelectorAll("#app-shell-chat-box").length;
async function settle(check: () => void) {
  for (let i = 0; i < 25; i++) {
    await flush();
    try { check(); return; } catch { /* keep polling */ }
  }
  await flush();
  check();
}

// A new browser: no stored preference, no scope in the link.
win.sessionStorage.clear();
history.pushState(null, "", "/app-installations/install-shell");
await React.act(async () => {
  root.render(React.createElement(AppShell, { installId: "install-shell", viewer: { operator: true, readOnly: false } }));
});
await flush(); await flush(); await flush();
assert(text().includes("All apps") && text().includes("Reports"), "← All apps back-link plus installed App title, deduped");
assert(text().includes("No records yet"), "generic outlet is truthfully empty, never fake rows");
assert(text().includes("not installed yet") && !text().includes("CAD-781"), "non-CRM outlet copy stays neutral");
assert(text().includes("Master answer stays connected"), "the left pane shows the real master thread");
assert(text().includes("✓ ctx-a"), "the read-back verified stamp renders, never a sent claim");
equal(panes(), 1, "exactly one chat pane in the document");
equal(boxes(), 1, "exactly one chat draft box id in the document");
equal(eventSources, 1, "exactly one SSE subscription for the shell");
assert(!host.querySelector('[aria-label="App context"]'), "context selector removed — context follows URL only");

// Drawer keyboard: toggle opens into the pane, Escape closes back to
// the trigger, the closed drawer keeps no tab stop yet keeps the draft.
const toggle = host.querySelector(".app-shell-chat-toggle") as HTMLButtonElement;
assert(toggle && toggle.getAttribute("aria-controls") === "app-shell-chat", "toggle controls the single chat node");
equal(toggle.getAttribute("aria-expanded"), "false", "drawer starts closed");
assert(!host.querySelector("#app-shell-chat")?.hasAttribute("data-open"), "closed drawer carries no open marker");
await click(toggle);
equal(toggle.getAttribute("aria-expanded"), "true", "drawer opens with accessible state");
// Focus is deferred two frames so it lands after the pointer/keyboard
// activation's own focus; flush lets each queued frame run.
await flush(); await flush(); await flush();
assert(document.activeElement?.id === "app-shell-chat-box", "opening moves focus into the pane");
await fill("#app-shell-chat-box", "unsent shell draft");
await React.act(async () => {
  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
});
await flush();
equal(toggle.getAttribute("aria-expanded"), "false", "Escape closes the drawer");
assert(document.activeElement === toggle, "closing returns focus to the trigger");
equal(eventSources, 1, "drawer cycles open no second stream");
// Controlled-frame proof that closing mid-deferral cancels the queued
// composer focus instead of stealing it back. Capture the rAF queue so
// we can run the outer frame (which schedules the inner focus frame),
// close before the inner frame fires, then prove the pending inner
// frame was cancelled — the composer is never focused after close.
const rafQueue = new Map<number, FrameRequestCallback>();
let rafNext = 0;
const cancelled = new Set<number>();
const realRaf = globalThis.requestAnimationFrame;
const realCaf = globalThis.cancelAnimationFrame;
globalThis.requestAnimationFrame = ((cb: FrameRequestCallback) => {
  const id = ++rafNext;
  rafQueue.set(id, cb);
  return id;
}) as typeof requestAnimationFrame;
globalThis.cancelAnimationFrame = ((id: number) => {
  cancelled.add(id);
  rafQueue.delete(id);
}) as typeof cancelAnimationFrame;
try {
  await React.act(async () => {
    toggle.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
  equal(toggle.getAttribute("aria-expanded"), "true", "drawer open before the deferred frame runs");
  // The outer rAF is queued; run it so it schedules the inner focus frame.
  equal(rafQueue.size, 1, "one outer frame queued");
  const outerCb = rafQueue.get(1)!;
  rafQueue.delete(1);
  await React.act(async () => { outerCb(performance.now()); });
  equal(rafQueue.size, 1, "outer frame queued the inner focus frame");
  // Close before the inner focus frame fires.
  await React.act(async () => {
    toggle.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
  equal(toggle.getAttribute("aria-expanded"), "false", "closed before the focus frame");
  assert(cancelled.has(2), "the pending inner focus frame was cancelled on close");
  equal(rafQueue.size, 0, "no frame survives to refocus after close");
  assert(document.activeElement === toggle, "focus returned to the trigger, never the composer");
} finally {
  globalThis.requestAnimationFrame = realRaf;
  globalThis.cancelAnimationFrame = realCaf;
}
await click(toggle);
equal((host.querySelector("#app-shell-chat-box") as HTMLTextAreaElement)?.value, "unsent shell draft", "one pane keeps one draft across close/open");
await click(toggle);

// Chat sends carry the verified-scope request; refusal would surface.
// Context is adopted from the URL — no selector needed.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-a"); });
await flush();
assert(location.search.includes("ctx=ctx-a"), "context is adopted from the URL");
posts.length = 0;
await fill("#app-shell-chat-box", "scoped question");
await click(host.querySelector(".app-chat-form button[type=submit]"));
await flush();
const chatPost = posts.find((p) => p.path === "/api/threads/master/messages");
assert(chatPost, "chat send posted");
equal(chatPost?.body.app, { install_id: "install-shell", context_id: "ctx-a" }, "send binds the selected scope, never actor claims");
assert(!("actor" in (chatPost?.body ?? {})) && !("verified" in (chatPost?.body ?? {})), "send carries no authority claim");

// Outlet navigation: New view + typed draft, then a URL context change
// clears the view, the selection and the unsaved draft.
const newTab = Array.from(host.querySelectorAll(".app-outlet-tab")).find((el) => el.textContent === "New");
await click(newTab);
assert(location.search.includes("appview=new"), "New view lands in the URL for direct links");
await fill("#app-outlet-draft", "typed outlet draft");
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-b"); });
await flush();
assert(!location.search.includes("appview="), "context switch returns the outlet to the list");
assert(!location.search.includes("record="), "context switch clears the stale selection");
assert(location.search.includes("ctx=ctx-b"), "context switch binds the new scope in the URL");
const newTab2 = Array.from(host.querySelectorAll(".app-outlet-tab")).find((el) => el.textContent === "New");
await click(newTab2);
equal((host.querySelector("#app-outlet-draft") as HTMLInputElement)?.value, "", "unsaved draft dies with the context switch");

// Authored cross-context links keep their params verbatim: a URL
// carrying ctx+record together is a deep link for that ctx — record
// ids are scoped per-context, so the same id may legitimately exist
// under a different scope. Adoption must not strip it.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-a&record=rec-1"); });
await flush(); await flush();
assert(location.search.includes("record=rec-1"), "deep link lands its record");
assert(text().includes("Record details"), "record drawer opens on the deep link");
// An authored ctx+record link for a different context keeps its record.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-b&record=rec-2"); });
await flush(); await flush();
assert(location.search.includes("ctx=ctx-b") && location.search.includes("record=rec-2"), "cross-context record link is preserved");
assert(text().includes("Record details"), "the new context's record opens");
// An authored cross-context New-view link keeps its view too.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-a&appview=new"); });
await flush(); await flush();
assert(location.search.includes("ctx=ctx-a") && location.search.includes("appview=new"), "cross-context New view link is preserved");
assert(host.querySelector("#app-outlet-draft"), "the New form opens for the linked scope");
// A context-only transition (the scoped-entry link shape: bare ctx,
// no record/appview) lands clean — the carry state was cleared at the
// link origin, not guessed at adoption.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-b"); });
await flush(); await flush();
assert(!location.search.includes("record=") && !location.search.includes("appview="), "a bare ctx link carries no stale record/view");
// Browser back restores the prior authored entry (ctx-a + New view).
history.back();
await flush(); await flush();
assert(location.search.includes("ctx=ctx-a") && location.search.includes("appview=new"), "browser back restores the authored New-view scope");

// Scoped entry: an unselected multi-context install offers one
// `?ctx=` link per active context — explicit, never a silent default.
// Clear the remembered preference so the multi-context install lands
// genuinely unscoped, like a fresh browser.
win.sessionStorage.removeItem("cadence.workspace-app.context.install-shell");
await React.act(async () => { navigate("/app-installations/install-shell"); });
await flush(); await flush();
assert(location.search === "" || !location.search.includes("ctx="), "no silent context is picked");
const scopeLinks = Array.from(host.querySelectorAll<HTMLAnchorElement>("[data-scope-link]"));
equal(scopeLinks.length, 2, "one scoped-entry link per active context");
assert(scopeLinks.every((a) => a.getAttribute("href")?.includes("ctx=")), "every entry is a real ?ctx= link");
const acmeEntry = scopeLinks.find((a) => a.getAttribute("data-scope-link") === "ctx-a");
assert(acmeEntry, "scoped entry exists for ctx-a");
await click(acmeEntry);
assert(location.search.includes("ctx=ctx-a"), "scoped entry lands its context in the URL");
// The links stay after a scope binds: a compact switch keeps every
// active context reachable without hand-editing the URL — the
// regression the removed header picker introduced. The bound context
// carries aria-current instead of disappearing.
const switchLinks = Array.from(host.querySelectorAll<HTMLAnchorElement>("[data-scope-link]"));
equal(switchLinks.length, 2, "context links stay reachable once a scope is bound");
assert(switchLinks.every((a) => a.getAttribute("href")?.includes("ctx=")), "switch entries stay real ?ctx= links");
const betaEntry = switchLinks.find((a) => a.getAttribute("data-scope-link") === "ctx-b");
assert(betaEntry, "the other context stays linkable once a scope is bound");
equal(
  host.querySelector('[data-scope-link="ctx-a"]')?.getAttribute("aria-current"),
  "page",
  "the bound context carries aria-current",
);
assert(
  !host.querySelector('[data-scope-link="ctx-b"]')?.hasAttribute("aria-current"),
  "unselected contexts carry no aria-current",
);
await click(betaEntry);
assert(location.search.includes("ctx=ctx-b"), "the switch rebinds the new scope in the URL");
assert(!location.search.includes("appview=") && !location.search.includes("record="), "the switch clears any stale record/view");
assert((host.textContent ?? "").includes("Beta · 0.1.0"), "the header label follows the switched context");

// A remembered context (sessionStorage) no longer traps the operator:
// the switch stays rendered and clicking through clears stale state.
win.sessionStorage.setItem("cadence.workspace-app.context.install-shell", "ctx-a");
await React.act(async () => { navigate("/app-installations/install-shell"); });
await flush(); await flush();
assert(location.search === "" || !location.search.includes("ctx="), "a remembered context needs no ?ctx in the URL");
assert((host.textContent ?? "").includes("Acme · 0.1.0"), "the remembered context binds without a URL param");
const rememberedLinks = Array.from(host.querySelectorAll<HTMLAnchorElement>("[data-scope-link]"));
equal(rememberedLinks.length, 2, "the switch still renders under a remembered context");
const rememberedBeta = rememberedLinks.find((a) => a.getAttribute("data-scope-link") === "ctx-b");
assert(rememberedBeta, "ctx-b stays linkable while ctx-a is remembered");
// Stale outlet state on the remembered scope: a carried record and a
// New view both die on the switch, matching the scoped-entry contract.
await React.act(async () => { navigate("/app-installations/install-shell?appview=new&record=rec-stale"); });
await flush(); await flush();
assert(host.querySelector("#app-outlet-draft"), "stale New view mounts for the remembered context");
await click(rememberedBeta);
assert(location.search.includes("ctx=ctx-b"), "switching under a remembered context binds the new scope");
assert(!location.search.includes("record=") && !location.search.includes("appview="), "switching clears the stale record and view");
assert((host.textContent ?? "").includes("Beta · 0.1.0"), "the switched context renders its own label");
win.sessionStorage.removeItem("cadence.workspace-app.context.install-shell");

// A context-bound direct link restores scope; browser back keeps it.
history.pushState(null, "", "/app-installations/install-shell?ctx=ctx-a&record=rec-9");
await React.act(async () => { win.dispatchEvent(new win.PopStateEvent("popstate")); });
await flush(); await flush();
assert(location.search.includes("record=rec-9"), "direct record link preserves scope");
assert(text().includes("Record details"), "record drawer opens from the link");
history.back();
await flush(); await flush();
assert(!location.search.includes("record="), "browser back drops the record scope");

// A stale linked context clears the selection with a notice.
history.pushState(null, "", "/app-installations/install-shell?ctx=ctx-gone&record=rec-9");
await React.act(async () => { win.dispatchEvent(new win.PopStateEvent("popstate")); });
await flush(); await flush();
assert(!location.search.includes("record="), "stale context link clears the record");
assert(text().includes("not active"), "stale context link explains itself");

// A record link without scope is ambiguous: refused with a notice.
history.pushState(null, "", "/app-installations/install-shell?record=rec-9");
await React.act(async () => { win.dispatchEvent(new win.PopStateEvent("popstate")); });
await flush(); await flush();
assert(!location.search.includes("record="), "scopeless record link clears the record");
assert(text().includes("names no context"), "scopeless record link explains itself");

// Installation switch: outlet query stripped, chat draft and stream kept.
history.pushState(null, "", "/app-installations/install-second?ctx=ctx-only&record=rec-1");
await React.act(async () => {
  root.render(React.createElement(AppShell, { installId: "install-second", viewer: { operator: true, readOnly: false } }));
});
await flush(); await flush(); await flush();
assert(!location.search.includes("record=") && !location.search.includes("ctx="), "install switch strips stale outlet query");
assert(text().includes("Second"), "new installation renders");
// A single-context install keeps the switch hidden — no link clutter.
assert(!host.querySelector("[data-scope-link]"), "single-context installs render no switch links");
equal(panes(), 1, "install switch keeps one chat pane");
equal(eventSources, 1, "install switch opens no second stream");
assert(text().includes("Scoped follow-up"), "install switch keeps the live thread");

// chatBinding: empty wants plain chat, inactive blocks early with the
// server as final proof, active or unknown binds for the daemon.
const chatBinding = AppShellModule.chatBinding;
equal(chatBinding({ installId: "i", wanted: "", known: true, activeIds: ["a"] }), { scope: null, error: null }, "empty scope sends plain chat");
equal(chatBinding({ installId: "i", wanted: "a", known: true, activeIds: ["a", "b"] }).scope, { install_id: "i", context_id: "a" }, "active scope binds");
equal(chatBinding({ installId: "i", wanted: "gone", known: true, activeIds: ["a"] }).scope, null, "stale scope binds nothing");
assert(chatBinding({ installId: "i", wanted: "gone", known: true, activeIds: ["a"] }).error?.includes("not active"), "stale scope explains itself");
equal(chatBinding({ installId: "i", wanted: "gone", known: false, activeIds: [] }).scope, { install_id: "i", context_id: "gone" }, "unknown receipts defer to the server");

// Social-content owns its context selector: the shell shows none and
// claims no context in the chat label. The existing mounted social
// flow (workspaceAppView suite) proves that selector still works.
// The stub below mimics WorkspaceApp's picker exactly (local state +
// rememberContext, no parent rerender): the shell must observe the
// switch in the same tab and send the new scope immediately.
const { rememberContext: rememberCtx } = require("../src/features/workspace-apps/contextSelection");
function StubPicker({ installId }: { installId: string }) {
  const [value, setValue] = React.useState("");
  return React.createElement("div", null,
    React.createElement("label", { htmlFor: "brand" }, "Optional brand context"),
    React.createElement("select", {
      id: "brand", value,
      onChange: (e: any) => { setValue(e.target.value); rememberCtx(installId, e.target.value); },
    },
      React.createElement("option", { value: "" }, "No brand context"),
      React.createElement("option", { value: "ctx-brand" }, "Brand"),
      React.createElement("option", { value: "ctx-beta" }, "Beta brand")));
}
// A stale remembered scope blocks the send before anything posts.
win.sessionStorage.setItem("cadence.workspace-app.context.install-social", "ctx-gone");
await React.act(async () => {
  root.render(React.createElement(AppShell, {
    installId: "install-social",
    viewer: { operator: true, readOnly: false },
    children: React.createElement(StubPicker, { installId: "install-social" }),
  }));
});
await flush(); await flush(); await flush();
assert(!host.querySelector('[aria-label="App context"]'), "no shell selector — social workspace owns its own");
assert(host.querySelector("#brand"), "the workspace keeps its own selector");
assert(text().includes("managed inside the workspace screen"), "shell display defers to the workspace owner");
posts.length = 0;
await fill("#app-shell-chat-box", "stale scope question");
await click(host.querySelector(".app-chat-form button[type=submit]"));
await flush();
assert(!posts.some((p) => p.path === "/api/threads/master/messages"), "stale social scope posts nothing");
assert(text().includes("not active"), "stale social scope explains itself");
// The workspace picker switches scope; the shell's immediate next
// send carries the new context ID with no intervening navigation.
const brand = host.querySelector("#brand") as HTMLSelectElement;
await React.act(async () => {
  const proto = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!;
  proto.set!.call(brand, "ctx-beta");
  brand.dispatchEvent(new Event("change", { bubbles: true }));
});
await flush();
posts.length = 0;
await fill("#app-shell-chat-box", "beta scope question");
await click(host.querySelector(".app-chat-form button[type=submit]"));
await flush();
const socialPost = posts.find((p) => p.path === "/api/threads/master/messages");
assert(socialPost, "social chat send posted");
equal(socialPost?.body.app, { install_id: "install-social", context_id: "ctx-beta" }, "send carries the just-picked scope, never the stale one");
// Switching installation clears the stale social scope: the next
// install sends its own scope, never the social one.
await React.act(async () => {
  root.render(React.createElement(AppShell, { installId: "install-second", viewer: { operator: true, readOnly: false } }));
});
await settle(() => assert(
  host.textContent?.includes("Only"),
  "second install adopts its sole active context",
));
posts.length = 0;
await fill("#app-shell-chat-box", "second install question");
await click(host.querySelector(".app-chat-form button[type=submit]"));
await flush();
const secondPost = posts.find((p) => p.path === "/api/threads/master/messages");
assert(secondPost, "second install send posted");
equal(secondPost?.body.app, { install_id: "install-second", context_id: "ctx-only" }, "install switch clears stale social scope");

// Cold deep links (CAD-863, browser blocker): a fresh mount of the
// real URL — no manual selection, no same-key navigation — must adopt
// the authored scope exactly once receipts land. Fresh mounts exercise
// the async detail→contexts→adopt path a first-render adoption never
// sees. The viewer.operator false→true transition is covered at the
// end of the block.
// The long-lived root above ends on install-second; unmount it so the
// cold mounts below are the only live shell and carry no install-
// switch or remembered-scope residue from the earlier flow.
await React.act(async () => { root.unmount(); });
win.sessionStorage.clear();
async function mountColdCold(url: string, install: string) {
  const hostEl = document.createElement("div");
  document.body.append(hostEl);
  const cold = createRoot(hostEl);
  history.pushState(null, "", url);
  await React.act(async () => {
    cold.render(React.createElement(AppShell, { installId: install, viewer: { operator: true, readOnly: false } }));
  });
  await settle(() => assert((hostEl.textContent ?? "").includes("Reports"), `cold mount renders: ${url}`));
  await flush(); await flush();
  return { hostEl, cold };
}

// Canonical link: every authored param survives and the scope binds.
{
  const { hostEl, cold } = await mountColdCold(
    "/app-installations/install-shell?ctx=ctx-a&crm=customers&record=rec-1",
    "install-shell",
  );
  // The adopted label "<label> · version" distinguishes adoption from
  // the un-adopted scope picker (which also prints context labels as
  // link text). Assert the bound label, not the picker's options.
  assert((hostEl.textContent ?? "").includes("Acme · 0.1.0"), "cold ctx+record link adopts the authored context");
  assert(!(hostEl.textContent ?? "").includes("No context ·"), "cold ctx+record link does not fall back to the scope picker");
  assert((hostEl.textContent ?? "").includes("Record details"), "cold ctx+record link opens the record");
  assert(location.search.includes("ctx=ctx-a") && location.search.includes("record=rec-1"), "cold link params survive adoption");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// Explicit default section: `crm=customers` is already canonical — it
// must not be rewritten, and the ctx must adopt alongside it.
{
  const { hostEl, cold } = await mountColdCold(
    "/app-installations/install-shell?ctx=ctx-a&crm=customers",
    "install-shell",
  );
  assert((hostEl.textContent ?? "").includes("Acme · 0.1.0"), "explicit crm=customers cold link adopts the context");
  assert(!(hostEl.textContent ?? "").includes("No context ·"), "explicit crm=customers does not fall back to the scope picker");
  assert(location.search.includes("ctx=ctx-a"), "explicit crm=customers keeps the ctx");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// Unknown section: only `crm` normalizes — the authored ctx and
// record still adopt, and no record from another scope can be
// displayed under this URL (the ctx-bound drawer keys the load).
{
  const { hostEl, cold } = await mountColdCold(
    "/app-installations/install-shell?ctx=ctx-a&crm=bogus&record=rec-2",
    "install-shell",
  );
  assert(!location.search.includes("crm="), "unknown crm section is stripped");
  assert(location.search.includes("ctx=ctx-a"), "unknown crm keeps the authored ctx");
  assert(location.search.includes("record=rec-2"), "unknown crm keeps the authored record");
  assert((hostEl.textContent ?? "").includes("Acme · 0.1.0"), "unknown crm still adopts the context");
  assert(!(hostEl.textContent ?? "").includes("No context ·"), "unknown crm does not fall back to the scope picker");
  assert((hostEl.textContent ?? "").includes("Record details"), "unknown crm keeps the ctx-bound record");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// Malformed record: only the record is stripped — the authored ctx
// adopts rather than dropping the whole link.
{
  const { hostEl, cold } = await mountColdCold(
    "/app-installations/install-shell?ctx=ctx-b&record=../escape",
    "install-shell",
  );
  assert(!location.search.includes("record="), "malformed record id is stripped");
  assert(location.search.includes("ctx=ctx-b"), "malformed record keeps the authored ctx");
  assert((hostEl.textContent ?? "").includes("Beta · 0.1.0"), "malformed record still adopts the context");
  assert(!(hostEl.textContent ?? "").includes("No context ·"), "malformed record does not fall back to the scope picker");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// Stale linked context on a cold mount still refuses cleanly.
{
  const { hostEl, cold } = await mountColdCold(
    "/app-installations/install-shell?ctx=ctx-gone&record=rec-9",
    "install-shell",
  );
  assert(!location.search.includes("ctx=") && !location.search.includes("record="), "stale cold link clears scope and record");
  assert((hostEl.textContent ?? "").includes("not active"), "stale cold link explains itself");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// The real cold-open path (browser finding): the viewer proves
// operator asynchronously, so receipts land on the false→true
// transition, not the first render. Adoption must follow.
{
  const hostEl = document.createElement("div");
  document.body.append(hostEl);
  const cold = createRoot(hostEl);
  history.pushState(null, "", "/app-installations/install-shell?ctx=ctx-a&record=rec-3");
  await React.act(async () => {
    cold.render(React.createElement(AppShell, { installId: "install-shell", viewer: { operator: false, readOnly: false } }));
  });
  await flush();
  assert((hostEl.textContent ?? "").includes("Sign in as the operator"), "unproven viewer sees the sign-in note, never records");
  await React.act(async () => {
    cold.render(React.createElement(AppShell, { installId: "install-shell", viewer: { operator: true, readOnly: false } }));
  });
  await settle(() => assert((hostEl.textContent ?? "").includes("Record details"), "operator false→true cold link opens the record"));
  assert((hostEl.textContent ?? "").includes("Acme · 0.1.0"), "operator false→true cold link adopts the context");
  assert(!(hostEl.textContent ?? "").includes("No context ·"), "operator transition does not fall back to the scope picker");
  assert(location.search.includes("ctx=ctx-a") && location.search.includes("record=rec-3"), "operator transition keeps the authored link");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

console.log("app shell checks passed");
}
void main();
