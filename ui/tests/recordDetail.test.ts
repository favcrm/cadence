/** CAD-2543: a `detail` record card (one email): parsing, the compact card, the reader tab and its auto-open. */
export {};
declare function require(name: string): any;
declare const process: { cwd(): string; exit(code: number): never };

// The reader pulls in the icon set (ESM only); a DOM and the icon stubs let the real components be clicked.
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/", settings: { disableIframePageLoading: true } });
for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLButtonElement", "SVGElement", "navigator", "MouseEvent", "Event", "location", "history", "sessionStorage", "localStorage"])
  Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const { createElement } = require("react") as typeof import("react");
const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { renderToStaticMarkup } = require("react-dom/server") as typeof import("react-dom/server");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { canvasSnapshot, closeTab, feedCanvas, openRecordView, resetCanvas } = require("../src/features/canvas/canvasStore") as typeof import("../src/features/canvas/canvasStore");
const { feedParts } = require("../src/features/canvas/feed") as typeof import("../src/features/canvas/feed");
const RecordViewBody = require("../src/features/canvas/RecordViewBody").default as typeof import("../src/features/canvas/RecordViewBody").default;
const { DETAIL_BODY_MAX, inPrompt, parseRecordRef, recordCardView, recordLayout } = require("../src/features/home/chatCards") as typeof import("../src/features/home/chatCards");
const { RecordCard } = require("../src/features/home/RecordCard") as typeof import("../src/features/home/RecordCard");
const { ComposerFillContext } = require("../src/features/home/composerFill") as typeof import("../src/features/home/composerFill");
const { asEntry, mergePage, threadItems } = require("../src/features/home/thread") as typeof import("../src/features/home/thread");
const { composerDraftState, setComposerDraft } = require("../src/features/app-shell/chat/composerStore") as typeof import("../src/features/app-shell/chat/composerStore");

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}
function check(ok: boolean, what: string): void {
  if (!ok) throw new Error(what);
}
const fixture = (name: string): any =>
  JSON.parse(require("fs").readFileSync(`${process.cwd()}/tests/fixtures/chat-cards/${name}.json`, "utf8"));
const entryOf = (raw: Record<string, unknown>, seq = 1) => {
  const e = asEntry({ seq, ...raw });
  if (!e) throw new Error("entry");
  return e;
};
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v));
const html = (node: any) => renderToStaticMarkup(createElement(LocaleProvider, null, node));

// ---- parser: the valid fixture and every invalid case ----
const valid = fixture("record-ref-detail");
const model = parseRecordRef(entryOf(valid));
check(model !== null && model.view === "detail", "the valid detail card parses");
const layout = recordLayout(model!);
check(layout?.view === "detail", "and lays out as a detail");
const detail = (layout as any).detail;
equal([detail.name, detail.address, detail.title, detail.to, detail.attachments, detail.source, detail.account], ["Mei Chan", "mei@harbour.example", "Re: Invoice INV-0140", "ops@example.test", ["INV-0140.pdf"], "Gmail", "ops@example.test"], "detail fields");
equal([detail.star, detail.unread, detail.chips, detail.truncated], [true, true, [], false], "label mapping: IMPORTANT is the star, UNREAD hidden but marks unread");
check(detail.body.startsWith("Hi Chris,\n\n") && detail.time.instant === Date.parse("2026-11-18T09:41:00Z"), "the body keeps its line breaks and the date parses");
for (const bad of fixture("record-ref-detail-invalid")) {
  equal(parseRecordRef(entryOf(bad.entry)), null, `invalid fixture rejected: ${bad.case}`);
}

// ---- the compact card ----
const hostile = `<img src=x onerror=alert(1)> [x](https://e.test/p) **b** ${"A".repeat(3000)}`;
const long = clone(valid);
Object.assign(long.payload.items[0], {
  subject: `${hostile} ${"S".repeat(3000)}`,
  from: `${"N".repeat(2000)} <${"a".repeat(150)}@x.example>`,
  to: `<script>x</script>${"T".repeat(2000)}`,
  body_text: `${hostile}\n${"B".repeat(DETAIL_BODY_MAX - 3500)}`,
  attachment_names: [`<b>${"F".repeat(200 - 7)}</b>`, "b.pdf"],
});
const state = mergePage(null, { thread: { id: "t" }, entries: [{ seq: 1, role: "operator", kind: "message", text: "hi", message: "m" }, { seq: 2, role: "agent", kind: "record_ref", message: "m", ...long }] });
const item: any = threadItems(state).find((i) => i.type === "record");
const card = html(createElement(RecordCard, { entry: item.entry }));
check(card.includes('data-record-view="detail"') && card.includes("recordcard-detail"), "a detail card renders the compact card");
check(card.includes("data-unread") && card.includes("recordcard-star"), "unread and star show");
check(card.includes("2 attachments") && card.includes("data-record-open") && card.includes(">Open<"), "attachment count and Open");
check(!card.includes("<img") && !card.includes("<script") && !card.includes("<a ") && !card.includes("<b>"), "third-party text is escaped, never linked");
check(card.includes("&lt;img") && !card.includes("A".repeat(300)) && !card.includes("N".repeat(301)), "values are plain and capped");
check(!card.includes("Ask for more") && !card.includes("data-record-truncated"), "a detail card has no list footer");
const plain = html(createElement(RecordCard, { entry: entryOf(valid, 5) }));
check(plain.includes("Mei Chan") && plain.includes("mei@harbour.example") && plain.includes("Re: Invoice INV-0140") && plain.includes("1 attachment") && plain.includes("Thanks for the invoice"), "compact card content");
check(!plain.includes("\n\nThanks"), "the preview is one flowed paragraph");
// A malformed detail card is the text line, never a half card.
const bad = clone(valid);
bad.payload.items.push(clone(bad.payload.items[0]));
check(html(createElement(RecordCard, { entry: entryOf(bad) })).includes("chatcard-line"), "malformed detail shows the text line");

