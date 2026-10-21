// CAD-1365 protected generation slot — INDEPENDENT guard acceptance driver.
// Authored against design-plans/designer-generation-slot-v1.md, not against
// any implementation. It transpiles the REAL screenSlot.ts + screenActions.ts
// with the pinned ui TypeScript, loads them in an isolated vm context with a
// controlled workspaceApps SDK counter, and drives the real makePlanner +
// SlotController through proposal, guard and tap paths.
//
// Transport/controller evidence only: the workspaceApps fake is a counter,
// not server proof — the immutable native acceptance owns RPC/provider truth.
// On the current baseline (request_id unlisted, no host quote label, no
// closed receipt) the NEW-feature checks must be honest RED while the OLD
// positive control (real guards on the existing planner) must still hold.
//
// Run (inside a build slot): node ui/acceptance/cad1365-generation-slot.mjs

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const UI = path.resolve(HERE, "..");
const WORK = fs.mkdtempSync(path.join(os.tmpdir(), "designer-generation-slot-acceptance-haiku-d1a14-"));
const OUT = path.join(WORK, "out");
fs.mkdirSync(OUT, { recursive: true });

let failures = 0, total = 0;
function check(value, label) {
  total++;
  if (value) { console.log(`  ok    ${label}`); return true; }
  failures++; console.log(`  FAIL  ${label}`); return false;
}
const controllers = [];
function cleanup() {
  for (const c of controllers.splice(0)) { try { c.close(); } catch {} }
  try { fs.rmSync(WORK, { recursive: true, force: true }); } catch {}
}
function finish(code) { cleanup(); process.exit(code); }

// ---------- transpile the real production modules ----------------------------
// Single-file transpileModule: no type-check, no dependency resolution, so
// screenActions.ts compiles alone and its imports become plain require()s
// satisfied by the stub files written at the mirrored paths below.
const require = createRequire(path.join(UI, "package.json"));
const ts = require("@typescript/typescript6");
function emit(rel) {
  const source = fs.readFileSync(path.join(UI, "src", rel), "utf8");
  const { outputText } = ts.transpileModule(source, { compilerOptions: {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, esModuleInterop: true } });
  const dest = path.join(OUT, rel.replace(/\.ts$/, ".js"));
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(dest, outputText);
  return dest;
}
for (const rel of [
  "features/workspace-apps/screen/screenProtocol.ts",
  "features/workspace-apps/screen/screenSlot.ts",
  "features/workspace-apps/screen/screenActions.ts",
  "features/workspace-apps/socialPublish.ts",
]) emit(rel);
const featureRoot = OUT;
const featureRootRel = p => path.join(featureRoot, p);
function stub(rel, source) {
  const dest = featureRootRel(rel);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(dest, source);
  return dest;
}

// ---------- controlled workspaceApps + dependency stubs ----------------------
const calls = { bindingQuote: [], startRun: [] };
const QUOTE = { schema: 1, currency: "USD", unit_price_micros: 12000, units: 1, total_price_micros: 12000, price_revision: "v1" };
const RUN_ID = `run-${"a".repeat(32)}`;
let nativeReply = { id: RUN_ID, state: "succeeded", snapshot: { quotes: "SECRET" }, prompt: "SECRET", original_artifact: { ref: "SECRET" } };
const workspaceAppsStub = {
  bindingQuote: async (installId, slot) => { calls.bindingQuote.push({ installId, slot }); return { slot, binding_digest: "b".repeat(64), quote_digest: "q".repeat(64), quote: QUOTE }; },
  startRun: async body => { calls.startRun.push(body); return nativeReply; },
};
// Stub the runtime imports of the REAL screenActions.js at their resolved
// paths. Only the workspaceApps surface (bindingQuote/startRun) is exercised;
// the rest are inert and never reached on the run.start path.
stub("lib/api.js", `exports.ApiError = class ApiError extends Error { constructor(m, s, e) { super(m); this.status = s; this.code = e && e.code; } };\n`);
stub("lib/useLocation.js", `exports.navigate = () => {};\n`);
stub("features/workspace-apps/contextSelection.js", `exports.rememberContext = () => {};\n`);
stub("lib/sessionKey.js", `exports.sessionHeaders = () => ({ "X-Cadence-Session": "test-key" }); exports.sessionKey = () => "test-key";\n`);
stub("features/workspace-apps/workspaceApps.js",
  `exports.workspaceApps = ${JSON.stringify(null)};\n`);
