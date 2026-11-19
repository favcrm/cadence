/** CAD-2443: connect_ref / record_ref parsing and layout, against the shared agenticos-v2 fixtures. */
import { CELL_MAX_CHARS, cellText, dayKey, parseConnectRef, parseInstant, parseRecordRef, plainText, recordLayout, splitAddress } from "../src/features/home/chatCards";
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
equal([list.toolLabel, list.toolkitLabel, list.account, list.total], ["List contacts", "Synthetic CRM", "Main CRM", 120], "header data");
equal(recordLayout(list), {
  view: "role-list",
  rows: [
    { title: "Alex Example", name: "Alex Example", address: "", subtitle: "c_1001", snippet: "", time: null, chips: [{ text: "VIP", muted: false }], star: false, unread: false, amount: "" },
    { title: "Sam Sample", name: "Sam Sample", address: "", subtitle: "c_1002", snippet: "", time: null, chips: [{ text: "New", muted: false }], star: false, unread: true, amount: "" },
  ],
}, "list layout from roles: labels map, null hides, unread matches an array element");
const generic = { ...list, roles: null };
equal(recordLayout(generic), {
  view: "list",
  rows: [
    [{ label: "ID", value: "c_1001" }, { label: "Name", value: "Alex Example" }, { label: "Visits", value: "4" }, { label: "Tags", value: "vip" }],
    [{ label: "ID", value: "c_1002" }, { label: "Name", value: "Sam Sample" }, { label: "Visits", value: "1" }, { label: "Tags", value: "new, internal" }],
  ],
}, "list without roles is the generic typed rows");

// record_ref thread
const thread = parseRecordRef(entryOf(fixture("record-ref-thread")));
assert(thread, "thread parses");
equal(recordLayout(thread), {
  view: "role-thread",
  messages: [
    { actor: "", time: { text: "2026-11-18T01:00:00Z", instant: Date.parse("2026-11-18T01:00:00Z") }, day: dayKey(Date.parse("2026-11-18T01:00:00Z")), text: "Is my order ready?", own: false, approved: false },
    { actor: "", time: { text: "2026-11-18T01:02:00Z", instant: Date.parse("2026-11-18T01:02:00Z") }, day: dayKey(Date.parse("2026-11-18T01:02:00Z")), text: "Yes, ready for pickup.", own: true, approved: false },
  ],
  contact: "",
}, "thread bubbles: side.self is the own side");
equal(recordLayout({ ...thread, roles: null }), {
  view: "thread",
  messages: [
    { sender: "in", time: "2026-11-18T01:00:00Z", text: "Is my order ready?", outbound: false },
    { sender: "out", time: "2026-11-18T01:02:00Z", text: "Yes, ready for pickup.", outbound: true },
  ],
}, "thread without roles keeps the heuristic layout");
const noText = clone(thread);
noText.fields = noText.fields.filter((f) => f.key !== "text");
noText.roles = null;
equal(recordLayout(noText), null, "a thread with no text field falls back");

// table: key/value of the first item
const tableFixture = parseRecordRef(entryOf(fixture("record-ref-table")));
assert(tableFixture, "table fixture parses");
equal(tableFixture.roles, null, "the table fixture has no roles");
equal(recordLayout(tableFixture, { yes: "Yes", no: "No" }), { view: "table", pairs: [{ label: "Order", value: "o_501" }, { label: "Total", value: "42" }, { label: "Paid", value: "Yes" }] }, "table fixture layout");
const table = { ...generic, view: "table" as const };
equal(recordLayout(table), { view: "table", pairs: [{ label: "ID", value: "c_1001" }, { label: "Name", value: "Alex Example" }, { label: "Visits", value: "4" }, { label: "Tags", value: "vip" }] }, "table layout");
equal(recordLayout({ ...table, items: [] }), null, "an empty table falls back");

