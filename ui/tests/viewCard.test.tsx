/** CAD-2475: view_ref parsing (agenticos-v2 fixtures), the card, its auto-open, and the canvas tab fixes. */
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { LocaleProvider } from "../src/lib/locale";
import { appViewId, appViewRef, appViewTitle } from "../src/features/canvas/appView";
import { canvasSnapshot, closeTab, feedCanvas, openAppView, patchAppViewRoute, resetCanvas } from "../src/features/canvas/canvasStore";
import { feedParts } from "../src/features/canvas/feed";
import { HOME_VIEW_SCOPE, parseViewRef, viewCardAppView, viewHere } from "../src/features/home/chatCards";
import { asEntry, mergePage, threadItems } from "../src/features/home/thread";
import { ViewCard, ViewScopeContext, viewState } from "../src/features/home/ViewCard";

export {};
declare function require(name: string): any;
declare const process: { cwd(): string };

const read = (name: string): any =>
  JSON.parse(require("fs").readFileSync(`${process.cwd()}/tests/fixtures/chat-cards/${name}.json`, "utf8"));
const check = (ok: boolean, what: string): void => {
  if (!ok) throw new Error(what);
};
const equal = (actual: unknown, expected: unknown, what: string): void => {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
};
const entryOf = (raw: Record<string, unknown>, extra: Record<string, unknown> = {}) => {
  const e = asEntry({ seq: 1, ...raw, ...extra });
  check(e !== null, "entry");
  return e!;
};

// Parser: the valid fixture, then every invalid case.
const valid = read("view-ref");
const model = parseViewRef(entryOf(valid));
equal(model, { app: "crm", appLabel: "CRM", installId: "install_crm", screenTag: "crm", section: "customers", recordId: "cust-0001", title: "Ada Example" }, "valid fixture");
const noRecord = JSON.parse(JSON.stringify(valid));
delete noRecord.payload.record_id;
equal(parseViewRef(entryOf(noRecord))?.recordId, null, "record_id may be absent");
noRecord.payload.record_id = null;
equal(parseViewRef(entryOf(noRecord))?.recordId, null, "record_id may be null");
const invalid = read("view-ref-invalid") as { case: string; entry: Record<string, unknown> }[];
// Every case but one is rejected by the parser. "unknown entry key" is the exception, by design:
// the thread wire (`asEntry`) keeps role, kind, text and payload only, for every card kind, so an
// extra entry key never reaches a parser or the page. It is asserted on its own below.
for (const { case: name, entry } of invalid) {
  if (name === "unknown entry key") continue;
  equal(parseViewRef(entryOf(entry)), null, `invalid fixture is rejected: ${name}`);
}
const withExtraKey = invalid.find((c) => c.case === "unknown entry key")!.entry;
check("html" in withExtraKey && !("html" in entryOf(withExtraKey)), "the wire drops the unknown entry key");
check(!renderToStaticMarkup(createElement(LocaleProvider, null, createElement(ViewCard, { entry: entryOf(withExtraKey) }))).includes("<b>x</b>"), "the dropped key is never rendered");
for (const [what, mutate] of [
  ["operator role", (e: any) => { e.role = "operator"; }],
  ["control character in the title", (e: any) => { e.payload.title = "a\u0007b"; }],
  ["empty title", (e: any) => { e.payload.title = ""; }],
  ["app with a slash", (e: any) => { e.payload.app = "crm/x"; }],
] as const) {
  const bad = JSON.parse(JSON.stringify(valid));
  mutate(bad);
  equal(parseViewRef(entryOf(bad)), null, `rejects ${what}`);
}

// The card opens the canvas tab: Home uses the default context, the app pane its own for its install.
const home = viewCardAppView(model!);
equal([home?.installId, home?.contextId, home?.route, home?.title, home?.appLabel, home?.fromCard], ["install_crm", "default", { section: "customers", view: "list", record_id: "cust-0001", segment_id: null }, "Ada Example", "CRM", true], "home ref");
equal(viewCardAppView(model!, { installId: "install_crm", contextId: "ctx_7" })?.contextId, "ctx_7", "pane context");
equal(viewCardAppView(model!, { installId: "other", contextId: "ctx_7" })?.contextId, "default", "another install keeps the default");
equal(viewCardAppView({ ...model!, screenTag: "Crm_Mount" }), null, "a tag the frame refuses is not openable");

