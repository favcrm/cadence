import {
  FORBIDDEN_BODY_KEYS,
  hostActions,
} from "../src/features/app-shell/hostActions";
import { ApiError } from "../src/lib/api";

export {};
/** CAD-802 trusted shared shell: host-action grammar + mounted shell. */
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
// URL-identity keys are tried against the grammar that must NOT carry
// them (`record_id` belongs only to create; install/context never do).
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

// No new guard ships here: authority stays server-checked by the
// CAD-768 HTTP peer (URL-bound identity, strict body grammar, operator
// admission). This client only shapes honest requests; the forged-field
// proof lives in CAD-768's HTTP parity tests.

// Mounted shell: breadcrumb, verified context, generic outlet with
// list/new/record routes, live master pane, narrow drawer.
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-shell" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
// The SSE stream never opens in node: the pane reads the REST thread.
Object.defineProperty(globalThis, "EventSource", {
  configurable: true,
  value: class {
    onopen: null = null;
    onerror: null = null;
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
const AppShell = (require("../src/features/app-shell/AppShell") as typeof import("../src/features/app-shell/AppShell")).default;

const installation = {
  install_id: "install-shell", title: "CRM", name: "crm", version: "0.1.0",
  digest: "install-digest", catalog_generation: "gen-1", approved: true,
  storage_kind: "workspace", files: [], capabilities: null, connection_slots: [],
};
const contexts = {
  contexts: [
    { id: "ctx-a", install_id: "install-shell", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } },
    { id: "ctx-b", install_id: "install-shell", revision: 1, state: "active", digest: "cb", config: { schema: 1, label: "Beta", input_defaults: {} } },
  ],
};
const thread = {
  entries: [
    { seq: 1, kind: "message", message: "m1", text: "Operator question", created: "2026-09-29T07:00:00Z" },
    { seq: 2, kind: "assistant_text", message: "m2", text: "Master answer stays connected", created: "2026-09-29T07:01:00Z" },
  ],
  more_before: false,
};
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const path = String(input);
  if (init?.method === "POST") return json({ ok: true });
  if (path === "/api/app-installations/install-shell") return json(installation);
  if (path === "/api/app-installations/install-shell/contexts") return json(contexts);
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
await React.act(async () => {
  root.render(React.createElement(AppShell, { installId: "install-shell", viewer: { operator: true, readOnly: false } }));
});
await flush(); await flush();
assert(text().includes("Apps") && text().includes("CRM"), "nested Apps → installed App breadcrumb");
assert(text().includes("No records yet"), "generic outlet is truthfully empty, never fake rows");
assert(text().includes("Master answer stays connected"), "the left pane shows the real master thread");
assert(host.querySelector('[aria-label="App context"]'), "verified context selector is present");

const toggle = host.querySelector(".app-shell-chat-toggle") as HTMLButtonElement;
assert(toggle && toggle.getAttribute("aria-controls") === "app-shell-chat-drawer", "narrow drawer toggle controls the chat drawer");
equal(toggle.getAttribute("aria-expanded"), "false", "drawer starts closed");
await click(toggle);
equal(toggle.getAttribute("aria-expanded"), "true", "drawer opens with accessible state");
assert(host.querySelector("#app-shell-chat-drawer")?.querySelector("[data-chat-pane]"), "drawer carries the same live chat pane");
await React.act(async () => {
  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
});
await flush();
equal(toggle.getAttribute("aria-expanded"), "false", "Escape closes the drawer");

const newTab = Array.from(host.querySelectorAll(".app-outlet-tab")).find((el) => el.textContent === "New");
await click(newTab);
assert(location.search.includes("appview=new"), "New view lands in the URL for direct links");
assert(text().includes("coming soon"), "New view is honest about CAD-781 owning the form");
const listTab = Array.from(host.querySelectorAll(".app-outlet-tab")).find((el) => el.textContent === "List");
await click(listTab);
assert(!location.search.includes("appview="), "list is the canonical view without a query marker");

// Switching context clears the selected record from the URL.
history.pushState(null, "", "/app-installations/install-shell?record=rec-9");
await React.act(async () => { win.dispatchEvent(new win.PopStateEvent("popstate")); });
await flush();
assert(location.search.includes("record=rec-9"), "direct record link preserves scope");
const ctxButton = host.querySelector('[aria-label="App context"]') as HTMLButtonElement;
await click(ctxButton);
const beta = Array.from(document.querySelectorAll('[role="option"]')).find((el) => el.textContent?.includes("Beta"));
await click(beta);
await flush();
assert(!location.search.includes("record="), "context switch clears the stale selection");
assert(!location.search.includes("appview="), "context switch returns the outlet to the list");

await React.act(async () => { root.unmount(); });
console.log("app shell checks passed");
}
void main();
