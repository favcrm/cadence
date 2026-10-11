/**
 * CAD-1108 acceptance check (ticket author's, CAD-1110 comment): a directive
 * naming an undeclared action, or carrying a forged identity, renders as text
 * and issues no host request. Name:
 * directive_naming_an_undeclared_action_or_forged_identity_renders_text_and_issues_no_host_request
 */
declare function require(name: string): any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(a: unknown, e: unknown, why: string) {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-notes" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "requestAnimationFrame", "cancelAnimationFrame"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
Object.defineProperty(globalThis, "EventSource", {
  configurable: true,
  value: class { onopen = null; onerror = null; addEventListener() {} removeEventListener() {} close() {} },
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
const Conversation = require("../src/features/app-shell/chat/Conversation").default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { loadAppChat } = require("../src/features/app-shell/chat/descriptorClient") as typeof import("../src/features/app-shell/chat/descriptorClient");

const INSTALL = "install-notes";
const DIGEST = "sha256:approved";
// The approved package's app-chat.json: kind A is a text card with one
// open-view button, kind B renders the app's own screen.
const descriptor = {
  contract: "app-chat/v1",
  app: "notes",
  directives: [
    {
      match: "cadence_note",
      card: {
        title: "Note",
        fields: [{ label: "Title", from: "title" }],
        buttons: [{ label: "Open the list", run: "open-view", view: "customers" }],
      },
    },
    { match: "cadence_post_preview", render: "screen:post-preview" },
  ],
};
const messages = [
  JSON.stringify({ cadence_note: { title: "Hi" } }),
  JSON.stringify({ cadence_note: { title: "Hi", action: "record-update", run: "open-view" } }),
  JSON.stringify({ cadence_post_preview: { text: "Hi", install_id: "install-other" } }),
];
const entries = messages.map((text, i) => ({
  seq: i + 1, role: "agent", kind: "turn_result", message: `m${i + 1}`, text, created: "2026-10-03T00:00:00Z", payload: { source: "master" },
}));

const fetched: { url: string; method: string }[] = [];
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  fetched.push({ url: url.pathname, method: init?.method ?? "GET" });
  // The generic descriptor route, serving the approved package's file.
  if (url.pathname === `/api/app-installations/${INSTALL}/chat-descriptor`) return json({ descriptor, digest: DIGEST, app: "notes" });
  if (url.pathname === `/api/app-installations/${INSTALL}/conversations`) {
    return json({ conversations: [{ id: "c-gen", title: null, subject: null, is_general: true }] });
  }
  if (url.pathname === "/api/threads/master") return json({ entries, more_before: false });
  if (url.pathname === "/api/master/state") return json({ alias: "master", provider: "p", endpoint_kind: "k", live: true, turn: null });
  throw new Error(`Unexpected request ${url.pathname}`);
}) as typeof fetch;

// Navigations: the shell callback, and any history write.
let navigations = 0;
const opened: string[] = [];
for (const m of ["pushState", "replaceState"] as const) {
  const original = history[m].bind(history);
  history[m] = ((...args: Parameters<History["pushState"]>) => { navigations++; return original(...args); }) as History["pushState"];
}

const chat = await loadAppChat(INSTALL, DIGEST, "notes");
assert(chat !== null, "the served descriptor validates against the installed app");

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null, React.createElement(Conversation, {
    mode: { kind: "app", installId: INSTALL, contextId: "", screen: null, recordOpen: false, contextName: null, descriptor: chat },
    density: "compact",
    viewer: { operator: true, readOnly: false },
    binding: { scope: null, error: null },
    collapsed: false,
    onCollapsed: () => undefined,
    onOpenView: (view: string) => { opened.push(view); },
  })));
});
for (let i = 0; i < 8; i++) await flush();

const rows = Array.from(host.querySelectorAll("[data-chat-item]")) as HTMLElement[];
equal(rows.length, 3, "three agent messages are listed");
const buttons = (row: HTMLElement) => Array.from(row.querySelectorAll("button, a, [role=button]"));

// 1: the control renders its one declared button.
equal(buttons(rows[0]).length, 1, "message 1 renders exactly one button");
assert((rows[0].textContent ?? "").includes("Hi") && rows[0].querySelector('[data-chat-card="directive"]'), "message 1 is the declared card with its title text");
equal(rows[0].querySelector("[data-chat-card-button]")?.textContent, "Open the list", "the declared button");

// 2 and 3: plain text, no button, no iframe.
for (const [i, text] of [[1, messages[1]], [2, messages[2]]] as const) {
  assert((rows[i].textContent ?? "").includes(text.slice(1, 20)), `message ${i + 1} reads as its own text`);
  equal(buttons(rows[i]).length, 0, `message ${i + 1} has no button`);
  equal(rows[i].querySelectorAll("iframe").length, 0, `message ${i + 1} has no iframe`);
  assert(!rows[i].querySelector('[data-chat-card="directive"]') && !rows[i].querySelector("[data-chat-frame]"), `message ${i + 1} is no card and no frame`);
}
// Click everything clickable in messages 2 and 3.
for (const i of [1, 2]) {
  for (const el of Array.from(rows[i].querySelectorAll("*"))) {
    await React.act(async () => { el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })); });
  }
}
await flush();
const risky = fetched.filter((f) => f.method !== "GET" || f.url.includes("/screens/") || f.url.endsWith("/mount"));
equal(risky, [], "zero host-action or screen-mount requests over the render and every click");
equal(opened, [], "zero navigations from messages 2 and 3");
equal(navigations, 0, "no history write");
assert(host.querySelector("#app-shell-chat-box"), "the composer is intact");

// The control is not vacuous: its declared button navigates, once, to the declared view.
await React.act(async () => { rows[0].querySelector("button")!.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
equal(opened, ["customers"], "the declared button opens its declared view");
await React.act(async () => { root.unmount(); });
console.log("app chat directive acceptance passed");
}
void main();