// Card states.
equal([viewState("ok", true, "showing"), viewState("ok", true, "closed"), viewState("denied", true, "showing"), viewState("missing", true, "closed"), viewState("ok", false, "closed"), viewState("ok", true, "moved"), viewState("ok", true, "here"), viewState("denied", true, "here")], ["showing", "closed", "denied", "missing", "missing", "moved", "here", "denied"], "states");
// CAD-2512: the same-app decision is the install match and a pane that can move its page.
const showHere = () => {};
check(viewHere(model!, { installId: "install_crm", contextId: "c", showHere }) && !viewHere(model!, { installId: "other", contextId: "c", showHere }) && !viewHere(model!, { installId: "install_crm", contextId: "c" }) && !viewHere(model!, HOME_VIEW_SCOPE), "same app, with a page to move");

// Render: hostile title stays plain text, truncation hooks are present, a bad payload is the text line.
resetCanvas("c", "k", "home");
const hostile = `<img src=x onerror=alert(1)> ${"W".repeat(120 - 30)}`.slice(0, 120);
const hostileEntry = JSON.parse(JSON.stringify(valid));
hostileEntry.payload.title = hostile;
hostileEntry.payload.app_label = `<b>${"L".repeat(54)}`.slice(0, 60);
const render = (raw: any) => renderToStaticMarkup(createElement(LocaleProvider, null, createElement(ViewCard, { entry: entryOf(raw) })));
const html = render(hostileEntry);
check(!html.includes("<img") && html.includes("&lt;img"), "title markup is escaped");
check(!html.includes("<b>"), "app label markup is escaped");
check(html.includes("viewcard-title") && html.includes("viewcard-place") && html.includes("data-view-open"), "title and place truncate; closed card has the Open button");
check(render(valid).includes("data-view-state=\"closed\""), "closed state");
const bad = JSON.parse(JSON.stringify(valid));
bad.payload.url = "https://example.test";
check(!render(bad).includes("data-view-open") && render(bad).includes("chatcard-line"), "malformed payload is the text line");
const showing = appViewRef({ ...home, title: home!.title });
openAppView(showing);
check(render(valid).includes("data-view-state=\"showing\"") && !render(valid).includes("data-view-open"), "showing state has no Open button");

// The thread path: view_ref is a card item of its turn.
const state = mergePage(null, {
  thread: { id: "t" },
  entries: [
    { seq: 1, role: "operator", kind: "message", text: "show Ada", message: "m1" },
    { seq: 2, role: "agent", kind: "view_ref", message: "m1", ...valid },
    { seq: 3, role: "operator", kind: "message", text: "again", message: "m2" },
    { seq: 4, role: "agent", kind: "view_ref", message: "m2", ...valid },
  ],
});
const items = threadItems(state);
check(items.filter((i) => i.type === "view").length === 2, "view_ref becomes a view item");

// Auto-open: history never opens; a live card of an operator turn opens once per turn; a close sticks for the turn; no auto-open on the member chat.
const live = (autoOpen: boolean | undefined, liveAfter: number, from: number) => {
  const parts = feedParts(items.slice(0, from), liveAfter, null);
  feedCanvas({ conversation: "c", surface: "home", composerKey: "k", autoOpen, ...parts });
};
resetCanvas("c", "k", "home");
live(undefined, 4, 2);
equal([canvasSnapshot().tabs.length, canvasSnapshot().open], [0, false], "history card opens nothing");
resetCanvas("c", "k", "home");
live(false, 0, 2);
equal([canvasSnapshot().tabs.length, canvasSnapshot().open], [0, false], "member chat never auto-opens");
resetCanvas("c", "k", "home");
live(undefined, 0, 2);
const id = appViewId(home!);
equal([canvasSnapshot().tabs.map((t) => t.id), canvasSnapshot().active, canvasSnapshot().open], [[id], id, true], "live card of an operator turn opens its tab");
closeTab(id);
live(undefined, 0, 2);
equal(canvasSnapshot().open, false, "an idempotent feed pass does not reopen a closed tab");
// A second card in the same turn does not open another tab; the next turn's card does.
resetCanvas("c", "k", "home");
const other = viewCardAppView({ ...model!, recordId: "cust-0002" })!;
const card = (key: string, ref: typeof other, turn: string) => ({ key, ref, live: true, operatorTurn: true, turn });
feedCanvas({ conversation: "c", surface: "home", composerKey: "k", cards: [], working: null, turn: "m1", views: [card("a", home!, "m1"), card("b", other, "m1")] });
equal(canvasSnapshot().tabs.map((t) => t.id), [id], "one auto-open per operator turn");
feedCanvas({ conversation: "c", surface: "home", composerKey: "k", cards: [], working: null, turn: "m2", views: [card("a", home!, "m1"), card("b", other, "m1"), card("c", other, "m2")] });
equal([canvasSnapshot().tabs.length, canvasSnapshot().active], [2, appViewId(other)], "the next operator turn opens again");

