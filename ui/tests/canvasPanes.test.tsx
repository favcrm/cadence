/**
 * CAD-2635: several canvas views side by side. The arrangement rules (split,
 * stack, grid, close to tab, the fourth pane, placement, persistence) are
 * checked on the store and the pure layout module; the stage, the divider
 * keys, the narrow fallback and the no-reload move are checked on the real
 * drawer in happy-dom. The `placement` key is checked against the contract
 * fixtures agenticos-v2 copies.
 */
declare function require(name: string): any;
declare const process: any;
export {};
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}
function equal(a: unknown, e: unknown, why: string) {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}

async function main() {
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/", settings: { disableIframePageLoading: true } });
for (const name of ["window", "document", "location", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLButtonElement", "HTMLTextAreaElement", "HTMLSelectElement", "HTMLIFrameElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "KeyboardEvent", "MouseEvent", "PointerEvent", "Event", "CustomEvent", "DOMRect", "localStorage", "CSS"])
  if (win[name] !== undefined) Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
for (const name of ["addEventListener", "removeEventListener", "dispatchEvent", "requestAnimationFrame", "cancelAnimationFrame", "getSelection"])
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
globalThis.fetch = (async () => new Response("{}", { status: 500 })) as typeof fetch;

const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");
const CanvasDrawer = require("../src/features/canvas/CanvasDrawer").default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const store = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");
const layoutLib = require("../src/features/canvas/canvasLayout") as typeof import("../src/features/canvas/canvasLayout");
const { appViewId, appViewRef } = require("../src/features/canvas/appView") as typeof import("../src/features/canvas/appView");
const { parseViewRef, viewCardAppView } = require("../src/features/home/chatCards") as typeof import("../src/features/home/chatCards");
const { asEntry } = require("../src/features/home/thread") as typeof import("../src/features/home/thread");
const { ViewCard } = require("../src/features/home/ViewCard") as typeof import("../src/features/home/ViewCard");
const { feedParts } = require("../src/features/canvas/feed") as typeof import("../src/features/canvas/feed");

const fixture = (name: string): any => JSON.parse(require("fs").readFileSync(`${process.cwd()}/tests/fixtures/chat-cards/${name}.json`, "utf8"));
const raw = (n: string) => ({
  installId: "inst_crm", tag: "crm", contextId: "ctx_1", appLabel: "CRM",
  route: { section: "customers", view: "list", record_id: n, segment_id: null },
  title: `Customer ${n}`, href: `/apps/crm?install=inst_crm&ctx=ctx_1&crm=customers&record=${n}`,
});
const id = (n: string) => appViewId(appViewRef(raw(n))!);
const snap = () => store.canvasSnapshot();
const eff = () => layoutLib.effective(snap().layout, snap().tabs.map((t) => t.id));
const open = (...names: string[]) => names.forEach((n) => store.openAppView(raw(n)));
const fv = (n: string, placement?: any, turn = "t1") => ({ key: `view:${n}:${turn}`, ref: appViewRef(raw(n))!, ...(placement ? { placement } : {}), live: true, operatorTurn: true, turn });
const feed = (views: any[]) => store.feedCanvas({ conversation: "c1", surface: "home", composerKey: "k", cards: [], views, working: null, turn: null });
// A fresh canvas has nothing saved, unless the test is about what was saved.
const fresh = (conversation = "c1", keepSaved = false) => { if (!keepSaved) layoutLib.clearSavedLayouts(); store.resetCanvas(conversation, "k", "home"); };

// ---- the layout module ----
const L = layoutLib;
const ids = ["a", "b", "c", "d", "e"];
let r = L.addPane(L.TABS_LAYOUT, ids, "b", "a", "split");
equal([r.placed, r.layout.mode, r.layout.panes], [true, "split", ["a", "b"]], "beside: two panes, split");
equal(L.addPane(L.TABS_LAYOUT, ids, "b", "a", "stack").layout.mode, "stack", "below: stacked");
equal(L.addPane(L.TABS_LAYOUT, ids, "b", "a", "split", true).layout.panes, ["b", "a"], "left drop goes before the anchor");
r = L.addPane(r.layout, ids, "c", "b", "split");
equal([r.layout.mode, r.layout.panes], ["grid", ["a", "b", "c"]], "three panes are a grid, after the anchor");
r = L.addPane(r.layout, ids, "d", "a", "split");
equal(r.layout.panes, ["a", "d", "b", "c"], "four panes, inserted after the anchor");
equal(L.addPane(r.layout, ids, "e", "a", "split").placed, false, "a fifth is not placed");
equal(L.removePane(r.layout, ids, "d").panes, ["a", "b", "c"], "a pane's close returns it to a tab");
equal(L.removePane(L.removePane(r.layout, ids, "d"), ids, "c").mode, "split", "back to two: side by side");
equal(L.removePane(L.removePane(L.removePane(r.layout, ids, "d"), ids, "c"), ids, "b"), { ...L.TABS_LAYOUT }, "one left is the tab layout");
equal(L.effective({ ...r.layout, panes: ["a", "zz"] }, ids), { mode: "tabs", panes: [] }, "a saved pane whose tab is not open yet is no arrangement");
equal([L.cellFor("grid", 3, 2), L.dividersFor("grid", 3).vertical], [{ col: [1, 4], row: [3, 4] }, [1, 2]], "three panes: the third spans the bottom");
equal(L.placementOf("beside"), "beside", "known placement");
equal([L.placementOf("floating"), L.placementOf(3)], [undefined, undefined], "unknown placement is ignored");
equal(L.nudgeShare(0.5, -24, 1000), 0.476, "a divider nudge is the grip's 24px");
equal([L.nudgeShare(0.25, -96, 1000), L.nudgeShare(0.75, 500, 1000)], [0.2, 0.8], "a divider stays between 20% and 80%");

// ---- the contract fixtures (agenticos-v2 copies view-ref-placement.json) ----
const entry = (name: string) => { const e = asEntry({ seq: 1, ...fixture(name) }); assert(e, name); return e!; };
equal(parseViewRef(entry("view-ref-placement"))?.placement, "beside", "placement parses");
const unknown = parseViewRef(entry("view-ref-placement-unknown"));
assert(unknown && unknown.placement === undefined && unknown.title === "Ada Example", "an unknown placement is ignored: the card still opens");
equal(parseViewRef(entry("view-ref"))?.placement, undefined, "absent placement: today's model");
const extra = fixture("view-ref-placement");
extra.payload.url = "https://example.test/x";
equal(parseViewRef(asEntry({ seq: 1, ...extra })!), null, "other unknown keys are still refused");

// ---- the store ----
fresh();
open("a", "b", "c", "d", "e");
equal([snap().active, eff().mode], [id("e"), "tabs"], "tabs by default");
store.focusTab(id("a"));
store.setLayoutMode("split");
equal([eff().mode, eff().panes], ["split", [id("a"), id("b")]], "Side by side: the focused view and the next one");
store.setLayoutMode("grid");
equal([eff().mode, eff().panes.length, snap().active], ["grid", 4, id("a")], "Grid takes up to four");
store.setLayoutMode("stack");
equal([eff().mode, eff().panes], ["stack", [id("a"), id("b")]], "Stacked keeps two");
store.setLayoutMode("tabs");
equal([eff().mode, snap().tabs.length], ["tabs", 5], "Tabs returns every pane to the strip");

// A pane's close returns the view to a tab; closing the tab closes the view.
store.setLayoutMode("grid");
store.unpane(id("c"));
equal([eff().panes.length, snap().tabs.length], [3, 5], "x on a pane: the view stays a tab");
store.closeTab(id("b"));
equal([eff().panes, snap().tabs.some((t) => t.id === id("b"))], [[id("a"), id("d")], false], "closing a pane's tab closes the view and the pane");
store.focusTab(id("d"));
store.unpane(id("d"));
equal([eff().mode, snap().active, snap().tabs.length], ["tabs", id("a"), 4], "the last pane left becomes the shown view");

// Dropping a tab on the stage: left, right, bottom, or in place of the focused view.
fresh();
open("a", "b", "c", "d");
store.focusTab(id("a"));
equal(store.dropTab(id("a"), "right"), false, "a view cannot be dropped on itself");
equal([store.dropTab(id("b"), "left"), eff().panes, snap().active], [true, [id("b"), id("a")], id("b")], "drop left: before the focused view, and focused");
store.dropTab(id("c"), "bottom");
equal([eff().mode, eff().panes], ["grid", [id("b"), id("c"), id("a")]], "a third drop makes the grid");
store.focusTab(id("a"));
store.dropTab(id("d"), "replace");
equal([eff().panes, snap().tabs.length], [[id("b"), id("c"), id("d")], 4], "replace: the focused pane goes back to a tab");
// A tab clicked in the strip while panes show takes the focused pane's place; a pane's tab focuses its pane.
store.focusTab(id("c"));
equal(snap().active, id("c"), "a paned tab focuses its pane");
store.focusTab(id("a"));
equal([eff().panes, snap().active], [[id("b"), id("a"), id("d")], id("a")], "an unpaned tab replaces the focused pane");

// Placement: beside / below anchor to the focused pane, else the last opened; four panes: a background tab.
fresh();
feed([fv("a")]);
equal([snap().tabs.length, eff().mode], [1, "tabs"], "no placement: one auto-open, as before");
feed([fv("b", "beside"), fv("c", "below")]);
equal([eff().mode, eff().panes, snap().active], ["grid", [id("a"), id("b"), id("c")], id("c")], "beside and below in one turn both open (an explicit placement is not the turn's one auto-open), and the newest is focused");
feed([fv("x")]);
equal(snap().tabs.some((t) => t.id === id("x")), false, "a later card without placement in the turn still waits");
store.focusTab(id("a"));
feed([fv("d", "beside", "t2")]);
equal(eff().panes, [id("a"), id("d"), id("b"), id("c")], "beside anchors to the focused pane");
const before = snap().layout;
feed([fv("e", "beside", "t3")]);
equal([snap().layout === before, eff().panes.length, snap().tabs.some((t) => t.id === id("e")), snap().active], [true, 4, true, id("d")], "four panes: it opens as a background tab, the arrangement and focus stay");
fresh();
open("a", "b");
store.focusTab(id("a"));
feed([fv("c", "beside")]);
equal(eff().panes, [id("a"), id("c")], "beside on a tab layout pairs it with the shown view");
fresh();
open("a", "b", "c");
store.setLayoutMode("split");
const focusedBefore = snap().active;
feed([fv("d", "tab", "t9")]);
equal([eff().panes.length, snap().active, snap().tabs.some((t) => t.id === id("d"))], [2, focusedBefore, true], "an explicit tab placement leaves the arrangement alone");
// The card says where it went.
const cardEntry = (n: string, placement?: string) => asEntry({ seq: 1, role: "agent", kind: "view_ref", text: "Opened", payload: { v: 1, app: "crm", app_label: "CRM", install_id: "inst_crm", screen_tag: "crm", section: "customers", record_id: n, title: `Customer ${n}`, ...(placement ? { placement } : {}) } })!;
const card = (n: string, placement?: string) => renderToStaticMarkup(React.createElement(LocaleProvider, null, React.createElement(ViewCard, { entry: cardEntry(n, placement) })));
// The same cards, as the thread would feed them (a card's own scope opens its view in the default context).
const fc = (n: string, placement?: string, turn = "t1") => ({ key: `view:${n}:${turn}`, ref: viewCardAppView(parseViewRef(cardEntry(n, placement))!)!, ...(placement ? { placement } : {}), live: true, operatorTurn: true, turn });
fresh();
feed([fc("a")]);
feed([fc("b", "beside")]);
assert(card("b", "beside").includes('data-view-placed="pane"'), "the card says it opened in a pane");
assert(!card("a").includes("data-view-placed"), "a card with no placement says nothing");
assert(/Opened beside [^<]+</.test(card("b", "beside")), "CAD-2682: the card names the view it opened beside");
assert(!card("b", "beside").includes("Opened in a pane"), "CAD-2682: the generic wording is only the fallback");
// CAD-2682: the polish that is CSS only, checked on the rules themselves.
const css = (f: string) => require("fs").readFileSync(`${process.cwd()}/src/${f}`, "utf8") as string;
const canvasCss = css("features/canvas/canvas.css");
assert(/\.cv-divider:focus-visible \{ outline: 2px solid var\(--color-accent\)/.test(canvasCss), "a focused divider has the standard focus ring");
assert(/\.cv-zone b \{[^}]*background: var\(--color-ink-900\)/.test(canvasCss), "a drop-zone label has a solid backing");
assert(/\.cv-tab\[data-kind="session"\] \{ position: sticky; right: 0;/.test(canvasCss) && /\.cv-tabrow \{[^}]*flex-wrap: wrap/.test(canvasCss), "the Browser tab stays whole and the strip never overlaps the layout buttons");
assert(/@container composer-foot \(max-width: 22rem\) \{\s*\.composer-hint \{ display: none; \}/.test(css("styles.css")), "a narrow composer hides its hint");
feed([fc("c", "beside"), fc("d", "beside")]);
feed([fc("e", "beside", "t4")]);
assert(card("e", "beside").includes('data-view-placed="full"'), "with four panes the card says it opened as a background tab");
// feedParts carries the placement from the card.
const parts = feedParts([{ type: "view", key: "k1", entry: cardEntry("p", "below") } as any], 0, null);
equal(parts.views.map((v) => v.placement), ["below"], "the feed carries placement");

// Persistence: per conversation, the arrangement waits for its tabs.
fresh("c1");
open("a", "b", "c");
store.focusTab(id("a"));
store.setLayoutMode("split");
const saved = snap().layout;
fresh("c2", true);
equal([eff().mode, snap().layout.mode], ["tabs", "tabs"], "another conversation has its own layout");
fresh("c1", true);
equal([snap().layout.panes, eff().mode], [saved.panes, "tabs"], "the saved arrangement is back, waiting for its tabs");
store.openAppView(raw("a"));
equal(eff().mode, "tabs", "one of two tabs open: still tabs");
store.openAppView(raw("b"));
equal([eff().mode, eff().panes, snap().active], ["split", [id("a"), id("b")], id("b")], "both tabs back: the arrangement is restored");
store.setPaneShare("cols", 0.3);
fresh("c1", true);
equal(snap().layout.cols, 0.3, "a divider's share is saved");
store.setPaneShare("cols", 0.4, false);
fresh("c1", true);
equal(snap().layout.cols, 0.3, "a drag in progress is not saved until it ends");
globalThis.localStorage.setItem("cadence.canvas.layout.c9", "{not json");
equal(layoutLib.readLayout("c9"), layoutLib.TABS_LAYOUT, "a malformed saved layout is the tab layout");
layoutLib.clearSavedLayouts();
equal(layoutLib.readLayout("c1"), layoutLib.TABS_LAYOUT, "a confirmed identity change clears what was saved");

// ---- the real drawer ----
const click = async (n: HTMLElement) => { await React.act(async () => { n.dispatchEvent(new MouseEvent("click", { bubbles: true })); }); await settle(); };
const settle = () => React.act(async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); });
const el = (root: ParentNode, sel: string) => root.querySelector(sel) as HTMLElement | null;
const all = (root: ParentNode, sel: string) => Array.from(root.querySelectorAll(sel)) as HTMLElement[];
const setWidth = async (w: number) => { (win as any).innerWidth = w; await React.act(async () => { window.dispatchEvent(new Event("resize")); }); await settle(); };
(win as any).innerWidth = 1700;
fresh("c1");
const hostEl = document.createElement("div");
document.body.append(hostEl);
const root = createRoot(hostEl);
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null, React.createElement("div", { "data-canvas-main": "" }, React.createElement("main", { className: "home-workspace" }, React.createElement("textarea", { id: "chat-box" }))), React.createElement(CanvasDrawer)));
});
await React.act(async () => { open("a", "b", "c"); });
await settle();
await React.act(async () => { store.focusTab(id("a")); });
await settle();
const pane = (n: string) => el(hostEl, `[data-pane="${id(n)}"]`);
const mode = (m: string) => el(hostEl, `[data-layout-mode="${m}"]`)!;
assert(pane("a") && !el(hostEl, ".cv-panes[data-panes]"), "tabs: one pane on stage, no divider");
equal(all(hostEl, "[data-layout-mode]").map((b) => (b as HTMLButtonElement).disabled), [false, false, false, false], "the switcher is on in a wide canvas");
const paneA = pane("a")!;
const bodyA = el(paneA, "[data-app-view]")!;
await click(mode("split"));
const stage = el(hostEl, ".cv-panes")!;
equal([stage.getAttribute("data-layout"), all(hostEl, ".cv-pane").length], ["split", 2], "Side by side: two panes");
assert(pane("a") === paneA && paneA.isConnected && el(paneA, "[data-app-view]") === bodyA, "tab to pane: the view's DOM (its frame) is the same node, so it does not reload");
assert(el(hostEl, '[role="separator"][data-axis="cols"]') && !el(hostEl, '[data-axis="rows"]'), "one vertical divider");
equal(all(hostEl, "[data-view-header]").length, 2, "each pane has its own one-row header");
assert(el(pane("a")!, ".cv-vtitle")?.textContent === "Customer a", "a pane's header names its view");

