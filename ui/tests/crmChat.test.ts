/** CAD-1051 / CAD-1046: the shell chat — rail, markdown, folded steps,
 *  directive cards, context chip and quick prompts. */
declare function require(name: string): any;
declare const process: { cwd(): string };
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(a: unknown, e: unknown, why: string) {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-crm" });
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
const AppShell = require("../src/features/app-shell/AppShell").default;
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
const { matchDirective, hideIds } = require("../src/features/app-shell/chat/directive") as typeof import("../src/features/app-shell/chat/directive");
const { parseAppChat } = require("../src/features/app-shell/chat/contract") as typeof import("../src/features/app-shell/chat/contract");
const { chatContext } = require("../src/features/app-shell/chat/Conversation") as typeof import("../src/features/app-shell/chat/Conversation");
// The descriptor under test is the package's own file, not a copy.
const crmDescriptor = JSON.parse(require("fs").readFileSync(require("path").join(process.cwd(), "..", "workspace-apps", "crm", "app-chat.json"), "utf8"));
const crmChat = parseAppChat(crmDescriptor);

const SECRET = ["tok", "en-", "abcdef0123456789"].join("");
const confirm = JSON.stringify({ cadence_csv_import: { request_id: "req-9", confirm_token: SECRET } });

// Pure directive rules, against the CRM package's own descriptor.
const d = matchDirective(confirm, crmChat);
assert(d && d.kind === "card" && d.card.title === "Customer import", "csv confirm parses to a human summary");
assert(!JSON.stringify(d).includes(SECRET) && !JSON.stringify(d).includes("req-9"), "summary carries no token or request id");
equal(matchDirective("plain words", crmChat), null, "prose is not a directive");
equal(matchDirective("{not json", crmChat), null, "broken JSON is not a directive");
assert(matchDirective(JSON.stringify({ confirm_token: SECRET }), crmChat)?.kind === "confirmation", "any confirm_token JSON is carded, never shown");
assert(matchDirective(confirm, null)?.kind === "confirmation", "with no descriptor a token-bearing body is still never printed");
equal(hideIds("done in ctx-abc-123 now"), "done in this workspace now", "ctx ids hidden");
equal(chatContext(crmChat, "customers", false)?.label, "Customers", "page chip");
assert(chatContext(crmChat, "segments", true)?.label.startsWith("Segment"), "record chip names the open record");
assert((chatContext(crmChat, "campaigns", false)?.prompts.length ?? 0) >= 2 && (chatContext(crmChat, "campaigns", false)?.prompts.length ?? 9) <= 3, "2-3 prompts");
equal(chatContext(crmChat, null, false), null, "no screen, no chip");

const crm = { install_id: "install-crm", title: "CRM", name: "crm", version: "0.1.0", digest: "d", catalog_generation: "g", approved: true, storage_kind: "workspace", files: [], capabilities: null, connection_slots: [] };
const ctxs = { contexts: [{ id: "ctx-a", install_id: "install-crm", revision: 1, state: "active", digest: "ca", config: { schema: 1, label: "Acme", input_defaults: {} } }] };
const ent = (seq: number, role: string, kind: string, text: string, payload: unknown = null) => ({ seq, role, kind, message: `m${seq}`, text, created: "2026-10-02T00:00:00Z", payload });
const stamp = { source: "operator", app: { install_id: "install-crm", context_id: "ctx-a", verified: true, context_revision: 1, context_digest: "ca" } };
let thread: any = {
  entries: [
    ent(1, "operator", "message", "Build a VIP segment", stamp),
    ent(2, "agent", "tool_call", "list_segments", { tool_use_id: "t1" }),
    ent(3, "agent", "tool_result", "ok", { tool_use_id: "t1" }),
    ent(4, "agent", "tool_call", "create_segment", { tool_use_id: "t2" }),
    ent(5, "agent", "tool_result", "ok", { tool_use_id: "t2" }),
    ent(6, "agent", "turn_result", "Created **VIP customers** in ctx-a", { source: "master" }),
    ent(7, "operator", "message", confirm, stamp),
  ],
  more_before: false,
};
const json = (v: unknown) => new Response(JSON.stringify(v), { status: 200, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown) => {
  const path = String(input);
  if (path === "/api/app-installations/install-crm") return json(crm);
  if (path === "/api/app-installations/install-crm/contexts") return json(ctxs);
  if (path.endsWith("/chat-descriptor")) return json({ descriptor: crmDescriptor, digest: crm.digest, app: "crm" });
  if (path.endsWith("/conversations")) return new Response("{}", { status: 404 });
  if (path.startsWith("/api/threads/master")) return json(thread);
  const url = new URL(path, "http://localhost");
  if (url.pathname === "/api/crm-send/list") return json({ sends: [] });
  if (url.pathname === "/api/crm-send/origin") return json({ unsubscribe_origin: null, stored: false });
  if (url.pathname.endsWith("/records")) return json({ records: [], truncated: false, next_cursor: null });
  if (url.pathname.endsWith("/list")) return json({ segments: [], exclusions: [], suppressions: [], contents: [], proposals: [], bindings: [] });
  throw new Error(`Unexpected read ${path}`);
}) as typeof fetch;

const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });
async function click(el: Element | null | undefined) {
  assert(el, "click target exists");
  await React.act(async () => { el.dispatchEvent(new MouseEvent("click", { bubbles: true })); });
  await flush();
}
history.pushState(null, "", "/app-installations/install-crm?ctx=ctx-a");
await React.act(async () => { root.render(React.createElement(AppShell, { installId: "install-crm", viewer: { operator: true, readOnly: false } })); });
for (let i = 0; i < 6; i++) await flush();
const pane = () => host.querySelector("[data-chat-pane]") as HTMLElement;
const grid = () => host.querySelector(".app-shell-grid") as HTMLElement;
const text = () => pane().textContent ?? "";

