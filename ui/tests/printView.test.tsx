export {};
/**
 * AOS-711 (CAD-2705): the print page the snapshot renderer opens. It draws
 * exactly one app view, read-only, with the viewer's own authority, and sets
 * `window.__printReady` only once the app frame is ready; a request it cannot
 * serve, a viewer who may not read the app and a failed frame never set it.
 *
 * The frame itself is the canvas's own screen outlet (stubbed here: what is
 * asserted is what the page asks it to mount).
 */
declare function require(name: string): any;
declare const process: { exit(code?: number): never };

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
const equal = (a: unknown, e: unknown, why: string) => {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
};

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/" });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "location", "sessionStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
Object.defineProperty(globalThis, "crypto", { value: require("crypto").webcrypto, configurable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

// The real outlet mints a mount and loads a frame; record what the page asks of it instead.
type Mounted = { readOnly?: boolean; route: any; contextId: string; admitted: boolean; installation: { install_id: string }; onNavigation?: (n: unknown) => void };
const mounted: Mounted[] = [];
let outletBehaviour: "ready" | "silent" | "fails" = "ready";
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  if (id.endsWith("workspace-apps/screen/ScreenHost")) {
    const React = originalRequire.call(this, "react");
    return {
      InstalledScreenOutlet: (props: Mounted & { renderFailure: (s: unknown, retry: () => void) => unknown; fallback: unknown }) => {
        React.useEffect(() => {
          mounted.push(props);
          if (outletBehaviour === "ready") props.onNavigation?.(null);
        }, []);
        return outletBehaviour === "fails"
          ? props.renderFailure({ kind: "refused", status: 403, code: "owner_required" }, () => undefined)
          : React.createElement("div", { "data-outlet": true });
      },
    };
  }
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const PrintView = require("../src/features/print/PrintView").default;
const { isPrintPath, parsePrintQuery, printRefusal, printRoute } = require("../src/features/print/printView") as typeof import("../src/features/print/printView");

// ---- the request grammar ----
{
  assert(isPrintPath("/print/view") && !isPrintPath("/print/view/") && !isPrintPath("/print"), "the route is /print/view");
  const spec = parsePrintQuery("?install=inst_acc&screen=accounting&section=pnl&view=pnl&params=compare%3Dprior_year%26from%3D2026-07-01%26to%3D2026-09-30");
  equal(spec, { installId: "inst_acc", tag: "accounting", section: "pnl", view: "pnl", recordId: null, params: { compare: "prior_year", from: "2026-07-01", to: "2026-09-30" } }, "view and params");
  equal(printRoute(spec!), { section: "pnl", view: "list", record_id: null, segment_id: null, params: { compare: "prior_year", from: "2026-07-01", to: "2026-09-30" } }, "one view, as a list route");
  equal(parsePrintQuery("?install=inst_acc&screen=accounting&section=customers&record=cust-0001")?.recordId, "cust-0001", "a record");
  equal(parsePrintQuery("?install=i&screen=a&section=s")?.params, undefined, "params are optional");
  for (const bad of [
    "", "?install=i&screen=a", "?install=i&section=s", "?screen=a&section=s",
    "?install=i&screen=A&section=s", "?install=i&screen=a&section=S", "?install=i&screen=a&section=s&record=A/B",
    "?install=i&install=j&screen=a&section=s", "?install=i&screen=a&section=s&view=a%20b",
    "?install=i&screen=a&section=s&params=Bad%3Dx", "?install=i&screen=a&section=s&params=a%3D1%26a%3D2%26b",
  ]) assert(parsePrintQuery(bad) === null, `refuses "${bad}"`);
  const files = ["screens/accounting/screens.json"];
  assert(printRefusal(spec!, { files, navigation: { sections: [{ id: "pnl" }] } }) === null, "a declared screen and section");
  assert(printRefusal(spec!, { files: ["screens/other/screens.json"] }) === "screen", "another screen is refused, not defaulted");
  assert(printRefusal(spec!, { files, navigation: { sections: [{ id: "cash" }] } }) === "section", "an undeclared section is refused, not defaulted to another");
}

// ---- the board: the installation scope ----
let scope: "ok" | "denied" = "ok";
const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
const installation = { install_id: "inst_acc", name: "accounting", title: "Accounting", digest: "d", files: ["screens/accounting/screens.json"], navigation: { sections: [{ id: "pnl" }], default_section: "pnl", section_query: "acc" } };
globalThis.fetch = (async (input: unknown) => {
  const url = new URL(String(input), "http://localhost");
  if (scope === "denied") return json({ error: { code: "owner_required" } }, 403);
  if (url.pathname === "/api/app-installations/inst_acc") return json(installation);
  if (url.pathname === "/api/app-installations/inst_acc/contexts") return json({ contexts: [{ id: "default", install_id: "inst_acc", state: "active", config: { label: "Default" } }] });
  return json({}, 404);
}) as typeof fetch;

const flush = () => React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
async function settle(check: () => void) {
  for (let i = 0; i < 100; i++) {
    await flush();
    try { check(); return; } catch { /* keep polling */ }
    await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 5)); });
  }
  check();
}
const w = win as unknown as { __printReady?: boolean };
async function page(search: string, settleMs = 0) {
  mounted.length = 0;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await React.act(async () => { root.render(React.createElement(LocaleProvider, null, React.createElement(PrintView, { search, settleMs }))); });
  const state = () => host.querySelector("[data-print-view]")?.getAttribute("data-print-state");
  const done = async () => { await React.act(async () => { root.unmount(); }); host.remove(); };
  return { host, state, done };
}
const REQUEST = "?install=inst_acc&screen=accounting&section=pnl&view=pnl&params=compare%3Dprior_year%26from%3D2026-07-01%26to%3D2026-09-30";

