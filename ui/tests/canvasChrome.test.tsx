/**
 * CAD-2657: the app view's one header row, no footer, the tab strip only for
 * two or more views, and the full-width expand (sidebar rail, chat rail and
 * peek, Esc order, no remount of the app frame's pane). The real drawer is
 * mounted in happy-dom. happy-dom does not evaluate container queries, so the
 * narrow rules are checked as structure (the "+N" chip and its title) and as
 * CSS text.
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
// The app scope cannot be read here: the body shows its failure note, which is all this test needs of it.
globalThis.fetch = (async () => new Response("{}", { status: 500 })) as typeof fetch;

const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");
const CanvasDrawer = require("../src/features/canvas/CanvasDrawer").default;
const ViewHeader = require("../src/features/canvas/ViewHeader").default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const canvasStore = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");
const chrome = require("../src/features/canvas/canvasChrome") as typeof import("../src/features/canvas/canvasChrome");
const { paramChips, paramsLine } = require("../src/features/canvas/viewParams") as typeof import("../src/features/canvas/viewParams");

const settle = () => React.act(async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); });
const el = (root: ParentNode, sel: string) => root.querySelector(sel) as HTMLElement | null;
const all = (root: ParentNode, sel: string) => Array.from(root.querySelectorAll(sel)) as HTMLElement[];
const click = async (n: HTMLElement) => { await React.act(async () => { n.dispatchEvent(new MouseEvent("click", { bubbles: true })); }); await settle(); };
const esc = async () => { await React.act(async () => { (document.activeElement ?? document.body).dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })); }); await settle(); };

// ---- params as chips ----
equal(paramChips({ from: "2026-07-01", to: "2026-09-30" }, "en"), ["1 Jul – 30 Sep 2026"], "two days read as one range chip");
equal(paramChips({ q: "vip" }, "en"), ["vip"], "a value is a chip");
equal(paramChips({ period: "Q3 2026", compare: "vs prior year", by: "by month" }, "en"), ["Q3 2026", "vs prior year", "by month"], "one chip per param, in order");
equal(paramChips(undefined, "en"), [], "no params, no chips");
equal(paramsLine({ period: "Q3 2026", by: "by month" }, "en"), "Q3 2026 · by month", "the card's line is the chips on one line");

// ---- the header, composed ----
const render = (props: Record<string, unknown>) => renderToStaticMarkup(React.createElement(LocaleProvider, null, React.createElement(ViewHeader, {
  mark: React.createElement("span", { className: "cv-mark" }, "A"),
  back: null,
  title: "Profit & Loss",
  chips: [],
  note: "Read-only here — open in Accounting to edit",
  open: { label: "Open in Accounting", href: "/apps/accounting", onOpen: () => {} },
  expand: { expanded: false, enabled: true, onToggle: () => {} },
  onClose: () => {},
  ...props,
})));
const one = render({ chips: ["vip"] });
assert(one.includes("Profit &amp; Loss") && one.includes("data-view-params") && !one.includes("data-chips-more"), "one chip: title, the chip, no +N");
assert(one.includes('aria-label="Read-only here — open in Accounting to edit"') && one.includes('role="tooltip"'), "the lock carries the read-only note as its name and tooltip");
assert(!one.includes("Shown as") && !one.includes("Live"), "no status words outside a deviation");
const many = render({ chips: ["Q3 2026", "vs prior year", "by month"] });
assert(many.includes("+2") && many.includes('title="vs prior year · by month"'), "three chips: the first stays, +2 carries the rest in its title");
assert(many.includes('title="Q3 2026 · vs prior year · by month"'), "the chips' container carries every param");
const tabbed = render({ title: null, chips: ["vip"] });
assert(!tabbed.includes("cv-vtitle") && tabbed.includes("vip"), "with a tab strip the header drops the title and keeps chips and actions");
const noAccess = render({ note: null, open: null });
const noLink = render({ open: null });
assert(noLink.includes("cv-lock") && !noLink.includes("cv-open"), "an app view that cannot link out is still read-only");
assert(!noAccess.includes("cv-lock") && !noAccess.includes("cv-open"), "without access there is no lock and no Open in");
const sheet = render({ back: { onBack: () => {}, expanded: false }, mark: null, expand: null });
assert(sheet.includes('aria-label="Back to chat"') && !sheet.includes("cv-mark") && !sheet.includes('aria-label="Expand canvas"') && !sheet.includes('aria-label="Close canvas"'), "the phone sheet: back chevron for the mark, no expand, no separate close");
const expandedHead = render({ back: { onBack: () => {}, expanded: true }, expand: { expanded: true, enabled: true, onToggle: () => {} } });
assert(expandedHead.includes("<kbd>Esc</kbd>") && !expandedHead.includes("cv-mark") && expandedHead.includes('aria-label="Collapse canvas"') && expandedHead.includes('aria-label="Close canvas"'), "expanded: Back to chat with Esc leads, the mark yields, collapse and close stay");
assert((expandedHead.match(/<header/g) ?? []).length === 1, "one header row");

// ---- the narrow rules, as written ----
const css = require("fs").readFileSync(`${process.cwd()}/src/features/canvas/canvas.css`, "utf8") as string;
assert(/@container cv-main \(max-width: 560px\) \{ \.cv-open-label \{ display: none; \} \}/.test(css), "the Open in label goes first");
assert(/@container cv-main \(max-width: 480px\)[^]*\.cv-wide \{ display: none; \}[^]*\.cv-fold \{ display: inline-flex; \}[^]*\.cv-chip-more \{ display: inline-block; \}/.test(css), "below 480px the actions fold into the menu and the chips into +N");

// ---- the drawer ----
(win as any).innerWidth = 1400;
const ref = (record: string, over: Record<string, unknown> = {}) => ({
  installId: "inst_acct", tag: "accounting", contextId: "ctx_1", appLabel: "Accounting",
  route: { section: "reports", view: "list", record_id: record, segment_id: null, params: { period: "Q3 2026", compare: "vs prior year", by: "by month" } },
  title: `Report ${record}`, href: `/apps/accounting?install=inst_acct&ctx=ctx_1&acc=reports&record=${record}`, ...over,
});
canvasStore.resetCanvas("c1", "k", "home");
const hostEl = document.createElement("div");
document.body.append(hostEl);
const root = createRoot(hostEl);
await React.act(async () => {
  root.render(React.createElement(LocaleProvider, null, React.createElement("div", { "data-canvas-main": "" }, React.createElement("main", { className: "home-workspace" }, React.createElement("textarea", { id: "chat-box" }))), React.createElement(CanvasDrawer)));
});
await React.act(async () => { canvasStore.openAppView(ref("a")); });
await settle();
const drawer = () => el(hostEl, "[data-canvas-drawer]")!;
const body = () => el(hostEl, "[data-app-view]");
assert(drawer() && body(), "the app view opens in the drawer");
assert(!el(hostEl, ".cv-foot") && !el(hostEl, "footer"), "the footer is gone");
assert(!el(hostEl, ".cv-tabs"), "no tab strip for one view");
equal(all(hostEl, "header").length, 1, "one header row");
equal(el(hostEl, ".cv-vtitle")?.textContent, "Report a", "one view: the header names it");
equal(el(hostEl, "[data-chips-more]")?.textContent, "+2", "three params: +2 after the first chip");
assert(!el(hostEl, ".cv-appview-sub") && !hostEl.textContent!.includes("read-only ·"), "the mono app line is gone");

// Two views: the strip shows, the header drops the title.
await React.act(async () => { canvasStore.openAppView(ref("b")); });
await settle();
equal(all(hostEl, '.cv-tabs [role="tab"]').length, 2, "two views show the strip");
assert(!el(hostEl, ".cv-vtitle") && el(hostEl, ".cv-tab .cv-mark"), "the tab carries the mark and the header drops the title");
equal(all(hostEl, "header").length, 1, "still one header row");
await React.act(async () => { canvasStore.closeTab(canvasStore.canvasSnapshot().tabs[1].id); });
await settle();
assert(!el(hostEl, ".cv-tabs") && el(hostEl, ".cv-vtitle"), "back to one view: no strip, the title returns");

// Expand: the pane stays mounted (the app frame is inside it and never reloads); the board's chrome steps aside.
const paneBefore = body();
assert(!chrome.canvasChrome().full && !document.documentElement.hasAttribute("data-canvas-full"), "not expanded at first");
await click(el(hostEl, '[aria-label="Expand canvas"]')!);
assert(chrome.canvasChrome().full && document.documentElement.hasAttribute("data-canvas-full"), "expanded: the sidebar folds (App.tsx reads this) and the chat steps aside");
assert(body() === paneBefore, "no remount of the app view on expand");
assert(el(hostEl, ".cv-rail") && el(hostEl, ".cv-rail-chat"), "the chat keeps a rail");
assert(!el(hostEl, ".cv-rail-chat .cv-dot"), "no unread dot yet");
assert(el(hostEl, ".cv-back[data-expanded]")?.textContent?.includes("Back to chat") && !el(hostEl, ".cv-vhead .cv-mark"), "Back to chat leads the header");

// An assistant reply while the chat is out of sight lights the rail.
await React.act(async () => { chrome.noteChatActivity(); });
assert(el(hostEl, ".cv-rail-chat .cv-dot"), "unread dot on the rail");
await click(el(hostEl, ".cv-rail-chat")!);
assert(document.documentElement.hasAttribute("data-canvas-peek") && !el(hostEl, ".cv-dot"), "the rail opens the chat peek and reads it");
assert(body() === paneBefore, "no remount on peek");
// A reply that lands while the peek is open is already in view.
await React.act(async () => { chrome.noteChatActivity(); });
assert(!el(hostEl, ".cv-dot"), "no dot while the chat is in view");

// Esc: a focused field blurs first, then the peek closes, then the split returns. Nothing else changes in between.
const field = el(hostEl, "#chat-box") as HTMLTextAreaElement;
await React.act(async () => { field.focus(); });
assert(document.activeElement === field, "the field has focus");
await esc();
assert(document.activeElement !== field && chrome.canvasChrome().peek && chrome.canvasChrome().full, "first Esc leaves the field only");
await esc();
assert(!chrome.canvasChrome().peek && chrome.canvasChrome().full, "second Esc closes the peek");
await esc();
assert(!chrome.canvasChrome().full && !document.documentElement.hasAttribute("data-canvas-full") && !el(hostEl, ".cv-rail"), "third Esc leaves expand");
assert(body() === paneBefore && drawer(), "no remount on collapse; the canvas stays open");
await esc();
assert(!el(hostEl, "[data-canvas-drawer]"), "Esc with nothing expanded closes the canvas, as before");

// Back to chat and the collapse icon are the same exit.
await React.act(async () => { canvasStore.openAppView(ref("a")); });
await settle();
await click(el(hostEl, '[aria-label="Expand canvas"]')!);
await click(el(hostEl, ".cv-back[data-expanded]")!);
assert(!chrome.canvasChrome().full && el(hostEl, "[data-canvas-drawer]"), "Back to chat restores the split");
await click(el(hostEl, '[aria-label="Expand canvas"]')!);
await click(el(hostEl, '[aria-label="Collapse canvas"]')!);
assert(!chrome.canvasChrome().full, "the collapse icon restores the split");
// Closing while expanded gives the board back.
await click(el(hostEl, '[aria-label="Expand canvas"]')!);
await click(el(hostEl, '[aria-label="Close canvas"]')!);
assert(!chrome.canvasChrome().full && !document.documentElement.hasAttribute("data-canvas-full"), "closing an expanded canvas restores the board");

await React.act(async () => { root.unmount(); });
console.log("canvas chrome tests passed");
}
main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
