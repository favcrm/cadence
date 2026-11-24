export {};
/**
 * CAD-2755: a staged custom MCP call is decided in the chat card: Approve,
 * Always allow (approve, then the tool's mode -> read) and Reject. Other kinds
 * keep the Review card. Checked in English and zh-TW.
 */
declare function require(name: string): any;
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLSelectElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
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
const ConversationDecisions = (require("../src/features/app-shell/chat/ConversationDecisions") as typeof import("../src/features/app-shell/chat/ConversationDecisions")).default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");

const mcpRow = (id: string) => ({
  id, tool: "custom_mcp_effect", status: "pending", createdAt: Date.now() - 60_000, decidedAt: null,
  input: {
    connection_id: "conn_1", connection_name: "Acme Billing", endpoint_host: "mcp.acme.example",
    tool: "create_invoice", snapshot_revision: 3, args: JSON.stringify({ customer: "Marlow Books" }),
  },
  result: null,
});
const appRow = (id: string) => ({
  id, tool: "app_call", status: "pending", createdAt: Date.now() - 60_000, decidedAt: null,
  title: "Update Marlow Books", app: "CRM", target: null, changes: [], unchanged_count: 0, changes_complete: true,
  reason: null, consent: null, effects: [], args: "{}", origin: null, result: null,
});

let rows: Array<{ id: string; status: string; [k: string]: unknown }> = [];
const calls: string[] = [];
let toolsStatus = 200;
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(input), "http://localhost");
  if (url.pathname === "/api/assist/actions") return json({ actions: rows });
  const m = /^\/api\/assist\/actions\/([^/]+)\/(approve|deny)$/.exec(url.pathname);
  if (m && init?.method === "POST") {
    calls.push(`${m[2]}:${m[1]}`);
    const row = rows.find((r) => r.id === m[1])!;
    row.status = m[2] === "approve" ? "approved" : "denied";
    if (m[2] === "approve") row.result = { ok: true };
    return json({ ok: true });
  }
  if (url.pathname === "/api/connections/mcp/conn_1/tools" && init?.method === "POST") {
    calls.push(`tools:${init.body}`);
    return toolsStatus === 200 ? json({ connection: {} }) : json({ error: { code: "tool_changed" } }, toolsStatus);
  }
  return json({});
}) as typeof fetch;

// The arming delay runs on a controlled clock: the card's ARM_MS timer is
// captured, and `arm()` advances Date.now and fires it. No wall-clock sleeps.
const { ARM_MS } = require("../src/features/decisions/StagedWriteDetail") as typeof import("../src/features/decisions/StagedWriteDetail");
const realNow = Date.now.bind(Date), realSetTimeout = globalThis.setTimeout;
let skew = 0;
Date.now = () => realNow() + skew;
const armTimers: Array<() => void> = [];
globalThis.setTimeout = ((fn: (...a: unknown[]) => void, ms?: number, ...rest: unknown[]) => {
  if (ms === ARM_MS) { armTimers.push(() => fn(...rest)); return 0; }
  return realSetTimeout(fn, ms, ...rest);
}) as typeof setTimeout;
async function arm() {
  skew += ARM_MS;
  await React.act(async () => { for (const fire of armTimers.splice(0)) fire(); });
}
/** Yield one macrotask so fetches resolve and React commits. */
const flush = () => React.act(async () => { await new Promise((resolve) => realSetTimeout(resolve, 0)); });
/** Wait for the condition; the iteration bound is only a loud failure safeguard. */
async function settle(check: () => void) {
  for (let i = 0; i < 200; i++) {
    try { check(); return; } catch { await flush(); }
  }
  check();
}
const text = (el: Element | null | undefined) => (el?.textContent ?? "").replace(/[\u2066-\u2069]/g, "").replace(/\s+/g, " ").trim();
const button = (host: Element, label: string) => Array.from(host.querySelectorAll("button")).find((b) => text(b) === label) as HTMLElement | undefined;
async function click(el: Element | undefined | null, quick = false) {
  assert(el, "click target exists");
  if (!quick && (text(el) === "Approve" || text(el) === "Always allow")) await arm();
  await React.act(async () => { el.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 1 } as any)); });
  await flush();
}
async function mount(owner: boolean, locale: "en" | "zh-TW", conversation: string) {
  win.localStorage.setItem("cadence-locale", locale);
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await React.act(async () => {
    root.render(React.createElement(LocaleProvider, null, React.createElement(ConversationDecisions, { conversation, revision: 0, canAlwaysAllow: owner })));
  });
  return { host, root };
}