// Divider keys: 24px, 96px with Shift, within 20-80%.
Object.defineProperty(stage, "getBoundingClientRect", { value: () => ({ left: 0, top: 0, width: 1000, height: 600, right: 1000, bottom: 600 }), configurable: true });
const divider = () => el(hostEl, '[data-axis="cols"]')!;
const key = async (n: HTMLElement, k: string, shiftKey = false) => { await React.act(async () => { n.dispatchEvent(new KeyboardEvent("keydown", { key: k, shiftKey, bubbles: true, cancelable: true })); }); };
equal(divider().getAttribute("aria-valuenow"), "50", "starts at 50%");
await key(divider(), "ArrowRight");
equal(snap().layout.cols, 0.524, "ArrowRight moves the divider 24px");
await key(divider(), "ArrowLeft", true);
equal(snap().layout.cols, 0.428, "Shift+ArrowLeft moves it 96px");
for (let i = 0; i < 5; i++) await key(divider(), "ArrowLeft", true);
equal([snap().layout.cols, divider().getAttribute("aria-valuenow")], [0.2, "20"], "it stops at 20%");
await key(divider(), "ArrowUp");
equal(snap().layout.cols, 0.2, "other keys do nothing");
assert(stage.style.gridTemplateColumns.includes("20fr") && stage.style.gridTemplateColumns.includes("80fr"), "the stage follows the share");