// The workspaceApps counter must be live-callable, so write a real module.
fs.writeFileSync(featureRootRel("features/workspace-apps/workspaceApps.js"),
  `const store = require(${JSON.stringify(featureRootRel("wa-store.js"))});\nexports.workspaceApps = store.workspaceApps;\n`);
fs.writeFileSync(featureRootRel("wa-store.js"), `exports.workspaceApps = {};\n`);
stub("features/workspace-apps/screen/screenProjection.js",
  `exports.contextDefaultKeys = () => new Set(); exports.instagramLink = () => null;\n`);
stub("features/workspace-apps/socialConnect.js",
  `exports.accountLabel = s => s; exports.connectReturnTo = () => ""; exports.publicationBinding = () => null; exports.sendSlot = () => null;\n`);
stub("features/workspace-apps/screen/screenAssets.js",
  `exports.downscaleToDataUrl = async () => "data:image/png;base64,x";\n`);

const waStore = require(featureRootRel("wa-store.js"));
Object.assign(waStore.workspaceApps, workspaceAppsStub);

const { SlotController } = require(featureRootRel("features/workspace-apps/screen/screenSlot.js"));
const { makePlanner } = require(featureRootRel("features/workspace-apps/screen/screenActions.js"));

const CTX = {
  installation: { install_id: "i1", name: "app", title: "App", version: "1", summary: "", digest: "d".repeat(64), catalog_generation: "g", storage_kind: "workspace", project_link: null, approved: true, executable: true, approval: { state: "ok" }, guide: "", files: [], capabilities: null, connection_slots: [],
    workflows: [{ name: "create-image", label: "Create image", capability_slots: ["image"], inputs: [{ name: "brief" }] }] },
  installId: "i1", contextId: "ctx1", runs: [], onChanged: () => {},
};

let now = 0;
const sent = [];
const views = [];
function makeController() {
  const c = new SlotController(m => sent.push(m), v => views.push(v), makePlanner(() => CTX), () => now);
  controllers.push(c);
  return c;
}
const ANCHOR = { x: 0, y: 0, w: 80, h: 40 };
const ARGS = { workflow: "create-image", inputs: { brief: "square hero" } };

// ---------- OLD positive control: real guards on the existing planner -------
console.log("CONTROL. existing planner + SlotController guards");
{
  const c = makeController();
  const before = sent.length;
  await c.request({ v: 2, op: "slot", id: "s1", verb: "run.start", args: ARGS, anchor: ANCHOR });
  check(calls.startRun.length === 0, "proposal performs no native start");
  check(views.length > 0 && typeof views[views.length - 1]?.label === "string", "planner produced a host slot view");
  const view = c.view();
  check(!!view && typeof view.token === "string", "controller exposes a live view token");
  if (view) {
    now = 600; // past the 500 ms guard
    check(c.tap(view.token, false, now) === false && calls.startRun.length === 0, "untrusted tap after guard still refuses");
    const c2 = makeController(); // born at now=600; an immediate tap is EARLY
    await c2.request({ v: 2, op: "slot", id: "s2", verb: "run.start", args: ARGS, anchor: ANCHOR });
    const v2 = c2.view();
    if (v2) check(c2.tap(v2.token, true, now) === false && calls.startRun.length === 0, "trusted but EARLY tap refuses (500 ms press guard)");
    now += 600; // monotonic: 1200, both slots past the guard
    check(c.tap(view.token, true, now) === true && calls.startRun.length === 1, "one legal protected tap starts native run once");
    const busyTap = c.tap(view.token, true, 800);
    check(busyTap === false && calls.startRun.length === 1, "second tap while busy cannot start again");
    await new Promise(r => setTimeout(r, 20));
    const done = sent.slice(before).find(m => m.op === "slot-state" && m.id === "s1" && m.state === "done");
    check(!!done, "completed run yields slot-state done on the real channel");
  }
}

