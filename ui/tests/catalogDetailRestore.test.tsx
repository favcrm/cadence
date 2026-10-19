/**
 * A successful legacy restore must leave one consistent installed state,
 * without retaining the removed state or offering another restore.
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
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
const CatalogDetail = (require("../src/features/explorer/CatalogDetail") as typeof import("../src/features/explorer/CatalogDetail")).default;

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
  if (url.pathname.endsWith("/stream")) return new Response(new ReadableStream(), { headers: { "Content-Type": "text/event-stream" } });
  if (url.pathname.endsWith("/restore") && init?.method === "POST") return json({ restored: true, digest: "sha256:x" });
  if (url.pathname === "/api/app-installations") return json([]);
  if (url.pathname === "/api/public-apps/crm") return new Response(JSON.stringify({ ok: false, error: { code: "not_found" } }), { status: 404, headers: { "Content-Type": "application/json" } });
  if (url.pathname === "/api/app-catalog/crm") return json(card);
  return json({});
}) as typeof fetch;

const host = document.createElement("div");
document.body.appendChild(host);
const root = createRoot(host);
const flush = () => React.act(async () => { await new Promise((r) => setTimeout(r, 0)); });
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null,
    React.createElement(NavigationProvider, { adapter: hostNavigation, children:
      React.createElement(CatalogDetail, { id: "crm", viewer: { operator: true, readOnly: false } }),
    })));
});
await flush();
await flush();
const restore = Array.from(host.querySelectorAll("button")).find((b) => b.textContent === "Restore");
assert(restore, "a removed app offers Restore");
await React.act(async () => { restore!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })); });
await flush();
assert(host.textContent!.includes("Installed"), "restored app reads Installed");
assert(!host.textContent!.includes("Removed"), "restored app no longer presents the contradictory Removed state");
assert(!Array.from(host.querySelectorAll("button")).some((button) => button.textContent === "Restore"), "a restored app cannot be restored again");
await React.act(async () => { root.unmount(); });
console.log("catalog detail restore checks passed");
}
void main().catch((e) => { console.error(e); (globalThis as any).process.exit(1); });
