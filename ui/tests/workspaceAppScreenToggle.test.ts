export {};
/** CAD-1040: an installed app screen renders with no host tab bar; `?screen=native` is the operator back door (was CAD-1026's toggle). */
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
// CAD-1560: the workspace takes its scope from an owner (AppShell in
// production); this harness owns the selection the same way — resolve
// the remembered/active defaults once and follow emitted picks.
const { initialContext, rememberedContext, subscribeContext } = require("../src/features/workspace-apps/contextSelection");
function ScopeOwner({ installId, activeIds, children }: { installId: string; activeIds: string[]; children: (contextId: string) => any }) {
  const [contextId, setContextId] = React.useState(() => initialContext(rememberedContext(installId), activeIds));
  React.useEffect(() => subscribeContext((changed: string, next: string | null) => {
    if (changed === installId) setContextId(next ?? "");
  }), [installId]);
  return children(contextId);
}
const mountApp = (viewer: any) => React.createElement(LocaleProvider, null, React.createElement(ScopeOwner, { installId: "install-a", activeIds: [], children: (contextId: string) => React.createElement(WorkspaceApp, { installId: "install-a", contextId, viewer }) }));
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");

function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
// One declared screen: WorkspaceApp mounts it over the native Social Content workspace.
const installation = { install_id: "install-a", title: "Social Content", name: "social-content", version: "0.2.0", digest: "bundle-digest", catalog_generation: "catalog-generation", approved: true, executable: true, storage_kind: "workspace", summary: "", guide: "", files: ["workflows/instagram.md", "workflows/facebook.md", "screens/main/screens.json"], capabilities: {}, connection_slots: [] };
const nonce = "a".repeat(64);
const receipt = { mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 1, tag: "main" };
const json = (value: unknown) => new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
const mounts: string[] = [];
const mountCount = () => mounts.length;
/** Holds the mount response so a test can change the pick mid-request. */
let holdMount: Promise<void> | null = null;
globalThis.fetch = async (input, init) => {
  const path = String(input);
  if (init?.method === "POST") {
    if (path === "/api/app-installations/install-a/screens/main/mount") { mounts.push(path); if (holdMount) await holdMount; return json(receipt); }
    throw new Error(`Unexpected write ${path}`);
  }
  if (path === "/api/app-installations/install-a") return json(installation);
  if (path === "/api/app-installations/install-a/automatic-updates") return json({ install_id: "install-a", automatic: false });
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
// The host stands in for AppShell's outlet: WorkspaceApp's top-level nodes are its direct children.
const host = document.createElement("section"); host.className = "app-shell-outlet"; document.body.append(host);
// happy-dom has no layout engine, so apply the shipped flex-column selectors to the real DOM:
// without it the outlet stays a grid of auto rows and the frame never receives the pane's height.
const shellCss: string = require("fs").readFileSync(require("path").join((globalThis as any).process.cwd(), "src/features/app-shell/app-shell.css"), "utf8");
const columnRule = Array.from(shellCss.matchAll(/([^{}]+)\{([^}]*)\}/g)).find(match => match[1].includes(".app-shell-outlet:has(") && /flex-direction:\s*column/.test(match[2]));
assert(columnRule, "app-shell.css keeps a flex-column rule for screen outlets");
const columnSelector = columnRule[1].replace(/\/\*[\s\S]*?\*\//g, "").trim();
const columnLayout = () => host.matches(columnSelector);
let root = createRoot(host);
const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
const control = (label: string) => Array.from(host.querySelectorAll("a, button")).find(value => value.textContent?.trim() === label);
const frame = () => host.querySelector("iframe");
async function go(href: string) {
  await React.act(async () => navigate(href, { replace: true }));
  await flush(); await flush();
}
/** No host chrome: the outlet's only children are the app screen (and its status/fallback), never a tab bar. */
function noTabBar(why: string) {
  assert(!host.querySelector("nav[aria-label='App view']"), `${why}: no App view tab bar`);
  assert(!control("App view") && !control("Native controls"), `${why}: no App view / Native controls tabs`);
}
async function render(operator = true) {
  await React.act(async () => root.render(mountApp({ operator, readOnly: false })));
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
  // Default: only the app screen renders, filling the pane, with no tab bar.
  await render();
  assert(location.search === "", "Default URL carries no screen parameter");
  assert(mountCount() === 1 && frame(), "Default view mounts the declared app screen");
  assert(!control("New post"), "Default view does not render the native controls");
  noTabBar("Default view");
  assert(host.firstElementChild?.getAttribute("aria-label") === "Installed app screen", "The app screen is the outlet's first child: nothing sits above it");
  assert(columnLayout(), "App screen lays the outlet out as a column, so the frame fills the pane");
  const port = await handshake();

  // The operator back door: ?screen=native tears down the frame and the port, like a context switch.
  await go("/app-installations/install-a?screen=native");
  assert(!frame(), "Native view removes the app frame");
  assert(port.closed && port.onmessage === null, "Native view closes the frame's port");
  assert(control("New post"), "Native view renders the native workspace controls");
  assert(["Publish", "Schedule"].every(label => control(label)), "Native sections are reachable with the screen projection present");
  noTabBar("Native view");
  assert(host.children.length === 1 && host.firstElementChild?.getAttribute("aria-label") === "Workspace app", "Native view is the plain native workspace, no host chrome");
  port.receive({ v: 1, op: "ready" });
  assert(port.sent.length === 1, "No stale channel: the closed port receives nothing further");
  assert(mountCount() === 1, "Native view does not mount the screen again");

  // Dropping the parameter remounts a fresh frame generation.
  await go("/app-installations/install-a");
  assert(mountCount() === 2 && frame(), "Leaving native remounts the screen");
  assert(!control("New post"), "App screen hides the native controls again");
  noTabBar("Remounted view");

  // Deep link straight into native controls: no mount at all.
  await React.act(async () => root.unmount()); root = createRoot(host);
  history.replaceState(null, "", "/app-installations/install-a?screen=native");
  mounts.length = 0;
  await render();
  assert(mountCount() === 0 && !frame(), "Deep link to native never mounts the screen");
  assert(control("New post"), "Deep link to native renders the native controls");
  noTabBar("Deep-linked native view");

  // The board's applied pick reaches the frame URL; a live change is pushed to
  // the open frame, never a re-mount that would discard the operator's draft.
  await React.act(async () => root.unmount()); root = createRoot(host);
  history.replaceState(null, "", "/app-installations/install-a");
  document.documentElement.setAttribute("data-theme", "light");
  mounts.length = 0;
  await render();
  assert(mountCount() === 1 && frame(), "A themed mount still mounts the screen");
  assert(frame()?.getAttribute("src") === `${receipt.mount}?theme=light`, "The pick rides the frame URL");
  const live: any[] = [];
  frame()!.contentWindow!.addEventListener("message", (event: any) => live.push(event.data));
  document.documentElement.setAttribute("data-theme", "dark");
  await flush(); await flush();
  assert(mountCount() === 1, "A pick change never re-mounts the open screen");
  assert(live.some(message => message?.type === "cadence-theme" && message?.theme === "dark"), "The open frame is told the new pick");
  document.documentElement.removeAttribute("data-theme");
  await React.act(async () => root.unmount()); root = createRoot(host);
  mounts.length = 0;
  await render();
  assert(frame()?.getAttribute("src") === receipt.mount, "System leaves the frame URL query-less");
  document.documentElement.removeAttribute("data-theme");
  await React.act(async () => root.unmount()); root = createRoot(host);

  // A pick that changes while the mount request is pending must not mint the
  // frame with the old theme: the URL is built from the pick current then.
  await React.act(async () => root.unmount()); root = createRoot(host);
  history.replaceState(null, "", "/app-installations/install-a");
  document.documentElement.setAttribute("data-theme", "light");
  mounts.length = 0;
  let release!: () => void;
  holdMount = new Promise<void>(resolve => { release = resolve; });
  await render();
  document.documentElement.setAttribute("data-theme", "dark");
  await flush();
  await React.act(async () => { release(); holdMount = null; await new Promise(resolve => setTimeout(resolve, 0)); });
  await flush(); await flush();
  assert(mountCount() === 1, "A pick change during the mount still mounts once");
  assert(frame()?.getAttribute("src") === `${receipt.mount}?theme=dark`, "The pending mount uses the pick current at frame creation");
  document.documentElement.removeAttribute("data-theme");
  await React.act(async () => root.unmount()); root = createRoot(host);

  // The back door is operator-only: a non-operator deep link gets neither native controls nor a screen.
  await React.act(async () => root.unmount()); root = createRoot(host);
  mounts.length = 0;
  await render(false);
  assert(!control("New post") && !control("Publish"), "Non-operator deep link renders no native controls");
  assert(mountCount() === 0 && !frame(), "Non-operator deep link mounts no screen");
  assert(host.textContent?.includes("Sign in as the operator"), "Non-operator sees the operator sign-in notice");
  await React.act(async () => root.unmount());
  assert(frameLoads.length > 0 && frameLoads.every(path => path === receipt.mount), "Only the stubbed frame document was loaded");
  console.log("workspace app screen host checks passed");
}
void main();