// ---- the record tab comes from the stored card ----
const view = recordCardView(entryOf(valid, 7))!;
equal([view.key, view.id, view.title], ["e7", "record:e7", "Re: Invoice INV-0140"], "the tab ref is keyed by the entry");
check(recordCardView(entryOf(fixture("record-ref-list"))) === null, "a list card has no reader");
resetCanvas("c", "k", "home");
openRecordView(view);
openRecordView(view);
equal([canvasSnapshot().tabs.map((t) => [t.id, t.kind]), canvasSnapshot().active, canvasSnapshot().open], [[["record:e7", "record"]], "record:e7", true], "Open adds one tab, opening again focuses it");
closeTab("record:e7");
equal([canvasSnapshot().tabs.length, canvasSnapshot().open], [0, false], "closing the last tab closes the drawer");

// ---- auto-open: a live detail card of an operator turn, once; never when the host turns it off ----
const two = clone(valid);
const items = threadItems(mergePage(null, { thread: { id: "t" }, entries: [
  { seq: 1, role: "operator", kind: "message", text: "read it", message: "m1" },
  { seq: 2, role: "agent", kind: "record_ref", message: "m1", ...valid },
  { seq: 3, role: "agent", kind: "record_ref", message: "m1", ...two },
] }));
resetCanvas("c", "k", "home");
feedCanvas({ conversation: "c", surface: "home", composerKey: "k", ...feedParts(items, 0, "m1") });
equal([canvasSnapshot().tabs.map((t) => t.id), canvasSnapshot().open], [["record:e2"], true], "a live card opens its tab once per turn");
resetCanvas("c", "k", "home");
feedCanvas({ conversation: "c", surface: "home", composerKey: "k", ...feedParts(items, 3, "m1") });
equal([canvasSnapshot().tabs.length, canvasSnapshot().open], [0, false], "history never opens a tab");
resetCanvas("c", "k", "home");
feedCanvas({ conversation: "c", surface: "home", composerKey: "k", autoOpen: false, ...feedParts(items, 0, "m1") });
equal([canvasSnapshot().tabs.length, canvasSnapshot().open], [0, false], "the member chat never auto-opens");

// ---- the reader ----
const read = (ref: any, composerKey: string | null = "k") =>
  html(createElement(RecordViewBody, { tab: canvasTab(ref), view: ref, composerKey, sheet: false, expanded: false, canExpand: true, onExpand() {}, onClose() {} }));
function canvasTab(ref: any): any {
  return { id: ref.id, kind: "record", title: ref.title, record: ref, latest: 1, selected: 1, follow: false, versions: {}, turn: null };
}
const reader = read(view);
check(reader.includes("Mei Chan &lt;mei@harbour.example&gt;") && reader.includes("ops@example.test") && reader.includes("Thanks for the invoice"), "header and body");
check(reader.includes('<li class="rv-file"') && reader.includes("INV-0140.pdf") && !reader.includes("<a ") && !reader.includes("href"), "attachment chips are names, not links");
check(reader.includes("From Gmail · ops@example.test · read-only"), "footer names toolkit, account and read-only");
check(!reader.includes("data-record-truncated"), "no truncation note on a full body");
check(reader.includes("Ask about this") && reader.includes("Draft reply") && !reader.includes("disabled"), "Ask and Draft reply are offered with a composer");
check(read(view, null).includes("disabled"), "no composer, no fill");
const cut = clone(valid);
cut.payload.truncated = true;
cut.payload.items[0].body_text = "Hello…";
check(read(recordCardView(entryOf(cut, 9))!).includes("data-record-truncated"), "a truncated body says so");
const reading = read(recordCardView(entryOf(long, 8))!);
check(!reading.includes("<img") && !reading.includes("<script") && !reading.includes("<a ") && reading.includes("&lt;img"), "hostile reader text is escaped, links are not made");
check(reading.includes("B".repeat(1000)) && !reading.includes("S".repeat(DETAIL_BODY_MAX)), "the body shows in full, within its cap");

