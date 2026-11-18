/** CAD-2452: a role-mapped record card through the thread path stays plain, capped text. */
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { LocaleProvider } from "../src/lib/locale";
import { RecordCard } from "../src/features/home/RecordCard";
import { CELL_MAX_CHARS } from "../src/features/home/chatCards";
import { ComposerFillContext } from "../src/features/home/composerFill";
import { mergePage, threadItems } from "../src/features/home/thread";

export {};
declare function require(name: string): any;
declare const process: { cwd(): string };

const fixture = (name: string): any =>
  JSON.parse(require("fs").readFileSync(`${process.cwd()}/tests/fixtures/chat-cards/${name}.json`, "utf8"));
const check = (ok: boolean, what: string): void => {
  if (!ok) throw new Error(what);
};

const hostile = `<img src=x onerror=alert(1)> [x](https://e.test) **b** ${"A".repeat(2000)}`;
const list = fixture("record-ref-list");
list.payload.items[0].name = hostile;
const thread = fixture("record-ref-thread");
thread.payload.items[0].text = hostile;

const state = mergePage(null, {
  thread: { id: "t" },
  entries: [
    { seq: 1, role: "operator", kind: "message", text: "hi", message: "m" },
    { seq: 2, role: "agent", kind: "record_ref", message: "m", ...list },
    { seq: 3, role: "agent", kind: "record_ref", message: "m", ...thread },
  ],
});
const records = threadItems(state).filter((i) => i.type === "record");
check(records.length === 2, "both cards come through threadItems");
for (const item of records) {
  const html = renderToStaticMarkup(
    createElement(LocaleProvider, null, createElement(RecordCard, { entry: (item as any).entry })),
  );
  check(!html.includes("<img"), "untrusted markup is escaped");
  check(!html.includes("<a "), "no link is rendered");
  check(html.includes("&lt;img"), "the markup shows as text");
  check(!html.includes("A".repeat(CELL_MAX_CHARS + 1)), "the long string is capped");
}

// CAD-2458: a mail-like list through the same path: header, split sender, labels, unread, row actions.
const mail = fixture("record-ref-list");
const long = (c: string) => `${c.repeat(2000)}<script>x</script>`;
mail.payload.fields.push({ key: "snippet", label: "Snippet", type: "string", untrusted: true });
mail.payload.items = [
  { id: long("S"), name: `${long("N")} <${"a".repeat(300)}@x.example>`, snippet: long("P"), visits: 1, tags: ["new", long("T"), "internal"] },
];
mail.payload.roles = { ...mail.payload.roles, snippet: "snippet", time: "id" };
const mailState = mergePage(null, {
  thread: { id: "t" },
  entries: [{ seq: 1, role: "agent", kind: "record_ref", message: "m", ...mail }],
});
const mailItem: any = threadItems(mailState).find((i) => i.type === "record");
const draw = (fill: ((text: string) => void) | null) =>
  renderToStaticMarkup(
    createElement(
      LocaleProvider,
      null,
      createElement(ComposerFillContext.Provider, { value: fill }, createElement(RecordCard, { entry: mailItem.entry })),
    ),
  );
const plain = draw(null);
const acted = draw(() => {});
check(plain.includes("Synthetic CRM") && plain.includes("List contacts") && plain.includes("Main CRM"), "header names toolkit, tool and account");
check(plain.includes("1 of 120"), "count pill reads N of TOTAL");
check(plain.includes("shown as plain text"), "the plain-text note is the header tooltip");
check(!plain.includes("recordcard-acts") && acted.includes("recordcard-acts"), "row actions need a composer to fill");
check(acted.includes("Ask about this") && acted.includes("Reply"), "ask and reply actions");
check(plain.includes("data-unread"), "the unread row is marked");
check(plain.includes("data-record-truncated") && !plain.includes("Ask for more") && acted.includes("Ask for more"), "truncated footer; ask-for-more fills the composer");
check(!plain.includes("<script") && !acted.includes("<script"), "hostile markup in any role is escaped");
check(!plain.includes("N".repeat(CELL_MAX_CHARS + 1)) && !plain.includes("P".repeat(CELL_MAX_CHARS + 1)) && !plain.includes("T".repeat(61)), "every role is capped");

console.log("record card render checks passed");
