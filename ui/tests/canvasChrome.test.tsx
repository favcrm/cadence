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
const { declaredOptionLabel, optionsLine } = require("../src/features/canvas/viewParams") as typeof import("../src/features/canvas/viewParams");
const { ViewCard } = require("../src/features/home/ViewCard") as typeof import("../src/features/home/ViewCard");
const { asEntry } = require("../src/features/home/thread") as typeof import("../src/features/home/thread");

const settle = () => React.act(async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); });
const el = (root: ParentNode, sel: string) => root.querySelector(sel) as HTMLElement | null;
const all = (root: ParentNode, sel: string) => Array.from(root.querySelectorAll(sel)) as HTMLElement[];
const click = async (n: HTMLElement) => { await React.act(async () => { n.dispatchEvent(new MouseEvent("click", { bubbles: true })); }); await settle(); };
const esc = async () => { await React.act(async () => { (document.activeElement ?? document.body).dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })); }); await settle(); };

// ---- options in words ----
const zh = (english: string) => ({ "as at {date}": "截至 {date}", "Prior year": "去年同期", "Monthly": "按月" } as Record<string, string>)[english] ?? english;
const same = (english: string) => english;
equal(optionsLine({ from: "2026-07-01", to: "2026-09-30", compare: "prior_year", columns: "monthly" }, "en", same), "1 Jul – 30 Sep 2026 · Prior year · Monthly", "a range, then humanized values");
equal(optionsLine({ as_at: "2026-09-30" }, "en", same), "as at 30 Sep 2026", "a single date reads as at");
equal(optionsLine({ from: "2026-07-01", to: "2026-09-30", compare: "prior_year", columns: "monthly" }, "zh-TW", zh), "7月1日 – 2026年9月30日 · 去年同期 · 按月", "zh-TW goes through the catalogue");
equal(optionsLine({ as_at: "2026-09-30" }, "zh-TW", zh), "截至 2026年9月30日", "zh-TW as at");
equal(optionsLine({ from: "2026-07-01", to: "2027-02-03" }, "en", same), "1 Jul 2026 – 3 Feb 2027", "a range across years names both");
equal(optionsLine({ q: "vip" }, "en", same), "Vip", "a value is sentence case");
equal(optionsLine({ basis: "cash", as_at: "2026-02-30" }, "en", same), "Cash · 2026-02-30", "an impossible day is a plain value, in order");
equal(optionsLine({ period: "Q3 2026", by: "by month" }, "en", same), "Q3 2026 · By month", "a free value only gets a capital");
equal(optionsLine(undefined, "en", same), "", "no options, no line");

// ---- CAD-2710: the app's own words for option values ----
const declared = [{ id: "pnl", params: { compare: { labels: { prior_year: { en: "vs prior year", "zh-TW": "對比去年" } } }, columns: { labels: { monthly: { zh: "按月", en: "by month" } } } } }];
const pnl = { from: "2026-07-01", to: "2026-09-30", compare: "prior_year", columns: "monthly" };
equal(optionsLine(pnl, "en", same, declaredOptionLabel(declared, "pnl", "en")), "1 Jul – 30 Sep 2026 · vs prior year · by month", "declared labels replace the humanized values");
equal(optionsLine(pnl, "zh-TW", zh, declaredOptionLabel(declared, "pnl", "zh-TW")), "7月1日 – 2026年9月30日 · 對比去年 · 按月", "exact locale, then the language prefix");
equal(optionsLine({ compare: "prior_year" }, "fr", same, declaredOptionLabel(declared, "pnl", "fr")), "vs prior year", "then en");
equal(optionsLine({ compare: "prior_year", columns: "weekly" }, "en", same, declaredOptionLabel(declared, "pnl", "en")), "vs prior year · Weekly", "a value without a label is humanized");
equal(optionsLine({ columns: "monthly" }, "fr", same, declaredOptionLabel([{ id: "pnl", params: { columns: { labels: { monthly: { "zh-TW": "按月" } } } } }], "pnl", "fr")), "Monthly", "no usable locale falls back to humanize");
for (const lookup of [declaredOptionLabel(declared, "other", "en"), declaredOptionLabel(declared, undefined, "en"), declaredOptionLabel(null, "pnl", "en"), declaredOptionLabel([{ id: "pnl" }], "pnl", "en")])
  equal(optionsLine(pnl, "en", same, lookup), "1 Jul – 30 Sep 2026 · Prior year · Monthly", "an unlabelled declaration changes nothing");
equal(declaredOptionLabel([{ id: "pnl", params: { compare: { labels: {} } } }], "pnl", "en")?.("compare", "constructor"), undefined, "an inherited key is not a label");

