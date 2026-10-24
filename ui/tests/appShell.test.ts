import { streamFixture } from "./streamFixture";
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

// Mounted shell: one chat pane, one active context owner, verified send
// binding, and (CAD-1519) the one generic unavailable state where an app
// ships no screen of its own.
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-shell" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"])
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
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const AppShellModule = (require("../src/features/app-shell/AppShell") as typeof import("../src/features/app-shell/AppShell"));
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
const AppShellComponent = AppShellModule.default;
const AppShell = (props: React.ComponentProps<typeof AppShellComponent>) =>
  React.createElement(LocaleProvider, null,
    React.createElement(NavigationProvider, { adapter: hostNavigation, children: React.createElement(AppShellComponent, props) }));
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
// A Social install with no contexts at all: the screen mounts, but every
// social source action needs a mounted active context (CAD-1702).
const socialZero = { ...generic, install_id: "install-social-zero", title: "Social Content", name: "social-content" };
const socialZeroContexts: Array<Record<string, unknown>> = [];
const second = { ...generic, install_id: "install-second", title: "Second", name: "reports" };
const crm = { ...generic, install_id: "install-crm", title: "CRM", name: "crm" };
// A CRM install with two active scopes: its scopeless record link is
// ambiguous and must be refused (CAD-1009).
const crmMulti = { ...generic, install_id: "install-crm-multi", title: "CRM Multi", name: "crm" };
const contextsFor: Record<string, unknown> = {
  "install-shell": { contexts: [
    { id: "ctx-a", install_id: "install-shell", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
  ] },
  "install-second": { contexts: [
    { id: "ctx-only", install_id: "install-second", revision: 1, state: "active", digest: "co", config: { schema: 1, label: "Only", input_defaults: {} } },
  ] },
  "install-social": { contexts: [
    { id: "ctx-brand", install_id: "install-social", revision: 1, state: "active", digest: "cs", config: { schema: 1, label: "Brand", input_defaults: {} } },
  ] },
  "install-crm": { contexts: [
    { id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
  ] },
  "install-crm-multi": { contexts: [
    { id: "ctx-a", install_id: "install-crm-multi", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
    { id: "ctx-b", install_id: "install-crm-multi", revision: 1, state: "active", digest: "cb", config: { schema: 1, label: "Beta", input_defaults: {} } },
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
const reads: string[] = [];
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const path = String(input);
  reads.push(path);
  if (new URL(path, "http://localhost").pathname.endsWith("/stream")) {
    return streamFixture(path);
  }
  if (init?.method === "POST") {
    const body = JSON.parse(String(init.body));
    posts.push({ path, body });
    if (path === "/api/app-installations/install-social-zero/contexts") {
      const row = {
        id: "ctx-general",
        install_id: "install-social-zero",
        revision: 1,
        state: "active",
        digest: "cg",
        config: { schema: 1, label: body.label, input_defaults: {} },
      };
      socialZeroContexts.push(row);
      return json({ context: row });
    }
    return json({ ok: true });
  }
  if (path === "/api/agents") return json({
    daemon: "ok",
    agents: [{
      alias: "master",
      provider: "pi",
      endpoint_kind: "managed",
      state: "running",
      params: { look: "builder" },
      look: "builder",
      group: "rollout",
      running: 0,
      queued: 0,
      unknown: 0,
      parked: 0,
    }],
    by_issue: {},
  });
  if (path === "/api/app-installations/install-shell") return json(generic);
  if (path === "/api/app-installations/install-second") return json(second);
  if (path === "/api/app-installations/install-social") return json(social);
  if (path === "/api/app-installations/install-social-zero") return json(socialZero);
  if (path === "/api/app-installations/install-social-zero/contexts") return json({ contexts: socialZeroContexts });
  if (path === "/api/app-installations/install-shell/contexts") return json(contextsFor["install-shell"]);
  if (path === "/api/app-installations/install-second/contexts") return json(contextsFor["install-second"]);
  if (path === "/api/app-installations/install-social/contexts") return json(contextsFor["install-social"]);
  if (path === "/api/app-installations/install-crm") return json(crm);
  if (path === "/api/app-installations/install-crm/contexts") return json(contextsFor["install-crm"]);
  if (path === "/api/app-installations/install-crm-multi") return json(crmMulti);
  if (path === "/api/app-installations/install-crm-multi/contexts") return json(contextsFor["install-crm-multi"]);
  // Daemon without conversations: the chat keeps the home thread (legacy).
  if (path.endsWith("/conversations")) return new Response("{}", { status: 404 });
  if (path.startsWith("/api/threads/master")) return json(thread);
  // CRM section reads: empty server receipts only — the mounted
  // dedupe assertions below exercise real list/new/detail paths, so
  // every section must answer its own read with the host's shape.
  const url = new URL(path, "http://localhost");
  if (url.pathname === "/api/crm-send/list") return json({ sends: [] });
  if (url.pathname === "/api/crm-send/origin") return json({ unsubscribe_origin: null, stored: false });
  if (url.pathname.endsWith("/records")) return json({ records: [], truncated: false, next_cursor: null });
  if (url.pathname.endsWith("/segments/list")) return json({ segments: [] });
  if (url.pathname.endsWith("/exclusions/list")) return json({ exclusions: [] });
  if (url.pathname.endsWith("/suppressions/list")) return json({ suppressions: [] });
  if (url.pathname.endsWith("/content/campaigns/list")) return json({ contents: [] });
  if (url.pathname.endsWith("/content/proposals/list")) return json({ proposals: [] });
  if (url.pathname.endsWith("/sender-bindings/list")) return json({ bindings: [] });
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

// The shell's adopted scope is visible on the wire: assistant reads carry it.
const readScopes = () => reads
  .filter((p) => p.includes("/assistant/"))
  .map((p) => new URL(p, "http://localhost").searchParams.get("context_id"));

// A new browser: no stored preference, no scope in the link.
win.sessionStorage.clear();
history.pushState(null, "", "/app-installations/install-shell");
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null, React.createElement(AppShell, { installId: "install-shell", viewer: { operator: true, readOnly: false } })));
});
await flush(); await flush(); await flush();
assert(text().includes("All apps") && text().includes("Reports"), "← All apps back-link plus installed App title, deduped");
assert(text().includes("This app's screen is unavailable."), "an app without its own screen shows the one unavailable state");
assert(!text().includes("No records yet") && !text().includes("CAD-781"), "core draws no records UI of its own");
assert(text().includes("Master answer stays connected"), "the left pane shows the real master thread");
assert(host.querySelector('.app-chat-title .agent-avatar[data-slug="builder"]'), "the chat header shows the master's configured avatar look");
assert(!host.querySelector(".app-chat-mark"), "the static star mark is gone");
assert(!text().includes("ctx-a"), "CAD-1051: internal context ids never render in the chat");
equal(panes(), 1, "exactly one chat pane in the document");
equal(boxes(), 1, "exactly one chat draft box id in the document");
assert(!host.querySelector('[aria-label="App context"]'), "context selector removed — context follows URL only");

// Narrow switch: one compact top-right toggle in the shell crumb replaces
// the full-width segmented row. The assistant is shown by default; both
// panels stay mounted so the draft survives switching.
const panelToggle = host.querySelector<HTMLButtonElement>(".app-shell-panel-toggle");
assert(panelToggle, "the narrow top-right pane toggle exists");
assert(!host.querySelector(".app-shell-panels"), "the full-width Assistant | Workspace row is gone");
assert(!host.querySelector(".app-shell-chat-toggle") && !host.querySelector(".app-shell-scrim"), "no overlay drawer toggle or scrim remains");
const grid = host.querySelector(".app-shell-grid") as HTMLElement;
equal(grid.getAttribute("data-shell-panel"), "chat", "the assistant is the default narrow panel");
equal(panelToggle.getAttribute("aria-pressed"), "false", "the toggle is not pressed for the assistant");
equal(panelToggle.getAttribute("aria-label"), "Open workspace", "the toggle names its next action");
await fill("#app-shell-chat-box", "unsent shell draft");
await click(panelToggle);
equal(grid.getAttribute("data-shell-panel"), "work", "the toggle shows the workspace");
equal(panelToggle.getAttribute("aria-pressed"), "true", "the workspace toggle is pressed");
equal(panelToggle.getAttribute("aria-label"), "Back to assistant", "the toggle flips its accessible name");
await click(panelToggle);
equal(grid.getAttribute("data-shell-panel"), "chat", "the toggle returns to the assistant");
equal((host.querySelector("#app-shell-chat-box") as HTMLTextAreaElement)?.value, "unsent shell draft", "one pane keeps one draft across switching");
equal(panes(), 1, "switching never mounts a second chat pane");

// Chat sends carry the verified-scope request; refusal would surface.
// Context is adopted from the URL — no selector needed.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-a"); });
await flush();
assert(!location.search.includes("ctx="), "a legacy ctx link is normalized to a scope-free URL");
// D1: an empty selector is distinct from an absent one and is not
// swallowed by the handled-query guard.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=&record=rec-1"); });
await flush(); await flush();
assert(!location.search.includes("ctx="), "an empty selector is normalized away");
assert(text().includes("not active"), "an empty selector is treated as inactive");
posts.length = 0;
await fill("#app-shell-chat-box", "scoped question");
await click(host.querySelector(".app-chat-form button[type=submit]"));
await flush();
const chatPost = posts.find((p) => p.path === "/api/threads/master/messages");
assert(chatPost, "chat send posted");
equal(chatPost?.body.app, { install_id: "install-shell", context_id: "ctx-a" }, "send binds the selected scope, never actor claims");
assert(!("actor" in (chatPost?.body ?? {})) && !("verified" in (chatPost?.body ?? {})), "send carries no authority claim");

// Outlet navigation: a New view in the URL; a stale context link returns the
// outlet to the list and clears the selection.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-a&appview=new"); });
await flush();
assert(location.search.includes("appview=new"), "New view lands in the URL for direct links");
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-gone&appview=new&record=rec-1"); });
await flush(); await flush();
assert(!location.search.includes("appview="), "a stale context link returns the outlet to the list");
assert(!location.search.includes("record="), "a stale context link clears the stale selection");
assert(text().includes("not active"), "a stale context link explains itself");

// Authored ctx+record links keep their params verbatim: a deep link's
// record is never stripped at adoption.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-a&record=rec-1"); });
await flush(); await flush();
assert(location.search.includes("record=rec-1"), "deep link lands its record");
// An authored New-view link keeps its view too.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-a&appview=new"); });
await flush(); await flush();
assert(location.search.includes("appview=new") && !location.search.includes("ctx="), "cross-context New view link keeps the view and loses the selector");
// A bare ctx link carries no stale record/view from the prior entry.
await React.act(async () => { navigate("/app-installations/install-shell?ctx=ctx-a"); });
await flush(); await flush();
assert(!location.search.includes("record=") && !location.search.includes("appview="), "a bare ctx link carries no stale record/view");
// Browser back restores the prior authored entry (ctx-a + New view).
history.back();
await flush(); await flush();
assert(location.search.includes("appview=new") && !location.search.includes("ctx="), "browser back restores the authored New view without a selector");

// A context-bound direct link restores scope; browser back keeps it.
history.pushState(null, "", "/app-installations/install-shell?ctx=ctx-a&record=rec-9");
await React.act(async () => { win.dispatchEvent(new win.PopStateEvent("popstate")); });
await flush(); await flush();
assert(location.search.includes("record=rec-9"), "direct record link preserves scope");
history.back();
await flush(); await flush();
assert(!location.search.includes("record="), "browser back drops the record scope");

// A stale linked context clears the selection with a notice.
history.pushState(null, "", "/app-installations/install-shell?ctx=ctx-gone&record=rec-9");
await React.act(async () => { win.dispatchEvent(new win.PopStateEvent("popstate")); });
await flush(); await flush();
assert(!location.search.includes("record="), "stale context link clears the record");
assert(text().includes("not active"), "stale context link explains itself");

// Installation switch: outlet query stripped, chat history stays visible.
history.pushState(null, "", "/app-installations/install-second?ctx=ctx-only&record=rec-1");
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null, React.createElement(AppShell, { installId: "install-second", viewer: { operator: true, readOnly: false } })));
});
await flush(); await flush(); await flush();
assert(!location.search.includes("record=") && !location.search.includes("ctx="), "install switch strips stale outlet query");
assert(text().includes("Second"), "new installation renders");
equal(panes(), 1, "install switch keeps one chat pane");
assert(text().includes("Scoped follow-up"), "install switch keeps the live thread");

