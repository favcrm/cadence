export {};
/**
 * CAD-1033: the sidebar footer is the desktop account-menu row. Mounts the
 * real App for each session state: the row in the sidebar, the signed-out
 * and unknown footers, the phone-only header avatar (lg:hidden), and no
 * "writes:" chip in the phone menu.
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
Object.defineProperty(globalThis, "EventSource", {
  configurable: true,
  value: class { onopen: null = null; onerror: null = null; addEventListener() {} removeEventListener() {} close() {} },
});
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  if (id === "react-markdown") return { __esModule: true, default: () => null };
  if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const App = (require("../src/App") as typeof import("../src/App")).default;

const user = { name: "Fable Chen", email: "fable@example.com", role: "member", handle: "fable", sub: "s" };
let meta: unknown = null;
const json = (value: unknown) => new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown) => {
  const path = String(input);
  if (path.startsWith("/api/meta")) return meta === "hang" ? new Promise(() => undefined) : json(meta);
  if (path === "/api/issues" || path.startsWith("/api/issues?")) return json({ issues: [] });
  if (path === "/api/projects") return json({ projects: [] });
  if (path === "/api/agents") return json({ daemon: "unreachable", agents: [], totals: null });
  if (path === "/api/health") return json({ ok: true, pm_present: false, projects: 0, issues: 0, daemon: "stub", embedded: true });
  if (path === "/api/update/banner") return json(null);
  if (path === "/api/setup") return json({ checks: [], offers: [] });
  if (path.startsWith("/api/threads/master")) return json({ thread: null, entries: [], more_before: false });
  return json({});
}) as typeof fetch;

const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
async function mountApp(state: unknown) {
  meta = state;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await React.act(async () => { root.render(React.createElement(App)); });
  for (let i = 0; i < 20; i++) await flush();
  return { host, done: () => { React.act(() => root.unmount()); host.remove(); } };
}
const base = { read_only: false, operator: true, actor: "Fable (board)" };
const rowTrigger = (host: Element) => host.querySelector("aside button[aria-haspopup=dialog]") as HTMLButtonElement | null;

// Signed in: the row lives in the sidebar, opens above it; the header avatar is phone-only.
{
  const { host, done } = await mountApp({ ...base, signed_in: true, session: { id: "s1", origin: "public", user } });
  const row = rowTrigger(host);
  assert(row, "the sidebar footer renders the row trigger");
  assert(row.className.includes("account-row") && row.textContent?.includes("Fable Chen") && row.textContent.includes("member · localhost"), "row shows name and role · host");
  assert(!(host.querySelector("aside")?.textContent ?? "").toLowerCase().includes("session"), "no old SESSION block");
  await React.act(async () => { row.click(); });
  const dialog = host.querySelector("aside [role=dialog]");
  assert(dialog && (dialog.className as string).includes("bottom-full"), "the row opens the dialog above itself");
  const headerTrigger = host.querySelector("header button[aria-haspopup=dialog]");
  assert(headerTrigger, "the header still carries the avatar trigger");
  let el: Element | null = headerTrigger;
  while (el && el.tagName !== "HEADER" && !(el.className as string).split(/\s+/).includes("lg:hidden")) el = el.parentElement;
  assert(el && el.tagName !== "HEADER", "the header avatar is hidden from lg up (lg:hidden)");
  // Phone menu: identity has one home, so no writes chip.
  const open = host.querySelector("button[aria-label='Open navigation']") as HTMLButtonElement;
  await React.act(async () => { open.click(); });
  const menu = host.querySelector("#mobile-navigation");
  assert(menu && menu.textContent?.includes("Refresh board"), "phone menu is open");
  assert(!(menu.textContent ?? "").includes("writes:"), "no writes: chip in the phone menu");
  done();
}

// Read-only board with a session: the row stays, the dialog hides Sign out.
{
  const { host, done } = await mountApp({ ...base, read_only: true, signed_in: true, session: { id: "s1", origin: "public", user } });
  const row = rowTrigger(host);
  assert(row, "read-only board with a session keeps the row");
  await React.act(async () => { row.click(); });
  assert(!(host.querySelector("aside [role=dialog]")?.textContent ?? "").includes("Sign out"), "read-only: no Sign out");
  done();
}

// Signed out: plain text, no menu, and the header keeps the theme button instead of an avatar.
{
  const { host, done } = await mountApp({ ...base, signed_in: false });
  const footer = host.querySelector("aside .mt-auto");
  assert(footer?.textContent?.trim() === "Not signed in · read only", `signed-out footer text, got ${footer?.textContent}`);
  assert(!host.querySelector("[data-account-menu]"), "signed out: no account menu anywhere");
  done();
}

// Unknown (meta not loaded): the placeholder, no menu.
{
  const { host, done } = await mountApp("hang");
  assert(host.querySelector("aside .mt-auto")?.textContent?.trim() === "…", "unknown footer keeps the ellipsis");
  assert(!host.querySelector("[data-account-menu]"), "unknown: no account menu");
  done();
}
console.log("sidebar account checks passed");
}
declare const process: any;
main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