// ---- the header, composed ----
const render = (props: Record<string, unknown>) => renderToStaticMarkup(React.createElement(LocaleProvider, null, React.createElement(ViewHeader, {
  mark: React.createElement("span", { className: "cv-mark" }, "A"),
  back: null,
  title: "Profit & Loss",
  appName: "Accounting",
  options: "",
  note: "Read-only here — open in Accounting to edit",
  open: { label: "Open in Accounting", href: "/apps/accounting", onOpen: () => {} },
  expand: { expanded: false, enabled: true, onToggle: () => {} },
  onClose: () => {},
  ...props,
})));
const one = render({ options: "1 Jul – 30 Sep 2026 · Prior year" });
assert(one.includes("Profit &amp; Loss") && one.includes("data-view-options") && one.includes("<b") === true, "the row names the view and the line carries the options");
assert(one.includes('title="Accounting · 1 Jul – 30 Sep 2026 · Prior year · Read-only"'), "the line keeps its full text in a title");
assert(one.includes("Read-only") && one.includes('role="tooltip"') && one.includes("Read-only here — open in Accounting to edit") && !one.includes("cv-lock"), "the word replaces the lock and keeps the note as its tooltip");
assert(!one.includes("Shown as") && !one.includes("Live"), "no status words outside a deviation");
const tabbed = render({ title: null, options: "Prior year", tabRow: undefined });
assert(!tabbed.includes("cv-vtitle") && tabbed.includes("Prior year"), "with no title the header keeps the line and actions");
const docked = render({ title: null, mark: null, tabRow: null });
assert(docked.includes("data-docked") && !docked.includes("cv-vrow") && !docked.includes("cv-open") && docked.includes("Accounting"), "docked, the header is the line alone; its actions wait for the tab bar");
const noAccess = render({ note: null, open: null });
const noLink = render({ open: null });
assert(noLink.includes("data-read-only") && !noLink.includes("cv-open"), "an app view that cannot link out is still read-only");
assert(!noAccess.includes("data-read-only") && !noAccess.includes("Read-only") && !noAccess.includes("cv-open"), "without access there is no Read-only and no Open in");
const sheet = render({ back: { onBack: () => {}, expanded: false }, mark: null, expand: null });
assert(sheet.includes('aria-label="Back to chat"') && !sheet.includes("cv-mark") && !sheet.includes('aria-label="Expand canvas"') && !sheet.includes('aria-label="Close canvas"'), "the phone sheet: back chevron for the mark, no expand, no separate close");
const expandedHead = render({ back: { onBack: () => {}, expanded: true }, expand: { expanded: true, enabled: true, onToggle: () => {} } });
assert(expandedHead.includes("<kbd>Esc</kbd>") && !expandedHead.includes("cv-mark") && expandedHead.includes('aria-label="Collapse canvas"') && expandedHead.includes('aria-label="Close canvas"'), "expanded: Back to chat with Esc leads, the mark yields, collapse and close stay");
assert((expandedHead.match(/<header/g) ?? []).length === 1, "one header row");

// ---- the narrow rules, as written ----
const css = require("fs").readFileSync(`${process.cwd()}/src/features/canvas/canvas.css`, "utf8") as string;
assert(/@container cv-main \(max-width: 640px\) \{[^}]*\.cv-open-label \{ display: none; \}/.test(css), "up to 640px Open in goes icon-only");
assert(/@container cv-main \(max-width: 480px\)[^]*\.cv-wide \{ display: none; \}[^]*\.cv-fold \{ display: inline-flex; \}/.test(css), "below 480px the actions fold into the menu");
assert(/\.cv-meta-text \{[^}]*overflow: hidden;[^}]*text-overflow: ellipsis;[^}]*white-space: nowrap;/.test(css) && /\.cv-ro \{[^}]*flex: none;/.test(css), "the options line ends in an ellipsis and Read-only never shrinks");
assert(/\.cv-vtitle \{[^}]*flex: 0 1 auto;[^}]*overflow: hidden;[^}]*text-overflow: ellipsis;/.test(css), "the title is what truncates");
const longTitle = render({ title: "Quarterly profit and loss by cost centre, compared with prior year", options: "Q3 2026 · Prior year · Monthly" });
assert(longTitle.includes('class="cv-vtitle" title="Quarterly profit and loss by cost centre, compared with prior year"'), "a long title keeps its full text in a title");

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
equal(el(hostEl, ".cv-meta-text")?.textContent, "Accounting · Q3 2026 · Vs prior year · By month", "the line names the app and the options");
equal(el(hostEl, "[data-read-only]")?.firstChild?.textContent, "Read-only", "Read-only is a word, not a lock");
assert(!el(hostEl, ".cv-lock") && !el(hostEl, ".cv-tabactions"), "one view: no tab bar, the header keeps its own actions");
assert(!el(hostEl, ".cv-appview-sub") && !hostEl.textContent!.includes("read-only ·"), "the mono app line is gone");