// chatBinding: empty wants plain chat, inactive blocks early with the
// server as final proof, active or unknown binds for the daemon.
const chatBinding = AppShellModule.chatBinding;
equal(chatBinding({ installId: "i", wanted: "", known: true, activeIds: ["a"] }), { scope: null, error: null }, "empty scope sends plain chat");
equal(chatBinding({ installId: "i", wanted: "a", known: true, activeIds: ["a", "b"] }).scope, { install_id: "i", context_id: "a" }, "active scope binds");
equal(chatBinding({ installId: "i", wanted: "gone", known: true, activeIds: ["a"] }).scope, null, "stale scope binds nothing");
assert(chatBinding({ installId: "i", wanted: "gone", known: true, activeIds: ["a"] }).error?.includes("not active"), "stale scope explains itself");
equal(chatBinding({ installId: "i", wanted: "gone", known: false, activeIds: [] }).scope, { install_id: "i", context_id: "gone" }, "unknown receipts defer to the server");

// Social-content uses the same shell-owned scope as every other app: its
// one active context. The shell reads no remembered choice, so a stale
// stored id can neither block nor redirect a send.
win.sessionStorage.setItem("cadence.workspace-app.context.install-social", "ctx-gone");
reads.length = 0;
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null, React.createElement(AppShell, { installId: "install-social", viewer: { operator: true, readOnly: false } })));
});
await settle(() => assert(readScopes().includes("ctx-brand"), "social adopts its one active context"));
// The Social private screen renders its own chrome — the shell must NOT
// emit the technical `installId · version · context` subtitle.
assert(
  !text().includes("managed inside the workspace screen") &&
    !text().includes("install-social · 0.1.0"),
  "shell emitted the removed Social technical subtitle",
);
assert(!host.querySelector("select[data-social-context]"), "no in-shell context picker remains");
posts.length = 0;
await fill("#app-shell-chat-box", "social scope question");
await click(host.querySelector(".app-chat-form button[type=submit]"));
await flush();
const socialPost = posts.find((p) => p.path === "/api/threads/master/messages");
assert(socialPost, "social chat send posted");
equal(socialPost?.body.app, { install_id: "install-social", context_id: "ctx-brand" }, "send binds the one active context, never a remembered id");
// CAD-1702: a zero-context Social install gets its General context on open;
// the mount and the send use it instead of an empty scope. Without the fix
// no create is posted and the send binds no context.
socialZeroContexts.length = 0;
posts.length = 0;
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null, React.createElement(AppShell, { installId: "install-social-zero", viewer: { operator: true, readOnly: false } })));
});
await settle(() => assert(socialZeroContexts.length === 1, "zero-context Social creates its General context"));
const zeroCreate = posts.find((p) => p.path === "/api/app-installations/install-social-zero/contexts");
assert(zeroCreate, "General context create posted");
equal(zeroCreate?.body.label, "General", "the created context is General");
await settle(() => assert(readScopes().includes("ctx-general"), "the created context becomes the scope"));
posts.length = 0;
await fill("#app-shell-chat-box", "zero context social");
await click(host.querySelector(".app-chat-form button[type=submit]"));
await flush();
const zeroPost = posts.find((p) => p.path === "/api/threads/master/messages");
assert(zeroPost, "zero-context social send posted");
equal(zeroPost?.body.app, { install_id: "install-social-zero", context_id: "ctx-general" }, "send binds the created context, not an empty scope");
// Installation switch: the next install sends its own sole context.
reads.length = 0;
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null, React.createElement(AppShell, { installId: "install-second", viewer: { operator: true, readOnly: false } })));
});
await settle(() => assert(readScopes().includes("ctx-only"), "second install adopts its sole active context"));
posts.length = 0;
await fill("#app-shell-chat-box", "second install question");
await click(host.querySelector(".app-chat-form button[type=submit]"));
await flush();
const secondPost = posts.find((p) => p.path === "/api/threads/master/messages");
assert(secondPost, "second install send posted");
equal(secondPost?.body.app, { install_id: "install-second", context_id: "ctx-only" }, "install switch clears the social scope");

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
async function mountColdCold(url: string, install: string, scope: string) {
  const hostEl = document.createElement("div");
  document.body.append(hostEl);
  const cold = createRoot(hostEl);
  history.pushState(null, "", url);
  reads.length = 0;
  await React.act(async () => {
    cold.render(React.createElement(AppShell, { installId: install, viewer: { operator: true, readOnly: false } }));
  });
  await settle(() => {
    assert((hostEl.textContent ?? "").includes("Reports"), `cold mount renders: ${url}`);
    assert(readScopes().includes(scope), `cold mount adopts ${scope}: ${url}`);
  });
  await flush(); await flush();
  return { hostEl, cold };
}

