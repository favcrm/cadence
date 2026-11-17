import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { Window } from "happy-dom";
import { LocaleProvider } from "../src/lib/locale";
import ScreenSlotLayer from "../src/features/workspace-apps/screen/ScreenSlotLayer";
import { recordServices } from "../src/features/workspace-apps/screen/screenServices";
import type { MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";

function assert(value: unknown, message: string): asserts value { if (!value) throw new Error(message); }
const win = new Window({ url: "http://localhost/" });
Object.defineProperty(globalThis, "window", { value: win, configurable: true });
const receipt: MountReceipt = {
  mount: `/api/app-screen/${"a".repeat(64)}`, bridge_nonce: "b".repeat(64), generation: 1,
  tag: "app", action_token: "c".repeat(64), tools: [], host_contract: "screen-records.v1",
  operations: ["catalog.confirm"], navigation: null, install_id: "inst-1", context_id: "ctx-1",
  digest: `sha256:${"d".repeat(64)}`, display_scope_key: "e".repeat(64), display_session_key: "f".repeat(64), remote_images: [],
};
let effect: "read" | "draft" = "read";
globalThis.fetch = (async () => new Response(JSON.stringify({ confirmation_token: "1".repeat(64), card: {
  operation: "catalog.confirm", facts: { tool: "Read data", slug: "synthetic.lookup", effect,
    price: { currency: "USD", scale: 6, amount: "0.123456" }, input: { body: { query: "Account" } }, account: "Account" },
} }), { status: 200, headers: { "Content-Type": "application/json" } })) as typeof fetch;

async function render(locale: "en" | "zh-TW"): Promise<string> {
  win.localStorage.setItem("cadence-locale", locale);
  const services = recordServices(receipt, "locale-test", { current: () => true, refused: () => {}, command: () => {} });
  const plan = await services.planner("catalog.confirm", { slot: "lookup", request_id: "synthetic-request-1", input: { body: { query: "Account" } } });
  assert("run" in plan && plan.recordCard, "a valid quoted action creates a host card");
  const view = { token: "host-slot", label: plan.label, anchor: "footer" as const, pending: false, recordCard: plan.recordCard };
  const html = renderToStaticMarkup(<LocaleProvider><ScreenSlotLayer view={view} link={null} onTap={() => {}} onCancel={() => {}} onDismissLink={() => {}} /></LocaleProvider>);
  const container = win.document.createElement("div");
  container.innerHTML = html;
  return container.textContent ?? "";
}

async function main(): Promise<void> {
  const en = await render("en");
  assert(en.includes("Run this tool?") && en.includes("Run tool") && en.includes("USD 0.123456"), "English owner sees action and exact price");
  const zh = await render("zh-TW");
  for (const text of ["執行此工具？", "執行工具", "價格", "輸入內容", "讀取資料", "USD 0.123456", "synthetic.lookup", '"query":"Account"'])
    assert(zh.includes(text), `Traditional Chinese owner sees ${text}`);
  assert(zh.includes("Read data") && zh.includes("Account"), "provider-supplied tool and account names are never translated");
  assert(!zh.includes("Review this exact tool, input and price before confirming.") && !zh.includes("If the result is unclear, check this request's status before trying again."), "review and uncertainty instructions are localized");
  effect = "draft";
  const draft = await render("zh-TW");
  assert(draft.includes("建立草稿") && draft.includes("draft"), "draft effect is described without hiding the exact operation");
  await win.happyDOM.close();
}
void main().catch(error => { throw error; });