// Two views: the strip shows, the header drops the title.
await React.act(async () => { canvasStore.openAppView(ref("b")); });
await settle();
equal(all(hostEl, '.cv-tabs [role="tab"]').length, 2, "two views show the strip");
assert(!el(hostEl, ".cv-vtitle") && el(hostEl, ".cv-tab .cv-mark"), "the tab carries the mark and the header drops the title");
equal(all(hostEl, "header").length, 1, "still one header row");
assert(!el(hostEl, ".cv-vhead .cv-mark") && !el(hostEl, ".cv-vhead .cv-vrow") && el(hostEl, ".cv-vhead[data-docked] .cv-meta"), "the second row drops the app icon and is the line alone");
assert(el(hostEl, ".cv-tabrow .cv-tabactions [data-expand]") && el(hostEl, ".cv-tabrow .cv-tabactions .cv-open") && el(hostEl, '.cv-tabrow .cv-tabactions [aria-label="Close canvas"]'), "Open in, expand and close sit at the right of the tab bar");
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
await React.act(async () => { await new Promise((r) => requestAnimationFrame(() => r(null))); });
assert(document.activeElement === el(hostEl, "[data-expand]"), "after Back to chat, focus lands on the expand control");
await click(el(hostEl, '[aria-label="Expand canvas"]')!);
await click(el(hostEl, '[aria-label="Collapse canvas"]')!);
assert(!chrome.canvasChrome().full, "the collapse icon restores the split");
// Closing while expanded gives the board back.
await click(el(hostEl, '[aria-label="Expand canvas"]')!);
await click(el(hostEl, '[aria-label="Close canvas"]')!);
assert(!chrome.canvasChrome().full && !document.documentElement.hasAttribute("data-canvas-full"), "closing an expanded canvas restores the board");

// CAD-2710: the header and the view card read the installed declaration's labels.
const installationBody = (views: unknown) => JSON.stringify({
  install_id: "inst_acct", name: "accounting", title: "Accounting", state: "active", removed: null, digest: "d", files: [],
  navigation: { section_query: "acc", default_section: "reports", sections: [{ id: "reports", label: "Reports" }] }, views,
});
const serve = (views: unknown) => { globalThis.fetch = (async (url: string) => new Response(String(url).endsWith("/contexts") ? '{"contexts":[]}' : installationBody(views), { status: 200 })) as typeof fetch; };
const labelled = [{ id: "pnl", params: { compare: { labels: { prior_year: { en: "vs prior year", "zh-TW": "對比去年" } } }, columns: { labels: { monthly: { en: "by month" } } } } }];
const labelRef = (record: string, installId: string) => ref(record, { installId, viewId: "pnl", route: { section: "reports", view: "list", record_id: record, segment_id: null, params: { from: "2026-07-01", to: "2026-09-30", compare: "prior_year", columns: "monthly", basis: "cash" } } });
const cardEntry = (installId: string) => asEntry({ seq: 1, role: "agent", kind: "view_ref", text: "Opened", payload: { v: 1, app: "accounting", app_label: "Accounting", install_id: installId, screen_tag: "accounting", section: "reports", record_id: "pnl", title: "Profit and loss", view: "pnl", view_label: "Profit and loss", params: { from: "2026-07-01", to: "2026-09-30", compare: "prior_year", columns: "monthly", basis: "cash" } } })!;
// A fresh install id per case: the app scope read is cached per install.
const cardRoot = createRoot(document.body.appendChild(document.createElement("div")));
for (const [i, [views, line]] of ([[labelled, "1 Jul – 30 Sep 2026 · vs prior year · by month · Cash"], [[{ id: "pnl", params: { compare: {} } }], "1 Jul – 30 Sep 2026 · Prior year · Monthly · Cash"], [null, "1 Jul – 30 Sep 2026 · Prior year · Monthly · Cash"]] as const).entries()) {
  const installId = `inst_lbl${i}`;
  serve(views);
  canvasStore.resetCanvas("c1", "k", "home");
  await React.act(async () => { canvasStore.openAppView(labelRef("l", installId)); });
  await settle();
  equal(el(hostEl, ".cv-meta-text")?.textContent, `Accounting · ${line}`, "the header line uses the declared labels, humanizing the rest");
  await React.act(async () => { cardRoot.render(React.createElement(LocaleProvider, null, React.createElement(ViewCard, { entry: cardEntry(installId) }))); });
  await settle();
  equal(document.querySelector("[data-view-params]")?.textContent, line, "the view card line uses the declared labels, humanizing the rest");
}
await React.act(async () => { cardRoot.unmount(); });
canvasStore.resetCanvas("c1", "k", "home");

await React.act(async () => { root.unmount(); });
console.log("canvas chrome tests passed");
}
main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