// Canonical link: every authored param survives and the scope binds.
{
  const { hostEl, cold } = await mountColdCold(
    "/app-installations/install-shell?ctx=ctx-a&crm=customers&record=rec-1",
    "install-shell",
    "ctx-a",
  );
  assert(location.search.includes("record=rec-1") && !location.search.includes("ctx="), "cold link keeps its record and loses the selector");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// An app-owned section parameter is the app's: the shell neither strips
// nor rewrites it, and the authored ctx and record still adopt.
{
  const { hostEl, cold } = await mountColdCold(
    "/app-installations/install-shell?ctx=ctx-a&crm=bogus&record=rec-2",
    "install-shell",
    "ctx-a",
  );
  assert(location.search.includes("crm=bogus"), "an app-owned section parameter is left to the app");
  assert(!location.search.includes("ctx="), "the authored ctx is normalized away");
  assert(location.search.includes("record=rec-2"), "the authored record is kept");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// Malformed record: only the record is stripped — the authored ctx
// adopts rather than dropping the whole link.
{
  const { hostEl, cold } = await mountColdCold(
  "/app-installations/install-shell?ctx=ctx-a&record=../escape",
    "install-shell",
    "ctx-a",
  );
  assert(!location.search.includes("record="), "malformed record id is stripped");
  assert(readScopes().includes("ctx-a") && !location.search.includes("ctx="), "malformed record keeps the scope on the wire, not in the URL");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// Stale linked context on a cold mount still refuses cleanly.
{
  const { hostEl, cold } = await mountColdCold(
    "/app-installations/install-shell?ctx=ctx-gone&record=rec-9",
    "install-shell",
    "ctx-a",
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
  reads.length = 0;
  await React.act(async () => {
    cold.render(React.createElement(AppShell, { installId: "install-shell", viewer: { operator: true, readOnly: false } }));
  });
  await settle(() => assert(readScopes().includes("ctx-a"), "operator false→true cold link adopts the context"));
  assert(location.search.includes("record=rec-3") && !location.search.includes("ctx="), "operator transition keeps the record without the selector");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// CAD-1519: core bundles no app UI. A CRM install whose package ships no
// screen gets the same one unavailable state as any other app — no
// host-drawn Customers/Segments/Campaigns pages, and one Apps link only.
{
  win.sessionStorage.clear();
  const hostEl = document.createElement("div");
  document.body.append(hostEl);
  const cold = createRoot(hostEl);
  history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a&crm=segments");
  await React.act(async () => {
    cold.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
  });
  await settle(() => assert((hostEl.textContent ?? "").includes("This app's screen is unavailable."), "CRM without its own screen shows the unavailable state"));
  assert(!hostEl.querySelector("[data-outlet-heading]"), "no host-drawn CRM section pages");
  assert(!(hostEl.textContent ?? "").match(/No customers yet|No segments yet|No campaigns yet/), "no host-drawn CRM lists");
  equal(Array.from(hostEl.querySelectorAll<HTMLAnchorElement>("a[href='/apps']")).length, 1, "exactly one Apps link — the shell row only");
  await React.act(async () => { cold.unmount(); });
  hostEl.remove();
}

// CAD-1009: an app writes `?record=cust-…` after Create/Open. A reload of
// that scopeless link resolves to the only active context and keeps the
// record; a genuinely ambiguous (multi-context) link is still refused, as
// a compact notice.
{
  win.sessionStorage.clear();
  const sole = document.createElement("div");
  document.body.append(sole);
  const soleRoot = createRoot(sole);
  history.pushState(null, "", "/app-installations/install-crm?crm=customers&record=cust-0123456789ab");
  await React.act(async () => {
    soleRoot.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } }));
  });
  await settle(() => assert(readScopes().includes("ctx-a"), "a scopeless record link resolves to the sole active context on the wire"));
  assert(location.search.includes("record=cust-0123456789ab"), "the record survives resolution");
  assert(!location.search.includes("ctx="), "the URL never carries a context after resolution");
  assert(!(sole.textContent ?? "").includes("names no context"), "single-context mode shows no refusal");
  assert(!sole.querySelector("[data-link-notice]"), "single-context mode shows no notice at all");
  await React.act(async () => { soleRoot.unmount(); });
  sole.remove();

  win.sessionStorage.clear();
  const ambiguous = document.createElement("div");
  document.body.append(ambiguous);
  const ambiguousRoot = createRoot(ambiguous);
  history.pushState(null, "", "/app-installations/install-crm-multi?crm=customers&record=cust-0123456789ab");
  await React.act(async () => {
    ambiguousRoot.render(React.createElement(AppShell, { installId: "install-crm-multi", viewer: { operator: true, readOnly: false } }));
  });
  await settle(() => assert((ambiguous.textContent ?? "").includes("names no context"), "a multi-context scopeless link is still refused"));
  assert(!location.search.includes("record="), "the refused record is stripped");
  const notice = ambiguous.querySelector("[data-link-notice]");
  assert(notice, "the refusal renders as a link notice");
  assert(!notice!.className.includes("card") && notice!.className.includes("py-1.5"), "the refusal is a compact notice, not a card");
  await React.act(async () => { ambiguousRoot.unmount(); });
  ambiguous.remove();
}

// The assistant activity panel never asks before it has a context (an empty
// context ID is a 400 on every poll).
assert(reads.some((p) => p.includes("/assistant/")), "the assistant panel did read with a context");
assert(!reads.some((p) => /\/assistant\/[a-z]+\?context_id=(&|$)/.test(p)), "no assistant read is sent with an empty context");
console.log("app shell checks passed");
}
void main();
