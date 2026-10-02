export {};
/** CAD-1026: host-chrome toggle between an installed app screen and the native controls. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
// Frame documents are served in-process: the test never reaches a real network listener.
const frameLoads: string[] = [];
const win = new Window({ url: "http://localhost/app-installations/install-a", settings: { fetch: { interceptor: {
  beforeAsyncRequest: async ({ request, window }: { request: { url: string }; window: any }) => {
    frameLoads.push(new URL(request.url).pathname);
    return new window.Response("<!doctype html><title>screen</title>", { headers: { "Content-Type": "text/html" } });
  },
} } } });
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
const WorkspaceApp = (require("../src/features/workspace-apps/WorkspaceApp") as typeof import("../src/features/workspace-apps/WorkspaceApp")).default;

function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
// One declared screen: WorkspaceApp mounts it over the native Social Content workspace.
const installation = { install_id: "install-a", title: "Social Content", name: "social-content", version: "0.2.0", digest: "bundle-digest", catalog_generation: "catalog-generation", approved: true, executable: true, storage_kind: "workspace", summary: "", guide: "", files: ["workflows/instagram.md", "workflows/facebook.md", "screens/main/screens.json"], capabilities: {}, connection_slots: [] };
const nonce = "a".repeat(64);
const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 1, tag: "main" };
const json = (value: unknown) => new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
const mounts: string[] = [];
const mountCount = () => mounts.length;
globalThis.fetch = async (input, init) => {
  const path = String(input);
  if (init?.method === "POST") {
    if (path === "/api/app-installations/install-a/screens/main/mount") { mounts.push(path); return json(receipt); }
    throw new Error(`Unexpected write ${path}`);
  }
  if (path === "/api/app-installations/install-a") return json(installation);
  if (path === "/api/app-installations/install-a/contexts") return json({ contexts: [] });
  if (path === "/api/app-installations/install-a/bindings") return json({ bindings: [] });
  if (path === "/api/connections") return json({ connections: [] });
  if (path === "/api/agents") return json({ agents: [] });
  if (path === "/api/app-runs?install_id=install-a") return json({ runs: [] });
  if (path === "/api/app-installations/install-a/effects") return json({ effects: [] });
  if (path.startsWith("/api/social-publishes")) return json({ intents: [] });
  throw new Error(`Unexpected read ${path}`);
};
win.setInterval = () => 1;
win.clearInterval = () => {};
const host = document.createElement("div"); document.body.append(host);
let root = createRoot(host);
const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
const control = (label: string) => Array.from(host.querySelectorAll("a, button")).find(value => value.textContent?.trim() === label);
const frame = () => host.querySelector("iframe");
async function click(element: Element | undefined | null) {
  assert(element, "Click target exists");
  await React.act(async () => { element.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })); });
  await flush(); await flush();
}
async function render() {
  await React.act(async () => root.render(React.createElement(WorkspaceApp, { installId: "install-a", viewer: { operator: true, readOnly: false } })));
  await flush(); await flush();
}

/** Stand-in for the frame's transferred port; records exactly what the host does with it. */
class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  sent: unknown[] = [];
  closed = false;
  start() {}
  close() { this.closed = true; }
  postMessage(value: unknown) { this.sent.push(value); }
  receive(value: unknown) { this.onmessage?.({ data: value }); }
}
/** Completes the init/ready handshake from the mounted frame and returns its live port. */
async function handshake(): Promise<Port> {
  const source = frame()?.contentWindow;
  assert(source, "Mounted frame has a window");
  const port = new Port();
  const event = new win.Event("message");
  for (const [key, value] of Object.entries({ data: { v: 1, op: "init", tag: "main", bridge_nonce: nonce, generation: 1 }, origin: "null", source, ports: [port] }))
    Object.defineProperty(event, key, { value });
  await React.act(async () => { win.dispatchEvent(event); });
  await React.act(async () => { port.receive({ v: 1, op: "ready" }); });
  await flush();
  assert(port.sent.length === 1, "Ready frame receives its projection over the port");
  return port;
}

async function main() {
  // Default is unchanged: the app screen mounts and the native workspace stays behind it.
  await render();
  assert(location.search === "", "Default URL carries no view");
  assert(mountCount() === 1 && frame(), "Default view mounts the declared app screen");
  assert(!control("New post"), "Default view does not render the native controls");
  const appView = control("App view"), nativeView = control("Native controls");
  assert(appView && nativeView, "Host chrome offers both views outside the frame");
  assert(!appView.closest("[aria-label='Installed app screen']") && !nativeView.closest("[aria-label='Installed app screen']"), "Toggle lives in host chrome, not the frame container");
  const port = await handshake();

  // Switching to native tears down the frame and the port, like a context switch.
  await click(nativeView);
  assert(new URLSearchParams(location.search).get("view") === "native", "Native view is deep-linkable");
  assert(!frame(), "Native view removes the app frame");
  assert(port.closed && port.onmessage === null, "Native view closes the frame's port");
  assert(control("New post"), "Native view renders the native workspace controls");
  assert(["Publish", "Schedule"].every(label => control(label)), "Native sections are reachable with the screen projection present");
  port.receive({ v: 1, op: "ready" });
  assert(port.sent.length === 1, "No stale channel: the closed port receives nothing further");
  assert(mountCount() === 1, "Native view does not mount the screen again");

  // Switching back remounts a fresh frame generation.
  await click(control("App view"));
  assert(!new URLSearchParams(location.search).has("view"), "App view is the default and drops the parameter");
  assert(mountCount() === 2 && frame(), "App view remounts the screen");
  assert(!control("New post"), "App view hides the native controls again");

  // Deep link straight into native controls: no mount at all.
  await React.act(async () => root.unmount()); root = createRoot(host);
  history.replaceState(null, "", "/app-installations/install-a?view=native");
  mounts.length = 0;
  await render();
  assert(mountCount() === 0 && !frame(), "Deep link to native never mounts the screen");
  assert(control("New post"), "Deep link to native renders the native controls");
  await React.act(async () => root.unmount());
  assert(frameLoads.length > 0 && frameLoads.every(path => path === receipt.mount), "Only the stubbed frame document was loaded");
  console.log("workspace app screen toggle checks passed");
}
void main();
