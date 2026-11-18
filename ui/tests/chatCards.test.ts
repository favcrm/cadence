/** CAD-2443: connect_ref / record_ref parsing and layout, against the shared agenticos-v2 fixtures. */
import { CELL_MAX_CHARS, cellText, parseConnectRef, parseRecordRef, plainText, recordLayout } from "../src/features/home/chatCards";
import { asEntry, mergePage, threadItems, threadRuns } from "../src/features/home/thread";

export {};
declare function require(name: string): any;
declare const process: { cwd(): string };

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

const fixture = (name: string): Record<string, unknown> =>
  JSON.parse(require("fs").readFileSync(`${process.cwd()}/tests/fixtures/chat-cards/${name}.json`, "utf8"));
const entryOf = (raw: Record<string, unknown>, seq = 1) => {
  const e = asEntry({ seq, ...raw });
  assert(e, "entry");
  return e;
};
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v));

// connect_ref
const connect = fixture("connect-ref");
equal(parseConnectRef(entryOf(connect)), { toolkit: "synthetic_mail", label: "Synthetic Mail" }, "connect parses");
for (const [what, mutate] of [
  ["extra payload key (a URL)", (p: any) => { p.url = "https://example.test"; }],
  ["bad version", (p: any) => { p.v = 2; }],
  ["bad toolkit slug", (p: any) => { p.toolkit = "Has Space"; }],
  ["blank label", (p: any) => { p.toolkit_label = " "; }],
  ["long label", (p: any) => { p.toolkit_label = "x".repeat(201); }],
] as const) {
  const bad = clone(connect) as any;
  mutate(bad.payload);
  equal(parseConnectRef(entryOf(bad)), null, `connect rejects ${what}`);
}
equal(parseConnectRef(entryOf({ ...connect, role: "operator" })), null, "connect rejects a non-agent role");
equal(parseConnectRef(entryOf({ ...connect, payload: null })), null, "connect rejects no payload");
equal(parseRecordRef(entryOf(connect)), null, "a connect entry is not a record");

// record_ref list
const list = parseRecordRef(entryOf(fixture("record-ref-list")));
assert(list, "list parses");
equal([list.view, list.truncated, list.tool, list.items.length], ["list", true, "synthetic_crm_list_contacts", 2], "list model");
const listLayout = recordLayout(list);
equal(listLayout, {
  view: "list",
  rows: [
    [{ label: "ID", value: "c_1001" }, { label: "Name", value: "Alex Example" }, { label: "Visits", value: "4" }, { label: "Tags", value: "vip" }],
    [{ label: "ID", value: "c_1002" }, { label: "Name", value: "Sam Sample" }, { label: "Visits", value: "1" }],
  ],
}, "list layout drops empty values");

// record_ref thread
const thread = parseRecordRef(entryOf(fixture("record-ref-thread")));
assert(thread, "thread parses");
equal(recordLayout(thread), {
  view: "thread",
  messages: [
    { sender: "in", time: "2026-11-18T01:00:00Z", text: "Is my order ready?", outbound: false },
    { sender: "out", time: "2026-11-18T01:02:00Z", text: "Yes, ready for pickup.", outbound: true },
  ],
}, "thread layout");
const noText = clone(thread);
noText.fields = noText.fields.filter((f) => f.key !== "text");
equal(recordLayout(noText), null, "a thread with no text field falls back");

// table: key/value of the first item
const table = { ...list, view: "table" as const };
equal(recordLayout(table), { view: "table", pairs: [{ label: "ID", value: "c_1001" }, { label: "Name", value: "Alex Example" }, { label: "Visits", value: "4" }, { label: "Tags", value: "vip" }] }, "table layout");
equal(recordLayout({ ...table, items: [] }), null, "an empty table falls back");

// strict narrowing: any malformed payload is null, never partial
const listFixture = fixture("record-ref-list") as any;
for (const [what, mutate] of [
  ["unknown view", (p: any) => { p.view = "grid"; }],
  ["extra key", (p: any) => { p.href = "https://example.test"; }],
  ["missing truncated", (p: any) => { delete p.truncated; }],
  ["non-boolean untrusted", (p: any) => { p.fields[0].untrusted = "yes"; }],
  ["bad field type", (p: any) => { p.fields[0].type = "html"; }],
  ["bad field key", (p: any) => { p.fields[0].key = "1bad"; }],
  ["object item value", (p: any) => { p.items[0].name = { a: 1 }; }],
  ["non-string array value", (p: any) => { p.items[0].tags = [1]; }],
  ["too many items", (p: any) => { p.items = Array.from({ length: 51 }, () => ({})); }],
  ["too many fields", (p: any) => { p.fields = Array.from({ length: 41 }, (_, i) => ({ key: `k${i}`, label: "", type: "string", untrusted: false })); }],
  ["bad tool", (p: any) => { p.tool = "Bad Tool"; }],
] as const) {
  const bad = clone(listFixture);
  mutate(bad.payload);
  equal(parseRecordRef(entryOf(bad)), null, `record rejects ${what}`);
}

// plain text and caps: no Markdown or link handling, values capped
equal(plainText("[x](https://e.test) **b**"), "[x](https://e.test) **b**", "markup stays literal text");
equal(plainText("a\u0000b\u0007c"), "abc", "control characters dropped");
equal(plainText("x".repeat(CELL_MAX_CHARS + 10)).length, CELL_MAX_CHARS, "cells are capped");
equal(cellText(["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]), "a, b, c, d, e, f, g, h +2", "tags are capped");
equal([cellText(true), cellText(false, { yes: "是", no: "否" }), cellText(null), cellText(undefined)], ["Yes", "否", "", ""], "booleans and empties");

// thread items: the kinds are cards that follow their tool run, like an artifact
const state = mergePage(null, {
  thread: { id: "t" },
  entries: [
    { seq: 1, role: "operator", kind: "message", text: "hi", message: "m" },
    { seq: 2, role: "agent", kind: "tool_call", text: "call", message: "m" },
    { seq: 3, role: "agent", kind: "tool_result", text: "ok", message: "m" },
    { seq: 4, role: "agent", kind: "connect_ref", text: "Connect X", payload: { v: 1 }, message: "m" },
    { seq: 5, role: "agent", kind: "record_ref", text: "rows", payload: { v: 1 }, message: "m" },
    { seq: 6, role: "agent", kind: "turn_result", text: "done", message: "m" },
  ],
});
equal(threadItems(state).map((i) => i.type), ["operator", "tools", "connect", "record", "answer"], "kinds become items after the tool run");
const runs = threadRuns(threadItems(state));
equal(runs.length, 2, "the cards stay inside the assistant turn");

console.log("chat cards checks passed");