// Grid, then a pane's close and "Move to tab".
await click(mode("grid"));
equal(all(hostEl, ".cv-pane").length, 3, "Grid with three tabs: three panes");
assert(el(hostEl, '[data-axis="rows"]'), "a horizontal divider appears");
await click(el(pane("c")!, '[aria-label="Close pane"]')!);
equal([stage.getAttribute("data-layout"), all(hostEl, ".cv-pane").length, snap().tabs.length], ["split", 2, 3], "a pane's close: two panes, three tabs");
assert(pane("a") === paneA && el(paneA, "[data-app-view]") === bodyA, "closing another pane leaves this one untouched");
await click(el(pane("a")!, ".cv-fold button")!);
const moveItem = all(document.body, '[role="menuitem"]').find((n) => n.textContent === "Move to tab");
assert(moveItem, "Move to tab is in the pane's menu");
await click(moveItem!);
equal([all(hostEl, ".cv-pane").length, eff().mode], [1, "tabs"], "Move to tab: back to tabs");

// Narrow: tabs with a "N panes saved" chip, the switcher off; wide again restores it.
await click(mode("split"));
equal(all(hostEl, ".cv-pane").length, 2, "split again");
await setWidth(800);
equal(all(hostEl, ".cv-pane").length, 1, "narrow: the active tab alone");
assert(el(hostEl, "[data-panes-saved]")?.textContent?.includes("2 panes saved"), "the chip says the panes are saved");
assert(all(hostEl, "[data-layout-mode]").every((b) => (b as HTMLButtonElement).disabled), "narrow: the switcher is disabled");
assert(eff().mode === "split", "the arrangement is kept");
assert(el(hostEl, "[data-panes-saved]")!.tagName === "SPAN", "800px: even expanded the canvas is too narrow, so the chip is only a note");
// 1000px: expanded the canvas is wide enough, so the chip offers it.
await setWidth(1000);
assert(all(hostEl, ".cv-pane").length === 1 && all(hostEl, "[data-layout-mode]").every((b) => (b as HTMLButtonElement).disabled), "1000px: still tabs, switcher off");
await click(el(hostEl, "button[data-panes-saved]")!);
equal(all(hostEl, ".cv-pane").length, 2, "the chip expands the canvas and the panes return");
await click(el(hostEl, '[aria-label="Collapse canvas"]')!);
equal([all(hostEl, ".cv-pane").length, !!el(hostEl, "[data-panes-saved]")], [1, true], "collapsed: tabs and the chip again");
await setWidth(500);
assert(all(hostEl, ".cv-pane").length === 1 && el(hostEl, "[data-panes-saved]"), "a phone sheet is tabs too");
await setWidth(1700);
equal([all(hostEl, ".cv-pane").length, !!el(hostEl, "[data-panes-saved]")], [2, false], "wide again: the arrangement comes back");