// ---------- NEW contract assertions (expected RED on current baseline) -------
console.log("NEW. generation-slot contract from designer-generation-slot-v1");

// valid stable request_id inside native grammar reaches native start verbatim
{
  const c = makeController();
  calls.startRun.length = 0;
  await c.request({ v: 2, op: "slot", id: "g1", verb: "run.start", args: { ...ARGS, request_id: "gen-intent-1" }, anchor: ANCHOR });
  const v = c.view();
  check(!!v, "run.start args with a valid stable request_id still plans a host slot");
  if (v) {
    now += 600;
    check(c.tap(v.token, true, now) === true, "legal protected tap fires (past guard)");
    await new Promise(r => setTimeout(r, 20));
    const body = calls.startRun[0];
    check(!!body && body.request_id === "gen-intent-1", "native start receives the SAME supplied request_id");
    check(!!body && body.inputs && body.inputs.brief === "square hero" && body.workflow === "create-image",
      "frozen workflow + inputs reach native start");
    check(!!body && body.expected_quotes && body.expected_quotes.image &&
      JSON.stringify(body.expected_quotes.image) === JSON.stringify(QUOTE),
      "exact native quoted terms frozen into expected_quotes");
    const done = sent.find(m => m.op === "slot-state" && m.id === "g1" && m.state === "done");
    check(!!done && done.receipt && Object.keys(done.receipt).sort().join() === "request_id,run_id,state" &&
      done.receipt.run_id === RUN_ID && done.receipt.request_id === "gen-intent-1" && done.receipt.state === "succeeded",
      "slot-state done carries a closed receipt {run_id,request_id,state} only — no snapshot/prompt/ref leak");
  }
}

// hostile authority fields refuse BEFORE any start
for (const [i, field] of ["actor", "approval", "workspace", "quote", "token", "origin"].entries()) {
  const c = makeController();
  const n0 = calls.startRun.length;
  await c.request({ v: 2, op: "slot", id: `h${i}`, verb: "run.start", args: { ...ARGS, [field]: "forged" }, anchor: ANCHOR });
  check(c.view() === null && calls.startRun.length === n0, `run.start args with forged '${field}' refuse before start`);
}

// quote appears ONLY in the host label, never in child reply
{
  const c = makeController();
  await c.request({ v: 2, op: "slot", id: "q1", verb: "run.start", args: ARGS, anchor: ANCHOR });
  const v = c.view();
  check(!!v && /0\.012000|12000/.test(v.label), "host slot label carries the USD quote at micros precision");
  check(sent.every(m => !(m.op === "slot-state" && m.id === "q1" && JSON.stringify(m).includes("12000"))),
    "no amount or quote object reaches a child-facing slot-state reply");
}

// unsafe quote number refuses the slot before start
{
  calls.startRun.length = 0;
  waStore.workspaceApps.bindingQuote = async () => ({ slot: "image", binding_digest: "b".repeat(64), quote_digest: "q".repeat(64),
    quote: { schema: 1, currency: "USD", unit_price_micros: NaN, units: 1, total_price_micros: NaN, price_revision: "v1" } });
  const c = makeController();
  await c.request({ v: 2, op: "slot", id: "b1", verb: "run.start", args: ARGS, anchor: ANCHOR });
  check(c.view() === null && calls.startRun.length === 0, "malformed/unsafe quote (NaN micros) refuses the slot — no confirmation, no start");
  waStore.workspaceApps.bindingQuote = async (installId, slot) => { calls.bindingQuote.push({ installId, slot }); return { slot, binding_digest: "b".repeat(64), quote_digest: "q".repeat(64), quote: QUOTE }; };
}