// D4: the tab title follows in-frame navigation.
resetCanvas("c", "k", "app");
openAppView(home);
const tabId = appViewId(home!);
patchAppViewRoute(tabId, { record_id: "cust-0002", segment_id: null });
equal(canvasSnapshot().tabs[0].title, "cust-0002", "title follows to the record now shown");
patchAppViewRoute(tabId, { record_id: "cust-0001", segment_id: null });
equal(canvasSnapshot().tabs[0].title, "Ada Example", "back on the opened record, the opened title");
patchAppViewRoute(tabId, { record_id: null, segment_id: null });
equal(canvasSnapshot().tabs[0].title, "customers", "a list shows the section");
equal(appViewTitle(tabId, "Ada Example", home!.route), "Ada Example", "unchanged route keeps the title");

// CAD-2512: the card reopens its own route; a tab that moved away reads Opened.
resetCanvas("c", "k", "app");
openAppView(home);
patchAppViewRoute(tabId, { record_id: "cust-0002", segment_id: null });
check(render(valid).includes("data-view-state=\"moved\"") && render(valid).includes("Opened") && render(valid).includes("data-view-open"), "a moved tab reads Opened, with the Open button");
openAppView(home);
equal([canvasSnapshot().tabs[0].app?.route.record_id, canvasSnapshot().tabs[0].title], ["cust-0001", "Ada Example"], "the card resets the tab to its own record");
check(render(valid).includes("data-view-state=\"showing\""), "back on its record the card reads Open");

// CAD-2512: in the app's own pane the card moves the live page and opens no canvas tab.
const moves: unknown[] = [];
const pane = { installId: "install_crm", contextId: "ctx_7", showHere: (route: unknown) => moves.push(route) };
const hereHtml = renderToStaticMarkup(createElement(LocaleProvider, null, createElement(ViewScopeContext.Provider, { value: pane }, createElement(ViewCard, { entry: entryOf(valid) }))));
check(hereHtml.includes("data-view-state=\"here\"") && hereHtml.includes("Shown in CRM") && hereHtml.includes("data-view-here") && !hereHtml.includes("data-view-open"), "same-app card reads Shown in <App>");
resetCanvas("c", "k", "app");
const herePart = feedParts(items.slice(0, 2), 0, null, pane);
equal(herePart.views.map((v) => v.here), [true], "same-app card is marked here");
feedCanvas({ conversation: "c", surface: "app", composerKey: "k", showHere: pane.showHere, ...herePart });
equal([moves, canvasSnapshot().tabs.length, canvasSnapshot().open], [[{ section: "customers", view: "list", record_id: "cust-0001", segment_id: null }], 0, false], "live same-app card moves the page, no tab");
resetCanvas("c", "k", "app");
const otherPart = feedParts(items.slice(0, 2), 0, null, { installId: "other", contextId: "c", showHere: pane.showHere });
equal(otherPart.views.map((v) => v.here), [undefined], "another app's card keeps the canvas");