// English, owner: three buttons, no Review; Approve once.
rows = [mcpRow("a1")];
let { host, root } = await mount(true, "en", "c1");
await settle(() => assert(button(host, "Approve") && button(host, "Always allow") && button(host, "Reject"), "three buttons"));
assert(!button(host, "Review"), "no Review button");
assert(text(host).includes("Run create_invoice on Acme Billing?") && text(host).includes("customer: Marlow Books") && host.querySelector("details"), "title, operation line and collapsed input");
await click(button(host, "Approve"), true);
assert(calls.length === 0, "a click before arming does nothing");
await click(button(host, "Approve"));
await settle(() => assert(text(host).includes("Approved · create_invoice") && !button(host, "Approve"), "the card shrinks to one line"));
assert(calls.join("|") === "approve:a1", "approve called once, no tool-mode update");
await React.act(async () => { root.unmount(); });

// Always allow: approve, then mode read.
calls.length = 0; rows = [mcpRow("a2")];
({ host, root } = await mount(true, "en", "c2"));
await settle(() => assert(button(host, "Always allow"), "loaded"));
await click(button(host, "Always allow"));
await settle(() => assert(text(host).includes("Always allowed · create_invoice"), "always allowed line"));
assert(calls.length === 2 && calls[0] === "approve:a2" && calls[1] === `tools:${JSON.stringify({ tools: [{ tool_name: "create_invoice", mode: "read" }] })}`, "approve then read: " + calls.join("|"));
await React.act(async () => { root.unmount(); });

// A refused mode update leaves the call approved and says why.
calls.length = 0; rows = [mcpRow("a3")]; toolsStatus = 409;
({ host, root } = await mount(true, "en", "c3"));
await settle(() => assert(button(host, "Always allow"), "loaded"));
await click(button(host, "Always allow"));
await settle(() => assert(text(host).includes("Approved · create_invoice") && !text(host).includes("Always allowed"), "approved only"));
assert(calls.join("|").startsWith("approve:a3") && text(host).length > "Approved · create_invoice".length, "a reason line is shown");
toolsStatus = 200;
await React.act(async () => { root.unmount(); });

// Reject calls deny.
calls.length = 0; rows = [mcpRow("a4")];
({ host, root } = await mount(true, "en", "c4"));
await settle(() => assert(button(host, "Reject"), "loaded"));
await click(button(host, "Reject"));
await settle(() => assert(text(host).includes("Rejected · create_invoice"), "rejected line"));
assert(calls.join("|") === "deny:a4", "deny called once");
await React.act(async () => { root.unmount(); });

// A non-owner has no Always allow.
rows = [mcpRow("a5")];
({ host, root } = await mount(false, "en", "c5"));
await settle(() => assert(button(host, "Approve"), "loaded"));
assert(!button(host, "Always allow"), "no Always allow for a non-owner");
await React.act(async () => { root.unmount(); });

// An app_call keeps today's card.
rows = [appRow("a6")];
({ host, root } = await mount(true, "en", "c6"));
await settle(() => assert(button(host, "Review") && !button(host, "Approve") && !button(host, "Always allow"), "app_call keeps Review"));
await React.act(async () => { root.unmount(); });

// zh-TW render.
rows = [mcpRow("a7")];
({ host, root } = await mount(true, "zh-TW", "c7"));
await settle(() => assert(button(host, "一律允許") && button(host, "核准") && button(host, "拒絕"), "zh-TW buttons: " + text(host)));
assert(text(host).includes("要在 Acme Billing 執行 create_invoice 嗎？") && text(host).includes("顯示輸入內容"), "zh-TW title and Show input");
await React.act(async () => { root.unmount(); });
console.log("inlineToolDecision ok");
}
void main();
