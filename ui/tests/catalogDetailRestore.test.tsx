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
const CatalogDetail = require("../src/features/explorer/CatalogDetail").default;

// CAD-1209 (browser QA on #847): Restore on a removed app's detail page must
// leave the page reading "Installed" only. The local card kept `removed: true`,
// so the hero showed both chips and a "restore window has closed" banner.
let card: Record<string, unknown> = {
  id: "crm", source_kind: "builtin", name: "crm", title: "CRM", version: "0.1.0", trust: "cadence",
  state: "available", removed: true, restorable: true, install_id: "inst-1",
  listing: { tagline: "t", category: "customers" }, access: [], digest: "sha256:x",
};
globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
  const url = new URL(String(typeof input === "string" ? input : (input as { url: string }).url), "http://localhost");
  const json = (v: unknown) => new Response(JSON.stringify(v), { status: 200, headers: { "Content-Type": "application/json" } });
  if (url.pathname.endsWith("/restore") && init?.method === "POST") return json({ restored: true, digest: "sha256:x" });
  if (url.pathname === "/api/app-catalog/crm") return json(card);
  return json({});
}) as typeof fetch;

const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });
await React.act(async () => {
  root.render(React.createElement(CatalogDetail, { id: "crm", viewer: { operator: true, readOnly: false } }));
});
await flush();
assert(host.textContent!.includes("CRM was removed"), "the removed banner shows before Restore");
const restore = Array.from(host.querySelectorAll("button")).find((b) => b.textContent === "Restore");
assert(restore, "a removed app offers Restore");
await React.act(async () => { restore!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })); });
await flush();
const chips = Array.from(host.querySelectorAll(".chip")).map((c) => c.textContent);
assert(chips.includes("Installed"), "restored app reads Installed");
assert(!chips.includes("Removed"), `no Removed chip after Restore, got ${JSON.stringify(chips)}`);
assert(!host.textContent!.includes("restore window has closed"), "no closed-window banner after Restore");
assert(!host.textContent!.includes("was removed"), "no removed banner after Restore");
await React.act(async () => { root.unmount(); });
console.log("catalog detail restore checks passed");
}
void main().catch((e) => { console.error(e); (globalThis as any).process.exit(1); });
