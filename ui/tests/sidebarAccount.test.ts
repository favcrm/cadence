import { streamFixture } from "./streamFixture";
export {};
/**
 * CAD-1033/CAD-1452: authenticated sidebar account controls and the
 * fail-closed App admission boundary. Unknown or refused sessions never
 * mount the private shell or start its reads.
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
  if (id === "react-markdown") return { __esModule: true, default: () => null };
  if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const AppComponent = (require("../src/App") as typeof import("../src/App")).default;
const App = () => React.createElement(LocaleProvider, null, React.createElement(AppComponent));

const user = { name: "Fable Chen", email: "fable@example.com", role: "member", handle: "fable", sub: "s" };
const validSession = { id: "s1", origin: "public", created: 1700000000, last_used: 1700000000, idle_expires_at: 4102444800, expires_at: 4102444800, user_agent: "test", user };
let meta: unknown = null;
let privateReads = 0;
let pendingMeta: ((response: Response) => void)[] = [];
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
globalThis.fetch = (async (input: unknown) => {
  const path = String(input);
  if (path.startsWith("/api/meta")) return meta === "hang" ? new Promise<Response>((resolve) => pendingMeta.push(resolve)) : json(meta);
  privateReads++;
  if (new URL(path, "http://localhost").pathname.endsWith("/stream")) return streamFixture(path);
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
async function mountApp(state: unknown, storedKey?: string) {
  meta = state;
  privateReads = 0;
  pendingMeta = [];
  sessionStorage.clear();
  if (storedKey) sessionStorage.setItem("cadence.sessionKey", storedKey);
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
  const { host, done } = await mountApp({ ...base, signed_in: true, session: validSession });
  const row = rowTrigger(host);
  assert(row, "the sidebar footer renders the row trigger");
  assert(row.className.includes("account-row") && row.textContent?.includes("Fable Chen") && row.textContent.includes("member"), "row shows name and role");
  assert(!row.textContent?.includes("localhost"), "row no longer repeats the host");
  assert(!(host.querySelector("aside")?.textContent ?? "").toLowerCase().includes("session"), "no old SESSION block");
  assert(!row.closest(".overflow-y-auto"), "the footer is outside the scrolling nav, so its dialog is not clipped");
  const aside = host.querySelector("aside")!;
  const raised = () => /\bz-\[/.test(aside.className) || aside.hasAttribute("data-account-open");
  assert(!raised(), "menu closed: the aside carries no z-index (it must not cover the page's scrims)");
  await React.act(async () => { row.click(); });
  assert(raised(), "menu open: the aside is raised over the page");
  await React.act(async () => { row.click(); });
  assert(!raised(), "menu closed again: the aside drops back");
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
  const menu = document.querySelector("#mobile-navigation");
  assert(menu && menu.textContent?.includes("Refresh board"), "phone menu is open");
  assert(!(menu.textContent ?? "").includes("writes:"), "no writes: chip in the phone menu");
  await React.act(async () => {
    document.activeElement?.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true, bubbles: true, cancelable: true }));
  });
  assert(!document.querySelector('[role="dialog"][aria-label="Search issues and knowledge"]'), "global search cannot open behind the modal drawer");
  const mobileAccount = menu.querySelector<HTMLButtonElement>(".account-row");
  assert(mobileAccount, "the drawer exposes its account menu");
  await React.act(async () => { mobileAccount.click(); });
  const settings = menu.querySelector<HTMLAnchorElement>('[data-account-menu] [role="dialog"] a');
  assert(settings, "the drawer account menu links to settings");
  await React.act(async () => { settings.click(); });
  assert(!document.querySelector("#mobile-navigation") && location.pathname === "/settings/preferences", "account navigation closes the drawer and opens preferences");
  assert(!host.querySelector<HTMLElement>("[data-app-shell]")?.inert, "the destination is interactive after navigation");
  done();
  history.replaceState(null, "", "/");
}

// CAD-1034: both triggers (sidebar row, phone header avatar) carry the version line.
{
  const sha = "c78b8a0535e0ddbc58b77d9df4a757261413c842";
  const { host, done } = await mountApp({ ...base, version: "1.2.3", build_commit: sha, signed_in: true, session: validSession });
  for (const [where, selector] of [["sidebar row", "aside button[aria-haspopup=dialog]"], ["header avatar", "header button[aria-haspopup=dialog]"]]) {
    const trigger = host.querySelector(selector) as HTMLButtonElement | null;
    assert(trigger, `${where}: trigger renders`);
    await React.act(async () => { trigger.click(); });
    const line = host.querySelector(`${selector.split(" ")[0]} [role=dialog] [data-version-line]`) as HTMLAnchorElement | null;
    assert(line, `${where}: the dialog carries the version line`);
    assert(line.textContent === "v1.2.3+c78b8a0", `${where}: version text, got ${line.textContent}`);
    assert(line.getAttribute("href") === "/settings/update", `${where}: links to Settings > Update, got ${line.getAttribute("href")}`);
    await React.act(async () => { trigger.click(); });
  }
  done();
}

// Read-only policy retains the authenticated account identity.
{
  const { host, done } = await mountApp({ ...base, read_only: true, signed_in: true, session: validSession });
  const row = rowTrigger(host);
  assert(row, "read-only board with a session keeps the row");
  done();
}

// signed_in is not enough when the accompanying session is malformed.
// Even the local anonymous opt-in cannot bypass a malformed signed-in claim.
const malformedSessions: [string, unknown][] = [
  ["missing session", undefined],
  ["null session", null],
  ["empty id", { ...validSession, id: "" }],
  ["invalid origin", { ...validSession, origin: "agent" }],
  ["public user missing", { ...validSession, user: null }],
  ["public subject missing", { ...validSession, user: { ...user, sub: undefined } }],
  ["public subject empty", { ...validSession, user: { ...user, sub: "" } }],
  ["public role invalid", { ...validSession, user: { ...user, role: "administrator" } }],
];
for (const field of ["created", "expires_at", "idle_expires_at"]) {
  malformedSessions.push([`${field} missing`, { ...validSession, [field]: undefined }]);
  // JSON serializes non-finite numbers as null; neither is a valid timestamp.
  malformedSessions.push([`${field} nonfinite`, { ...validSession, [field]: Number.POSITIVE_INFINITY }]);
}
for (const [label, session] of malformedSessions) {
  const { host, done } = await mountApp({ ...base, hosted: false, signed_in: true, anonymous_reads: true, session }, "retained-malformed-session-key");
  assert(host.querySelector("[data-auth-state='unavailable'] h1"), `${label}: malformed proof is unavailable, never admitted or signed out`);
  assert(!host.querySelector("aside, [data-account-menu]"), `${label}: malformed proof mounts no private workspace`);
  assert(privateReads === 0, `${label}: malformed proof triggers no private reads`);
  assert(sessionStorage.getItem("cadence.sessionKey") === "retained-malformed-session-key", `${label}: uncertainty retains the tab credential`);
  done();
}

// Read-only is not proof of a session, and hosted mode never permits anonymous reads.
for (const state of [
  { ...base, hosted: true, signed_in: false },
  { ...base, hosted: false, read_only: true, signed_in: false },
  { ...base, hosted: true, signed_in: false, anonymous_reads: true },
]) {
  const { host, done } = await mountApp(state);
  assert(host.querySelector("[data-auth-state='signedout'] h1"), "unsigned session has a public sign-in heading");
  assert(!host.querySelector("aside, header [data-account-menu]"), "unsigned session mounts no private shell");
  assert(privateReads === 0, "unsigned metadata cannot trigger private reads");
  done();
}

// Only explicitly opted-in local anonymous reads may mount the unsigned workspace.
{
  const { host, done } = await mountApp({ ...base, hosted: false, signed_in: false, anonymous_reads: true });
  assert(host.querySelector("aside"), "explicit local anonymous access mounts the workspace");
  assert(privateReads > 0, "admitted local workspace may load private resources");
  assert(!host.querySelector("[data-account-menu]"), "anonymous reader never gains an account identity");
  done();
}

// A signed-in member is admitted without operator capability.
{
  const { host, done } = await mountApp({ ...base, operator: false, signed_in: true, session: { ...validSession, id: "member" } });
  assert(rowTrigger(host), "member identity mounts the private workspace");
  assert(privateReads > 0, "signed-in member may read workspace resources");
  done();
}

// No loading-shell flash or background private fetch before the first proof.
{
  const { host, done } = await mountApp("hang");
  assert(host.querySelector("[data-auth-state='checking'] h1"), "pending verification has its own public heading");
  assert(!host.querySelector("aside, [data-account-menu]"), "unknown identity mounts no private shell");
  assert(privateReads === 0, "delayed initial metadata starts no private reads");
  assert(pendingMeta.length === 1, "initial verification makes one metadata request");
  await React.act(async () => pendingMeta[0](json({ ...base, operator: false, signed_in: true, session: { ...validSession, id: "delayed-member" } })));
  for (let i = 0; i < 20; i++) await flush();
  assert(rowTrigger(host), "verified delayed member enters the workspace");
  assert(privateReads > 0, "private requests begin only after verification");
  done();
}
console.log("sidebar account checks passed");
}
declare const process: any;
main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
