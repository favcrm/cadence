export {};
declare function require(name: string): any;
const actions = require("../src/features/workspace-apps/screen/screenActions") as typeof import("../src/features/workspace-apps/screen/screenActions");

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

const calls: { url: string; body: any }[] = [];
(globalThis as any).fetch = async (input: unknown, init?: { method?: string; body?: string }) => {
  const url = String(input);
  const body = init?.body ? JSON.parse(init.body) : undefined;
  calls.push({ url, body });
  if (url === "/api/app-social-drafts/radar/save" || url === "/api/app-social-drafts/radar/scan") {
    return new Response(JSON.stringify({ keywords: body.keywords ?? [], sources: body.sources ?? [], schedule: body.schedule ?? null, signals: [], angles: [] }), { status: 200 });
  }
  throw new Error(`Unexpected fetch ${init?.method ?? "GET"} ${url}`);
};

const ctx = {
  installation: { install_id: "inst-1" } as any,
  installId: "inst-1",
  contextId: "default",
  runs: [],
  onChanged: () => {},
};
const ui = { showLink: () => {}, actionToken: "a".repeat(64) };

async function main() {
  const saveOk = await actions.runCall(ctx, "social.radar.save", {
    alias: "social.draft",
    keywords: ["matcha latte HK"],
    sources: ["ig", "threads", "youtube"],
    schedule: { cadence: "daily", run_time: "09:00", timezone: "Asia/Hong_Kong" },
    region: "HK",
    locale: "zh-HK",
  }, ui);
  assert(saveOk.ok, "social.radar.save accepts optional sources and schedule");
  const saveReq = calls.find((c) => c.url === "/api/app-social-drafts/radar/save");
  assert(saveReq?.body?.sources?.length === 3 && saveReq.body.schedule?.run_time === "09:00", "forwards sources and schedule to backend");

  const scanOk = await actions.runCall(ctx, "social.radar.scan", {
    alias: "social.draft",
    request_id: "mr-test-1",
    keywords: ["matcha latte HK"],
    sources: ["youtube", "meta-ads"],
    region: "HK",
    locale: "zh-HK",
  }, ui);
  assert(scanOk.ok, "social.radar.scan accepts optional sources");
  const scanReq = calls.find((c) => c.url === "/api/app-social-drafts/radar/scan");
  assert(scanReq?.body?.sources?.length === 2 && scanReq.body.sources[1] === "meta-ads", "forwards selected sources to scan endpoint");

  const emptySources = await actions.runCall(ctx, "social.radar.save", {
    alias: "social.draft",
    keywords: ["matcha latte HK"],
    sources: [],
  }, ui);
  assert(!emptySources.ok && emptySources.refusal.code === "bad_args", "rejects empty sources array");

  const badSchedule = await actions.runCall(ctx, "social.radar.save", {
    alias: "social.draft",
    keywords: ["matcha latte HK"],
    schedule: { cadence: "daily", unknown_key: "x" },
  }, ui);
  assert(!badSchedule.ok && badSchedule.refusal.code === "bad_args", "rejects unknown schedule keys");

  const extraKeyScan = await actions.runCall(ctx, "social.radar.scan", {
    alias: "social.draft",
    request_id: "mr-test-2",
    schedule: { cadence: "daily" },
  }, ui);
  assert(!extraKeyScan.ok && extraKeyScan.refusal.code === "bad_args", "rejects disallowed fields on scan");

  console.log("social radar screenActions checks passed");
}

void main();