// Markdown renders, raw markers do not.
assert(host.querySelector('[data-kind="answer"] strong')?.textContent === "VIP customers", "markdown bold renders as <strong>");
assert(!text().includes("**"), "no raw ** in the chat");
// Tool steps fold to one line.
const steps = host.querySelectorAll("[data-chat-steps]");
equal(steps.length, 1, "tool run folds into one line");
assert((steps[0].textContent ?? "").includes("2 steps"), "the line counts the steps");
assert(!text().includes("list_segments") && !text().includes("create_segment"), "no per-step rows");
// Directive card, never raw JSON, token, request id or ctx id.
assert(host.querySelector('[data-chat-card="directive"]'), "CSV confirm renders as a card");
for (const bad of [SECRET, "req-9", "confirm_token", "cadence_csv_import", "ctx-a", "{"]) assert(!text().includes(bad), `chat never shows ${bad}`);
assert(text().includes("Customer import"), "card reads as a human summary");
// Context chip + quick prompts that fill, not send.
const chip = host.querySelector("[data-chat-context]");
assert(chip && (chip.textContent ?? "").includes("Customers"), "chip names the page");
const prompts = Array.from(chip!.querySelectorAll("button"));
assert(prompts.length >= 2 && prompts.length <= 3, "two or three quick prompts");
await click(prompts[0]);
const box = host.querySelector("#app-shell-chat-box") as HTMLTextAreaElement;
equal(box.value, prompts[0].textContent, "prompt fills the composer");
await React.act(async () => { navigate("/app-installations/install-crm?ctx=ctx-a&crm=segments"); });
await flush();
assert((host.querySelector("[data-chat-context]")?.textContent ?? "").includes("Segments"), "chip follows the page");
await React.act(async () => { navigate("/app-installations/install-crm?ctx=ctx-a&crm=segments&record=seg-1"); });
await flush();
assert((host.querySelector("[data-chat-context]")?.textContent ?? "").includes("Segment (open)"), "chip names the open record");

// Collapse to a rail; draft and pane survive; waiting dot on new reply.
const before = box.value;
await click(host.querySelector(".app-chat-collapse"));
assert(grid().hasAttribute("data-chat-collapsed"), "grid collapses");
equal(pane().getAttribute("data-collapsed"), "true", "pane marks collapsed");
equal(host.querySelectorAll("[data-chat-pane]").length, 1, "still one pane");
equal((host.querySelector("#app-shell-chat-box") as HTMLTextAreaElement).value, before, "draft survives collapse");
assert(!host.querySelector(".app-chat-dot"), "no dot without news");
thread = { ...thread, entries: [...thread.entries, ent(8, "agent", "turn_result", "Another reply", { source: "master" })] };
await React.act(async () => { await (require("../src/lib/resources") as any).resources.masterThread.refresh(); });
await flush();
assert(host.querySelector(".app-chat-dot"), "waiting dot appears for a reply while collapsed");
await click(host.querySelector(".app-chat-rail"));
assert(!grid().hasAttribute("data-chat-collapsed"), "rail expands the chat");
assert(!host.querySelector(".app-chat-dot"), "dot clears once seen");
assert(host.querySelector("aside, [data-chat-pane]")!.compareDocumentPosition(host.querySelector(".app-shell-outlet")!) & 4, "chat stays before (left of) the workspace");
console.log("crm chat checks passed");
}
void main();
