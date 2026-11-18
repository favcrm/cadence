/** CAD-2452: a role-mapped record card through the thread path stays plain, capped text. */
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { LocaleProvider } from "../src/lib/locale";
import { RecordCard } from "../src/features/home/RecordCard";
import { CELL_MAX_CHARS } from "../src/features/home/chatCards";
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
console.log("record card render checks passed");