// roles: instants, chips, amount, own side, hostile text, strict parsing
const NOW = Date.parse("2026-11-18T01:00:00Z");
equal([parseInstant("2026-11-18T01:00:00Z"), parseInstant(1_790_000_000), parseInstant(1_790_000_000_000), parseInstant("1790000000"), parseInstant("yesterday"), parseInstant("12"), parseInstant(null)], [NOW, 1_790_000_000_000, 1_790_000_000_000, 1_790_000_000_000, null, null, null], "instants");
const rich = clone(list) as any;
rich.fields.push({ ...rich.fields[1], key: "state" }, { ...rich.fields[1], key: "due" }, { ...rich.fields[1], key: "price" });
rich.items[0] = { ...rich.items[0], state: "open", due: "next week", price: "HK$12" };
rich.roles = { title: "name", snippet: "tags", time: "due", status: "state", amount: "price" };
const richLayout = recordLayout(rich);
assert(richLayout && richLayout.view === "role-list", "rich list");
equal(richLayout.rows[0], { title: "Alex Example", name: "Alex Example", address: "", subtitle: "", snippet: "vip", time: { text: "next week", instant: null }, chips: [{ text: "open", muted: true }], star: false, unread: false, amount: "HK$12" }, "all list roles; time as given; an unmapped value shows muted");
const hostile = "[x](https://e.test) **b** " + "A".repeat(2000);
const hostileModel = clone(list) as any;
hostileModel.items[0].name = hostile;
const hostileRows = recordLayout(hostileModel) as any;
equal(hostileRows.rows[0].title.length, CELL_MAX_CHARS, "role-mapped untrusted text is capped");
assert(hostileRows.rows[0].title.startsWith("[x](https://e.test) **b**"), "role-mapped text stays literal");
const rolesOf = (roles: unknown) => { const e = clone(fixture("record-ref-list")) as any; e.payload.roles = roles; return parseRecordRef(entryOf(e)); };
for (const [what, roles] of [
  ["unknown role", { colour: "name" }],
  ["a key that is not a field", { title: "nope" }],
  ["side without self", { side: { key: "name", self: [] } }],
  ["side naming no field", { side: { key: "zzz", self: ["a"] } }],
  ["extra side key", { side: { key: "name", self: ["a"], more: 1 } }],
  ["non-string role", { title: 3 }],
  ["roles not an object", "name"],
] as const) equal(rolesOf(roles), null, `record rejects roles: ${what}`);
equal(rolesOf({})?.roles, {}, "empty roles parse");
for (const [what, roles] of [
  ["labels not an object", { labels: [] }],
  ["label display not a string", { labels: { a: 1 } }],
  ["label display too long", { labels: { a: "x".repeat(101) } }],
  ["empty label key", { labels: { "": "x" } }],
  ["unread without values", { unread: { key: "tags", values: [] } }],
  ["unread naming no field", { unread: { key: "zzz", values: ["a"] } }],
  ["unread extra key", { unread: { key: "tags", values: ["a"], more: 1 } }],
] as const) equal(rolesOf(roles), null, `record rejects roles: ${what}`);
const tooMany = Object.fromEntries(Array.from({ length: 41 }, (_, i) => [`k${i}`, "x"]));
equal(rolesOf({ labels: tooMany }), null, "record rejects more than 40 labels");
for (const [what, mutate] of [
  ["blank tool_label", (p: any) => { p.tool_label = " "; }],
  ["long account", (p: any) => { p.account = "x".repeat(201); }],
  ["non-string toolkit_label", (p: any) => { p.toolkit_label = 3; }],
  ["fractional total", (p: any) => { p.total = 1.5; }],
  ["negative total", (p: any) => { p.total = -1; }],
  ["string total", (p: any) => { p.total = "3"; }],
] as const) {
  const bad = clone(fixture("record-ref-list")) as any;
  mutate(bad.payload);
  equal(parseRecordRef(entryOf(bad)), null, `record rejects ${what}`);
}
const bare = clone(fixture("record-ref-list")) as any;
for (const k of ["tool_label", "toolkit_label", "account", "total"]) delete bare.payload[k];
equal(parseRecordRef(entryOf(bare))!.account, null, "header data is optional");
equal(parseRecordRef(entryOf(bare))!.total, null, "total is optional");

// labels: null hides, a star label is the star, unmapped shows muted; a hostile key is plain data
const mail = clone(fixture("record-ref-list")) as any;
mail.payload.items = [
  { id: "1", name: "Dev Ops <ops@x.example>", tags: ["UNREAD", "IMPORTANT", "CATEGORY_UPDATES", "INBOX", "__proto__", "Odd"] },
];
mail.payload.roles = {
  title: "name", subtitle: "id", badge: "tags",
  labels: JSON.parse('{"UNREAD":null,"IMPORTANT":"★","CATEGORY_UPDATES":"Updates","INBOX":null,"__proto__":"Poison"}'),
  unread: { key: "tags", values: ["UNREAD"] },
};
const mailLayout = recordLayout(parseRecordRef(entryOf(mail))!) as any;
equal(mailLayout.rows[0].chips, [{ text: "Updates", muted: false }, { text: "Poison", muted: false }, { text: "Odd", muted: true }], "labels: hidden, star and mapped; the constructor-like key is just a key");
equal([mailLayout.rows[0].star, mailLayout.rows[0].unread, mailLayout.rows[0].name, mailLayout.rows[0].address], [true, true, "Dev Ops", "ops@x.example"], "star, unread and the sender split");

// "Name <address>" split: only a trailing angle address; anything else stays a name
equal(splitAddress('"Dev Ops" <a@b.example>'), { name: "Dev Ops", address: "a@b.example" }, "quoted name");
equal(splitAddress("<a@b.example>"), { name: "a@b.example", address: "" }, "address only");
equal(splitAddress("Mei Chan"), { name: "Mei Chan", address: "" }, "no address");
equal(splitAddress("Sam <https://e.test/x>"), { name: "Sam <https://e.test/x>", address: "" }, "a URL is not an address");
equal(splitAddress("A <b> <c@d.example>"), { name: "A <b>", address: "c@d.example" }, "only the last address splits");
const longName = "N".repeat(CELL_MAX_CHARS);
equal(splitAddress(`${longName} <a@b.example>`).name, longName, "a long name is kept whole for CSS to truncate");

// thread: actor "api" is the approved tag, a contact comes from the other side, days come from the time role
const wa = clone(fixture("record-ref-thread")) as any;
wa.payload.fields.push({ key: "by", label: "By", type: "string", untrusted: true });
wa.payload.roles.actor = "by";
wa.payload.items[0].by = "Mr Lee <+85255550123>";
wa.payload.items[1].by = "api";
wa.payload.items.push({ direction: "in", text: "YES", sent_at: "2026-11-19T09:00:00Z", by: "Mr Lee" });
const waLayout = recordLayout(parseRecordRef(entryOf(wa))!) as any;
equal([waLayout.contact, waLayout.messages.map((m: any) => [m.actor, m.approved])], ["Mr Lee", [["Mr Lee", false], ["", true], ["Mr Lee", false]]], "thread contact and approved tag");
equal(waLayout.messages[0].day === waLayout.messages[2].day, false, "a later day gets its own key");
equal(recordLayout(rolesOf({ side: { key: "name", self: ["x"] } })!)?.view, "list", "roles with no layout role fall back to the generic rows");

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
