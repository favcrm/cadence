export {};
/** Mounted Ready-to-run QA (CAD-796): rebind → approval required → reapproval, revoke → not ready. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/app-installations/install-a" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLDialogElement", "HTMLInputElement", "HTMLTextAreaElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function(this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { ReadinessPanel } = require("../src/features/workspace-apps/ReadinessPanel");

const installation = {
  install_id: "install-a", name: "social-content", title: "Social Content",
  version: "0.5.0", summary: "", digest: "sha256:current",
  catalog_generation: "gen-1", storage_kind: "workspace",
  project_link: null, approved: true, executable: false,
  approval: { state: "approved" }, guide: "", files: [],
  capabilities: {
    publication: {
      schema: 1, capability: "text.publish", version: 1,
      action: "publish", resource_kind: "connection_account", effect: "send",
    },
  },
  connection_slots: ["cms", "publication"],
};
const mapping = {
  capability: "text.publish", version: 1, action: "publish",
  resource_kind: "connection_account", effect: "send",
  semantics: "LocalMarkdownSink", scopes: [],
  input_contract: "in", output_contract: "out",
};
const binding = {
  id: "bind-a", install_id: "install-a", context_id: null,
  slot: "publication", revision: 2, state: "configured", digest: "sha256:bind",
  config: {
    bundle_digest: "sha256:current", connection_id: "builtin-local",
    provider: "local", account: "local", mapping,
  },
};
const connections = [{
  id: "builtin-local", provider: "local", account: "local", kind: "builtin",
  scopes: [], descriptor: { action_mappings: [mapping] },
  status: { manifest_status: "matched", custody_available: true, adapter_registered: true },
}];

const host = document.createElement("div"); document.body.append(host);
const root = createRoot(host);
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
const text = () => host.textContent ?? "";
async function render(props: Record<string, unknown>) {
  await React.act(async () => root.render(React.createElement(ReadinessPanel, props))); await flush();
}
async function main() {
  // Bound, healthy and approved: the slot reads Ready with its connection.
  await render({ installation, bindings: [binding], connections, contextId: null });
  assert(text().includes("Ready to run"), "panel heading renders");
  assert(text().includes("local/local (builtin-local)"), "bound connection named");
  assert(text().includes("revision 2"), "binding revision named");
  assert(text().includes("approval in force"), "approval state named");
  assert(text().includes("Ready") && !text().includes("Needs you"), "ready slot needs nothing");

  // Rebind withdrew approval: the same binding now requires reapproval.
  await render({ installation: { ...installation, approved: false }, bindings: [binding], connections, contextId: null });
  assert(text().includes("Needs you"), "unapproved slot needs you");
  assert(text().includes("Approve the app's current version before running."), "reapproval action shown");

  // Revoke closed the binding, which withdrew approval: the slot asks for a
  // fresh save and reapproval, never a silent gap.
  await render({ installation: { ...installation, approved: false }, bindings: [{ ...binding, state: "revoked" }], connections, contextId: null });
  assert(text().includes("Choose a publication connection below and save it."), "revoke action shown");
  assert(!text().includes("approval in force"), "revoked binding carries no approval");

  // Rotation vanished the connection: the row says so with its reason.
  await render({ installation, bindings: [binding], connections: [], contextId: null });
  assert(text().includes("health missing-connection"), "missing connection health named");
  assert(text().includes("choose another publication connection below"), "rebind action shown");

  // Legacy untyped slots never authorize an effect: no cms row.
  assert(!text().includes("cms readiness"), "legacy slot stays out of Ready-to-run");
  await React.act(async () => root.unmount());
  console.log("readiness panel mounted checks passed");
}
void main();