// Expand applies to the whole multi-pane canvas.
await click(el(hostEl, '[aria-label="Expand canvas"]')!);
assert(el(hostEl, "[data-canvas-drawer]")?.hasAttribute("data-full") && all(hostEl, ".cv-pane").length === 2, "expanded: the whole canvas, panes included");
await click(el(hostEl, '[aria-label="Collapse canvas"]')!);

// A drag lights the drop zones above the frames; a drop on one places the tab.
const tabEl = all(hostEl, ".cv-tab").find((t) => t.textContent?.includes("Customer c"))!;
await React.act(async () => { tabEl.dispatchEvent(Object.assign(new Event("dragstart", { bubbles: true }), { dataTransfer: { setData() {}, effectAllowed: "" } })); });
assert(el(hostEl, "[data-zones]") && all(hostEl, "[data-zone]").length === 4, "four drop zones while a tab is dragged");
await React.act(async () => { el(hostEl, '[data-zone="bottom"]')!.dispatchEvent(Object.assign(new Event("drop", { bubbles: true, cancelable: true }), { dataTransfer: {} })); });
assert(!el(hostEl, "[data-zones]"), "the zones go once the tab is dropped");
equal([eff().mode, eff().panes.length], ["grid", 3], "dropped below: a third pane");

// Laptop widths: saved panes never push the canvas over the chat for nothing.
await setWidth(1400);
const dr = () => el(hostEl, "[data-canvas-drawer]")!;
equal([dr().getAttribute("data-mode"), dr().style.width], ["push", "770px"], "1400px with saved panes: the canvas stays as wide as it was, still pushing");
assert(all(hostEl, ".cv-pane").length === 1 && el(hostEl, "[data-panes-saved]") && all(hostEl, "[data-layout-mode]").every((b) => (b as HTMLButtonElement).disabled), "1400px: tabs, the chip, the switcher off");
await setWidth(1440);
equal([dr().getAttribute("data-mode"), dr().style.width, all(hostEl, ".cv-pane").length], ["push", "900px", 3], "1440px: the canvas asks for 900px and the saved panes show");
await click(mode("tabs"));
equal([dr().style.width, all(hostEl, ".cv-pane").length], ["792px", 1], "back to tabs: the canvas returns to its width");
// Drag to split works at the default width: the drop widens the canvas.
const dragTab = all(hostEl, ".cv-tab").find((t) => t.textContent?.includes("Customer b"))!;
assert(dragTab.getAttribute("draggable") === "true", "a tab can be dragged at the default width");
await React.act(async () => { dragTab.dispatchEvent(Object.assign(new Event("dragstart", { bubbles: true }), { dataTransfer: { setData() {}, effectAllowed: "" } })); });
await React.act(async () => { el(hostEl, '[data-zone="right"]')!.dispatchEvent(Object.assign(new Event("drop", { bubbles: true, cancelable: true }), { dataTransfer: {} })); });
await settle();
equal([dr().style.width, all(hostEl, ".cv-pane").length], ["900px", 2], "a drop makes two panes and the canvas widens to show them");

await React.act(async () => { root.unmount(); });
console.log("canvas panes tests passed");
}
main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
