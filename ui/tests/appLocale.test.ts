import * as L from "../src/features/workspace-apps/appLocale";
import { parseNavigation } from "../src/features/workspace-apps/screen/screenLifecycle";
import { chatContext } from "../src/features/app-shell/chatScreen";
import { translateUiText } from "../src/lib/locale";
import * as C from "../src/features/app-shell/chat/contract";

export {};
declare function require(name: string): any;
const ok = (v: unknown, m: string) => { if (!v) throw new Error(m); };
const eq = (a: unknown, e: unknown, m: string) => {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${m}: ${JSON.stringify(a)} !== ${JSON.stringify(e)}`);
};
const tw = <T,>(v: T) => ({ "zh-TW": v });
const n9 = (v: unknown) => Object.fromEntries(Array.from({ length: 9 }, (_, i) => [`en-x${i}`, v]));

function main() {
  for (const [k, exp] of [["zh-TW", ["zh-TW", "zh-HK", "zh-Hant"]], ["zh-HK", ["zh-HK", "zh-TW", "zh-Hant"]], ["zh-Hant-HK", ["zh-Hant-HK", "zh-TW", "zh-HK", "zh-Hant"]], ["fr-CA", ["fr-CA", "fr"]]] as const) {
    eq(L.localeCandidates(k), exp, k);
  }
  for (const t of ["zh-TW", "zh-HK", "zh-Hant", "zh-Hant-HK"]) {
    const lo = L.localeCandidates(t).map((c) => c.toLowerCase());
    ok(!lo.includes("zh") && !lo.includes("zh-cn") && !lo.includes("zh-hans"), t);
  }
  eq([L.pickLocaleEntry({ "zh-CN": { title: "A" } }, "zh-TW"), L.pickLocaleEntry({ "zh-HK": { title: "B" } }, "zh-TW")], [undefined, { title: "B" }], "pick");
  ok(L.validLocaleTag("zh-TW") && !L.validLocaleTag("zh_TW") && !L.validLocaleTag("zh-TW\n"), "tag");

  const acc = { get title() { return "x"; } };
  for (const [v, b] of [[{ title: "a" }, true], [null, false], [[], false], [new Date(), false], [Object.create({ x: 1 }), false], [acc, false], [JSON.parse('{"__proto__":{"x":1}}'), false]] as const) {
    eq(L.isPlainJsonObject(v), b, "obj");
  }
  ok(L.isPlainTextValue("Hi", 10) && !L.isPlainTextValue("a\nb", 20) && L.isPlainTextValue("a\nb", 20, true) && !L.isPlainTextValue("a\u2028b", 20, true) && !L.isPlainTextValue("a\u0085b", 20) && !L.isPlainTextValue("a\u0085b", 20, true), "txt");

  eq(L.parseAppDisplayI18n(tw({ title: " 客 ", summary: "一\n二", tagline: "關" })), tw({ title: "客", summary: "一\n二", tagline: "關" }), "app");
  eq(L.parseAppDisplayI18n(tw({ summary: "x".repeat(280) })), tw({ summary: "x".repeat(280) }), "280");
  const badA = [{}, { zh_TW: { title: "x" } }, { "zh-tw": { title: "A" }, ...tw({ title: "B" }) }, tw({ title: "x", extra: "y" }), tw({ about: "x" }), { ...tw({}), ja: { title: "x" } }, n9({ title: "x" }), ...["a\u0007b", "a\nb", "x".repeat(81)].map((v) => tw({ title: v })), ...["a\nb", "x".repeat(81)].map((v) => tw({ tagline: v })), tw({ summary: "x".repeat(281) })];
  for (const b of badA) eq(L.parseAppDisplayI18n(b), null, "badApp");

  eq(L.parseListingDisplayI18n(tw({ title: " 客 ", summary: "一\n二", about: "甲\n乙", can: [" 建 "], access_notes: { c: " 權 " } })), tw({ title: "客", summary: "一\n二", about: "甲\n乙", can: ["建"], access_notes: { c: "權" } }), "list");
  eq(L.parseListingDisplayI18n(tw({ about: "x".repeat(600) })), tw({ about: "x".repeat(600) }), "600");
  eq([L.parseListingDisplayI18n(tw({ about: "x".repeat(601) })), L.parseListingDisplayI18n(tw({ summary: "x".repeat(281) })), L.displayI18nFromListing(tw({ about: "x" }))], [null, null, null], "max");
  eq(L.displayI18nFromListing(tw({ title: "客", about: "x" })), tw({ title: "客" }), "fromL");
  eq(L.parseAppDisplayI18n({ "zh-TW": {}, ja: { title: null } }, { wire: true }), {}, "wireApp0");
  eq(L.parseListingDisplayI18n({ "zh-TW": { title: " 客 ", about: null, can: [], access_notes: {}, tagline: null }, ja: {} }, { wire: true }), tw({ title: "客" }), "wireListNull");
  eq(L.parseListingDisplayI18n(tw({ access_notes: { "CRM Read / Write": " 唯讀 " } }), { wire: true }), tw({ access_notes: { "CRM Read / Write": "唯讀" } }), "wireNotes");
  for (const b of [tw({ title: "x", extra: "y" }), tw({ can: ["1", "2", "3", "4", "5", "6"] }), tw({ access_notes: { "bad\nkey": "x" } }), tw({ access_notes: JSON.parse('{"__proto__":"x"}') }), tw({ about: "x".repeat(601) })]) {
    eq(L.parseListingDisplayI18n(b, { wire: true }), null, "badWire");
  }

  const d = (k: string) => ({ CRM: "D-C", Sum: "D-S", Tag: "D-T", Sec: "D-N" })[k] ?? k;
  const src = { title: "CRM", summary: "Sum", tagline: "Tag", i18n: tw({ title: "T-C", summary: "T-S", tagline: "T-T" }), listing: { tagline: "L", i18n: tw({ title: "L-C", summary: "L-S", tagline: "L-T" }) } };
  eq([L.localizedAppTitle(src, "zh-TW", d), L.localizedAppSummary(src, "zh-TW", d), L.localizedAppTagline(src, "zh-TW", d), L.localizedAppTitle({ ...src, i18n: undefined }, "zh-TW", d), L.localizedAppTitle({ title: "CRM", i18n: { "zh-CN": { title: "SC" } } }, "zh-TW", d), L.localizedAppTitle({ title: "CRM", i18n: tw({ title: "T" }) }, "en-US", d), L.localizedAppTitle({ title: "CRM", listing: { i18n: tw({ title: "x", bad: "y" } as any) } }, "zh-TW")], ["T-C", "T-S", "T-T", "L-C", "D-C", "CRM", "CRM"], "loc");
  eq([L.localizedSectionLabel({ label: "Sec", i18n: { "zh-HK": { label: "HK" } } }, "zh-TW", d), L.localizedSectionLabel({ label: "Sec" }, "zh-TW", d), L.localizedSectionLabel({ label: "Sec" }, "en", d)], ["HK", "D-N", "Sec"], "sec");

  const nv = (i18n: unknown, label = "C", section_query = "section") => ({ section_query, default_section: "c", sections: [{ id: "c", label, ...(i18n === undefined ? {} : { i18n }) }] });
  eq(parseNavigation(nv(tw({ label: "聯" })))?.sections[0].i18n?.["zh-TW"]?.label, "聯", "nav");
  const nav0 = parseNavigation(nv({}));
  ok(nav0 !== null && nav0.sections[0].i18n === undefined, "nav{}");
  for (const b of [nv({ zh_TW: { label: "x" } }), nv(n9({ label: "x" })), ...["x\ny", "  ", "x".repeat(41), "x\u0085y"].map((v) => nv(tw({ label: v }))), nv(tw({ label: "x", extra: "y" })), nv({ "zh-tw": { label: "A" }, ...tw({ label: "B" }) }), nv(Object.defineProperty({}, "zh-TW", { enumerable: true, get: () => ({ label: "x" }) })), nv(undefined, "x\u2028y"), nv(undefined, "x\u0085y", "tab")]) eq(parseNavigation(b), null, "badN");

  const fs = require("fs"), path = require("path");
  const s = JSON.parse(fs.readFileSync(path.join(require("process").cwd(), "..", "contracts", "app-chat", "v1", "app-chat.schema.json"), "utf8")).$defs;
  eq([s.labelI18n.maxProperties, s.contextI18n.maxProperties, s.cardI18n.maxProperties], [8, 8, 8], "caps");

  const chat = {
    contract: C.APP_CHAT_CONTRACT, app: "crm",
    contexts: [{ id: "c", label: "C", prompts: ["S"], i18n: { "zh-HK": { label: "C-HK", prompts: ["P-HK"] } }, record: { label: "R", prompts: ["R0"], i18n: tw({ label: "C-TW", prompts: ["R-TW"] }) } }],
    attachments: [{ id: "csv-import", label: "CSV", i18n: tw({ label: "CSV-TW" }) }],
    directives: [{ match: "cadence_m", card: { title: "T", text: "X", i18n: tw({ title: "T-TW", text: "X-TW" }), fields: [{ label: "F", from: "s", i18n: tw({ label: "F-TW" }) }], buttons: [{ label: "B", run: "open-view", view: "v", i18n: tw({ label: "B-TW" }) }] } }],
    subjects: [{ kind: "k", label: "K", i18n: tw({ label: "K-TW" }) }],
  };
  const cTw = C.localizeAppChat(C.parseAppChat(chat), "zh-TW", (k) => k);
  eq([cTw.contexts[0].label, cTw.contexts[0].prompts, cTw.contexts[0].record?.label, cTw.contexts[0].record?.prompts, cTw.attachments[0].label, cTw.subjects[0].label, cTw.directives[0].card?.title, cTw.directives[0].card?.text, cTw.directives[0].card?.fields[0].label, cTw.directives[0].card?.buttons[0].label], ["C-HK", ["P-HK"], "C-TW", ["R-TW"], "CSV-TW", "K-TW", "T-TW", "X-TW", "F-TW", "B-TW"], "chat");
  const fb = C.localizeAppChat(C.parseAppChat({ contract: C.APP_CHAT_CONTRACT, app: "crm", contexts: [{ id: "c", label: "C", prompts: [] }], attachments: [{ id: "csv-import", label: "A" }] }), "zh-TW", (k) => ({ C: "F-C", A: "F-A" })[k] ?? k);
  eq([fb.contexts[0].label, fb.attachments[0].label, C.localizeAppChat(C.parseAppChat(chat), "en", (k) => k).contexts[0].label], ["F-C", "F-A", "C"], "fb");

  const c0 = (i18n: unknown) => ({ contexts: [{ id: "c", label: "C", prompts: [], i18n }] });
  const a0 = (i18n: unknown) => ({ attachments: [{ id: "csv-import", label: "A", i18n }] });
  const s0 = (i18n: unknown) => ({ subjects: [{ kind: "k", label: "K", i18n }] });
  for (const patch of [a0({}), a0({ zh_TW: { label: "x" } }), a0(n9({ label: "x" })), c0(tw({ prompts: ["a", "b", "c", "d"] })), c0(tw({ prompts: ["x".repeat(81)] })), s0(tw({ label: "a\nb" })), s0(tw({ label: "x", extra: "y" })), s0({ "zh-tw": { label: "A" }, ...tw({ label: "B" }) })]) {
    let threw = false;
    try { C.parseAppChat({ ...chat, ...patch }); } catch (e) { threw = e instanceof C.AppChatContractError; }
    ok(threw, "badChat");
  }

  const twT = (k: string) => translateUiText(k, "zh-TW");
  eq(chatContext(cTw, "c", true, twT)?.label, "C-TW（已開啟）", "ctx");
  const dChat = C.parseAppChat({ contract: C.APP_CHAT_CONTRACT, app: "crm", contexts: [{ id: "c", label: "C", prompts: [], record: { label: "A$&$$", prompts: [] } }] });
  eq(chatContext(dChat, "c", true, twT)?.label, "A$&$$（已開啟）", "$safe");
  eq([twT("New conversation (unsaved)"), twT("New {label} conversation (unsaved)").replace("{label}", () => "聯絡人")], ["新對話（未儲存）", "新聯絡人對話（未儲存）"], "draft");
  eq([L.localizedAppCategory("customers", "zh-TW"), L.localizedAppCategory("marketing", "zh-TW"), translateUiText("customers", "zh-TW")], ["客戶", "行銷", "位客戶"], "cat");
  console.log("app locale checks passed");
}

main();
