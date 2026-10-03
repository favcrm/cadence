export {};
/** Mounted Ready-to-run QA (CAD-796, CAD-1119): only real blockers, a widened contract confirms inline. */
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
  // Bound and healthy: no blockers, and no approval step or approval noise.
  await render({ installation, bindings: [binding], connections, contextId: null });
  assert(text().includes("Ready to run"), "panel heading renders");
  assert(text().includes("Every connection slot is bound and healthy."), "ready summary");
  assert(!text().includes("Needs you"), "ready slot needs nothing");
  assert(!/approv/i.test(text()), "no approval text");

  // Install = consent: even an installation the board reads as unapproved
  // shows no approval row here; access is the app header's concern.
  await render({ installation: { ...installation, approved: false }, bindings: [binding], connections, contextId: null });
  assert(!text().includes("Needs you") && !/approv/i.test(text()), "approval is never a slot blocker");

  // A same-contract provider change migrates silently: still ready.
  const migrating = { ...binding, drift: { state: "migrates", changes: [{ field: "mapping.tool", from: "old", to: "new" }] } };
  await render({ installation, bindings: [migrating], connections, contextId: null });
  assert(!text().includes("Needs you"), "silent migration is not a blocker");

  // A widened contract: the slot blocks, names the change, and one inline
  // confirm re-binds the same connection at its current revision.
  const widened = { ...binding, drift: { state: "needs_confirm", changes: [
    { field: "mapping.scopes", from: ["read"], to: ["read", "write"] },
    { field: "registration_digest", from: "sha256:" + "a".repeat(64), to: "sha256:" + "b".repeat(64) },
  ] } };
  const confirmed: unknown[] = [];
  await render({ installation, bindings: [widened], connections, contextId: null, canWrite: true, onConfirm: (b: unknown) => confirmed.push(b) });
  assert(text().includes("Needs you"), "widened slot needs you");
  assert(text().includes("mapping.scopes: read → read, write"), "the change is shown");
  assert(!text().includes("registration_digest"), "bookkeeping is not part of the confirm");
  const button = Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.includes("Confirm publication change"));
  assert(button, "inline confirm offered");
  // Layout at 390 (Browser QA): the confirm and the change live in the
  // full-width detail block under the row, never inside the one-line flex
  // `.wa-step`, and the change wraps anywhere.
  assert(button.closest(".wa-step") === null, "confirm is not in the one-line row");
  assert(button.closest(".wa-blocker-detail") !== null, "confirm is in the full-width detail block");
  const diff = host.querySelector(".wa-diff");
  assert(diff && diff.closest(".wa-step") === null, "change is not in the one-line row");
  const css: string = require("fs").readFileSync(require("path").join(require("process").cwd(), "src/features/workspace-apps/workspace-apps.css"), "utf8");
  const rule = (name: string) => css.slice(css.indexOf(`${name} {`), css.indexOf("}", css.indexOf(`${name} {`)));
  assert(/overflow-wrap:\s*anywhere/.test(rule(".wa-diff")), "the change wraps anywhere");
  assert(/display:\s*grid/.test(rule(".wa-blocker-detail")) && /min-width:\s*0/.test(rule(".wa-blocker-detail")), "detail block is full width and shrinkable");
  await React.act(async () => { button.click(); }); await flush();
  assert(confirmed.length === 1 && (confirmed[0] as { id: string }).id === "bind-a", "confirm names the bound connection");
  // A read-only viewer sees the change but cannot confirm it.
  await render({ installation, bindings: [widened], connections, contextId: null, canWrite: false, onConfirm: () => {} });
  const disabled = Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.includes("Confirm"));
  assert(disabled && disabled.disabled, "read-only confirm disabled");

  // Revoke closed the binding: the slot asks for a fresh save.
  await render({ installation, bindings: [{ ...binding, state: "revoked" }], connections, contextId: null });
  assert(text().includes("Choose a publication connection below and save it."), "revoke action shown");

  // Rotation vanished the connection: the row says so.
  await render({ installation, bindings: [binding], connections: [], contextId: null });
  assert(text().includes("choose another publication connection below"), "rebind action shown");

  // Deregistered provider: bound and healthy custody — still not Ready.
  await render({ installation, bindings: [binding], connections: [{ ...connections[0], status: { ...connections[0].status, adapter_registered: false } }], contextId: null });
  assert(text().includes("no longer registered"), "registration action shown");
  assert(text().includes("Needs you"), "deregistered slot needs you");

  // Legacy untyped slots never authorize an effect: no cms row.
  assert(!text().includes("cms readiness"), "legacy slot stays out of Ready-to-run");
  await React.act(async () => root.unmount());
  console.log("readiness panel mounted checks passed");
}
void main();