// native failed state stays failed — never mapped to done
{
  nativeReply = { id: `run-${"f".repeat(32)}`, state: "failed" };
  const c = makeController();
  await c.request({ v: 2, op: "slot", id: "f1", verb: "run.start", args: ARGS, anchor: ANCHOR });
  const v = c.view();
  if (v) {
    now += 600;
    check(c.tap(v.token, true, now) === true, "legal protected tap fires for the failed run");
    await new Promise(r => setTimeout(r, 20));
    const end = sent.find(m => m.op === "slot-state" && m.id === "f1" && (m.state === "done" || m.state === "refused"));
    check(!!end && end.state === "done" && end.receipt?.state === "failed" && end.receipt.run_id === `run-${"f".repeat(32)}`,
      "failed native run remains failed in its closed receipt — never a generation success");
  } else check(false, "run.start slot missing — could not exercise failed-state path");
  nativeReply = { id: RUN_ID, state: "succeeded" };
}

// malformed native reply becomes uncertain, not a false "nothing started"
{
  nativeReply = { bogus: true };
  const c = makeController();
  await c.request({ v: 2, op: "slot", id: "m1", verb: "run.start", args: ARGS, anchor: ANCHOR });
  const v = c.view();
  if (v) {
    now += 600;
    check(c.tap(v.token, true, now) === true, "legal protected tap fires for the malformed reply");
    await new Promise(r => setTimeout(r, 20));
    const end = sent.find(m => m.op === "slot-state" && m.id === "m1" && (m.state === "done" || m.state === "refused"));
    check(!!end && end.state === "refused" && end.refusal &&
      /uncertain|unconfirmed|changed|check/i.test(end.refusal.code + " " + end.refusal.text),
      "malformed native reply maps to uncertain — never a claim that nothing started");
  } else check(false, "run.start slot missing — could not exercise malformed-reply path");
  nativeReply = { id: RUN_ID, state: "succeeded" };
}

// a non-run.start slot verb completes with generic data but NEVER forwards
// a receipt or private run data — real SlotController, controlled planner
{
  const planner = async () => ({ label: "Use destination",
    run: async () => ({ ok: true, data: { run_id: RUN_ID, request_id: "not-generation", state: "succeeded", quote: "SECRET" } }) });
  const c = new SlotController(m => sent.push(m), () => {}, planner, () => now);
  controllers.push(c);
  await c.request({ v: 2, op: "slot", id: "p1", verb: "publish.destination.use", args: { destination_id: "d1" }, anchor: ANCHOR });
  const v = c.view();
  check(!!v, "publish.destination.use plans a host slot under the real controller");
  if (v) {
    now += 600;
    c.tap(v.token, true, now);
    await new Promise(r => setTimeout(r, 20));
    const end = sent.find(m => m.op === "slot-state" && m.id === "p1" && m.state === "done");
    check(!!end && !("receipt" in end) && !JSON.stringify(end).includes("SECRET") && !JSON.stringify(end).includes("run-"),
      "generic slot success carries no receipt and no generic run data");
  }
}

// teardown: a controller closed after the tap suppresses the late completion
{
  let release;
  const gate = new Promise(r => { release = r; });
  waStore.workspaceApps.startRun = async body => { calls.startRun.push(body); await gate; return nativeReply; };
  const c = makeController();
  const before = sent.length;
  await c.request({ v: 2, op: "slot", id: "t1", verb: "run.start", args: ARGS, anchor: ANCHOR });
  const v = c.view();
  if (v) {
    now += 600;
    check(c.tap(v.token, true, now) === true && calls.startRun.length >= 1, "in-flight protected tap dispatched the native start");
    c.close();
    release();
    await new Promise(r => setTimeout(r, 20));
    check(!sent.slice(before).some(m => m.op === "slot-state" && m.id === "t1" && (m.state === "done" || m.state === "refused")),
      "a closed slot sends no terminal slot-state for the late native reply");
  } else check(false, "run.start slot missing — could not exercise teardown path");
  waStore.workspaceApps.startRun = async body => { calls.startRun.push(body); return nativeReply; };
}

console.log(`\ncad1365-generation-slot: ${total} checks, ${failures} FAILED`);
if (failures) console.log("On the un-implemented baseline the NEW-check failures are the expected clean RED (request_id refused, no quote label, no closed receipt).");
finish(failures === 0 ? 0 : 1);