// ---- one read-only view, ready once the frame is ----
{
  outletBehaviour = "ready"; scope = "ok";
  const p = await page(REQUEST);
  await settle(() => assert(w.__printReady === true, "ready is set once the frame reports ready"));
  assert(mounted.length === 1, `exactly one view is mounted (got ${mounted.length})`);
  const m = mounted[0]!;
  assert(m.readOnly === true && m.admitted === true, "read-only, under the viewer's session");
  assert(m.installation.install_id === "inst_acc" && m.contextId === "default", "the install the request names");
  equal(m.route, { section: "pnl", view: "list", record_id: null, segment_id: null, params: { compare: "prior_year", from: "2026-07-01", to: "2026-09-30" } }, "exactly the view and params");
  assert(p.state() === "ready", "the page says so");
  assert(!p.host.querySelector("nav, header, aside"), "no board chrome");
  await p.done();
}
// ---- never ready for what it did not draw ----
async function neverReady(why: string, search: string, behaviour: typeof outletBehaviour, access: typeof scope) {
  outletBehaviour = behaviour; scope = access;
  const p = await page(search);
  await settle(() => assert(p.state() === "refused" || (behaviour === "silent" && mounted.length === 1), `${why}: settled`));
  await React.act(async () => { await new Promise((resolve) => setTimeout(resolve, 30)); });
  assert(w.__printReady === false, `${why}: __printReady stays false`);
  await p.done();
}
await neverReady("an invalid request", "?install=inst_acc", "ready", "ok");
await neverReady("a viewer who may not read the app", REQUEST, "ready", "denied");
await neverReady("another screen than the installation declares", REQUEST.replace("screen=accounting", "screen=other"), "ready", "ok");
await neverReady("a section the installation does not declare", REQUEST.replace("section=pnl", "section=cash"), "ready", "ok");
await neverReady("a frame that was refused", REQUEST, "fails", "ok");
await neverReady("a frame that never reports ready", REQUEST, "silent", "ok");
console.log("printView ok");
}
main().then(() => process.exit(0), (error) => { console.error(error); process.exit(1); });