// ---- composer fills: one line, capped ----
const nasty = `line one\nline two ${"x".repeat(500)}`;
check(!inPrompt(nasty).includes("\n") && inPrompt(nasty).length <= 120, "a value placed in a prompt is one capped line");

// ---- list rows: Read next to Ask and Reply ----
const mail = fixture("record-ref-list");
mail.payload.fields.push({ key: "snippet", label: "Snippet", type: "string", untrusted: true });
mail.payload.items[0].snippet = "hello";
mail.payload.roles.snippet = "snippet";
const row = (fill: any) => html(createElement(ComposerFillContext.Provider, { value: fill }, createElement(RecordCard, { entry: entryOf(mail) })));
check(row(() => {}).includes("Read") && !row(null).includes("recordcard-acts"), "Read joins the row actions, which need a composer");
const noSnippet = fixture("record-ref-list");
check(!html(createElement(ComposerFillContext.Provider, { value: () => {} }, createElement(RecordCard, { entry: entryOf(noSnippet) }))).includes(">Read<"), "a non-mail list has no Read");

// ---- clicking: Open, Ask, Draft reply and Read fill or open, and send nothing ----
async function main() {
  const click = async (n: Element) => { await React.act(async () => { n.dispatchEvent(new MouseEvent("click", { bubbles: true })); }); };
  const mount = async (node: any) => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    await React.act(async () => { root.render(createElement(LocaleProvider, null, node)); });
    return { host, unmount: async () => { await React.act(async () => { root.unmount(); }); host.remove(); } };
  };
  const button = (host: Element, label: string) => {
    const b = [...host.querySelectorAll("button")].find((x) => (x.textContent ?? "").includes(label));
    if (!b) throw new Error(`no button ${label}`);
    return b;
  };

  // Open on the compact card opens the reader tab from the stored entry.
  resetCanvas("c", "k", "home");
  const card = await mount(createElement(RecordCard, { entry: entryOf(valid, 4) }));
  await click(card.host.querySelector("[data-record-open]")!);
  equal([canvasSnapshot().active, canvasSnapshot().open], ["record:e4", true], "Open opens the reader tab");
  await card.unmount();

  // The reader's Ask and Draft reply append to the composer draft, one line each, and never send.
  const ref = recordCardView(entryOf(long, 8))!;
  const body = (composerKey: string | null, sheet = false, onClose = () => {}) => createElement(RecordViewBody, { tab: canvasTab(ref), view: ref, composerKey, sheet, expanded: false, canExpand: true, onExpand() {}, onClose });
  setComposerDraft("k", "typed already");
  let reader = await mount(body("k"));
  await click(button(reader.host, "Ask about this"));
  const asked = composerDraftState("k").text;
  const lines = asked.split("\n");
  check(lines[0] === "typed already" && lines.length === 2 && lines[1].startsWith("Summarise the email '") && lines[1].length < 300, "Ask appends one capped line to the draft");
  await click(button(reader.host, "Draft reply"));
  const drafted = composerDraftState("k").text.split("\n");
  check(drafted.length === 3 && drafted[2].startsWith("Draft a reply to ") && drafted[2].length < 300, "Draft reply appends one capped line");
  await reader.unmount();
  let closed = 0;
  setComposerDraft("k2", "");
  reader = await mount(body("k2", true, () => { closed++; }));
  await click(button(reader.host, "Draft reply"));
  equal(closed, 1, "on a phone the sheet closes so the composer shows");
  await reader.unmount();

  // A list row's Read fills the composer with the email line.
  const mail = fixture("record-ref-list");
  mail.payload.fields.push({ key: "snippet", label: "Snippet", type: "string", untrusted: true });
  mail.payload.items[0].snippet = "hello";
  mail.payload.items[0].id = `Subject one\ntwo ${"x".repeat(500)}`;
  mail.payload.roles.snippet = "snippet";
  const filled: string[] = [];
  const rows = await mount(createElement(ComposerFillContext.Provider, { value: (text: string) => filled.push(text) }, createElement(RecordCard, { entry: entryOf(mail) })));
  await click(button(rows.host, "Read"));
  check(filled.length === 1 && filled[0].startsWith("Open the email '") && filled[0].includes("' from Alex Example") && !filled[0].includes("\n") && filled[0].length < 300, "Read fills one capped line");
  await rows.unmount();
  console.log("record detail checks passed");
}
main().catch((e) => { console.error(e); process.exit(1); });