// CAD-2555: a declared view with params (agenticos-v2 AOS-681 fixtures, copied verbatim).
resetCanvas("c", "k", "app");
const withParams = read("view-ref-params");
const paramsModel = parseViewRef(entryOf(withParams));
equal(paramsModel, { app: "books", appLabel: "Books", installId: "install_books", screenTag: "books", section: "reports", recordId: "pnl", title: "Profit and loss", view: "pnl", viewLabel: "Profit and loss", params: { from: "2026-01-01", to: "2026-03-31" } }, "view-ref-params fixture parses");
equal(parseViewRef(entryOf(valid)), model, "a card without the new keys keeps its exact model");
for (const { case: name, entry } of read("view-ref-params-invalid") as { case: string; entry: Record<string, unknown> }[]) {
  equal(parseViewRef(entryOf(entry)), null, `invalid params fixture is rejected: ${name}`);
}
const viewOnly = JSON.parse(JSON.stringify(withParams));
delete viewOnly.payload.params;
delete viewOnly.payload.view_label;
equal([parseViewRef(entryOf(viewOnly))?.view, parseViewRef(entryOf(viewOnly))?.params], ["pnl", undefined], "view alone is enough");
const emptyParams = JSON.parse(JSON.stringify(withParams));
emptyParams.payload.params = {};
equal(parseViewRef(entryOf(emptyParams))?.params, {}, "an empty params object is accepted");
const paramsRef = viewCardAppView(paramsModel!);
equal([paramsRef?.route, paramsRef?.viewLabel], [{ section: "reports", view: "list", record_id: "pnl", segment_id: null, params: { from: "2026-01-01", to: "2026-03-31" } }, "Profit and loss"], "the card's route and label carry the view");
const paramsHtml = render(withParams);
check(paramsHtml.includes("Books · Profit and loss") && paramsHtml.includes("data-view-params") && paramsHtml.includes("1 Jan – 31 Mar 2026"), "the card shows the view label and a compact params line");
check(!render(valid).includes("data-view-params"), "a card without params has no params line");
// The tab opens with the params in its route, and the same view with other params resets it.
openAppView(paramsRef);
const paramsTab = appViewId(paramsRef!);
equal(canvasSnapshot().tabs[0].app?.route.params, { from: "2026-01-01", to: "2026-03-31" }, "the canvas route carries the params");
patchAppViewRoute(paramsTab, { params: { from: "2026-04-01", to: "2026-06-30" } });
equal(canvasSnapshot().tabs[0].app?.route.params, { from: "2026-04-01", to: "2026-06-30" }, "a patch replaces the params");
equal(canvasSnapshot().tabs[0].app?.route.section, "reports", "and keeps the rest of the route");
patchAppViewRoute(paramsTab, { record_id: "pnl", segment_id: null });
equal(canvasSnapshot().tabs[0].app?.route.params, { from: "2026-04-01", to: "2026-06-30" }, "a patch without params keeps them");
check(render(withParams).includes("data-view-state=\"moved\""), "a tab whose params moved reads Opened");
openAppView(paramsRef);
equal(canvasSnapshot().tabs[0].app?.route.params, { from: "2026-01-01", to: "2026-03-31" }, "the card resets the tab's params");
patchAppViewRoute(paramsTab, { params: {} });
check(canvasSnapshot().tabs[0].app?.route.params === undefined, "empty params clear them");

// CAD-2634: the params line wraps to two lines with the full text in a title; the app line stays one line.
const cardParamsHtml = render(withParams);
check(cardParamsHtml.includes("viewcard-params") && /data-view-params="true" title="[^"]+"/.test(cardParamsHtml), "the card's params line carries its full text as a title");
const readCss = (path: string): string => require("fs").readFileSync(`${process.cwd()}/src/${path}`, "utf8");
const rule = (css: string, selector: string): string => css.slice(css.indexOf(selector)).split("}")[0];
check(rule(readCss("styles.css"), ".viewcard-params {").includes("-webkit-line-clamp: 2"), "card params clamp to two lines");
check(rule(readCss("features/canvas/canvas.css"), ".cv-title > .cv-appview-params {").includes("-webkit-line-clamp: 2"), "tab params clamp to two lines");
check(rule(readCss("features/canvas/canvas.css"), ".cv-title > .cv-appview-sub {").includes("white-space: nowrap"), "the tab's app line stays on one line");
