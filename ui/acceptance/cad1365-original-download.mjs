// CAD-1365 designer original download — INDEPENDENT host-side acceptance driver.
// Authored against design-plans/designer-original-download-bridge-v1.md, not
// against any implementation. It transpiles the REAL screenSlot.ts and
// screenActions.ts with the pinned ui TypeScript and drives the real makePlanner
// + SlotController for the protected `media.original.download` slot verb.
//
// Effects are observed, not claimed: a global fetch spy records the single
// POST to /api/app-media/original, Node URL.createObjectURL/revokeObjectURL and
// a minimal document.createElement('a') spy record the parent Blob/link
// effects, and every frame message is captured from the real SlotController
// send callback. This is Node transport/controller evidence only. It is NOT
// browser gesture, download or saved-file evidence.
//
// On the current baseline the download verb does not exist, so the NEW checks
// must be honest RED while the OLD positive control (real run.start guards on
// the existing planner) must still hold.
//
// Run (inside a build slot): node ui/acceptance/cad1365-original-download.mjs

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const UI = path.resolve(HERE, "..");
const WORK = fs.mkdtempSync(path.join(os.tmpdir(), "designer-original-download-acceptance-haiku-d1a14-"));
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
  restoreGlobals();
  try { fs.rmSync(WORK, { recursive: true, force: true }); } catch {}
}
function finish(code) { cleanup(); process.exit(code); }

// ---------- transpile the real production modules ----------------------------
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
]) emit(rel);
const featureRootRel = p => path.join(OUT, p);
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
stub("lib/api.js", `exports.ApiError = class ApiError extends Error { constructor(m, s, e) { super(m); this.status = s; this.code = e && e.code; } };\n`);
stub("lib/useLocation.js", `exports.navigate = () => {};\n`);
stub("features/workspace-apps/contextSelection.js", `exports.rememberContext = () => {};\n`);
stub("lib/sessionKey.js", `exports.sessionHeaders = () => ({ "X-Cadence-Session": "test-key" }); exports.sessionKey = () => "test-key";\n`);
stub("features/workspace-apps/screen/screenProjection.js",
  `exports.contextDefaultKeys = () => new Set(); exports.instagramLink = () => null;\n`);
stub("features/workspace-apps/socialConnect.js",
  `exports.accountLabel = s => s; exports.connectReturnTo = () => ""; exports.publicationBinding = () => null; exports.sendSlot = () => null;\n`);
stub("features/workspace-apps/screen/screenAssets.js",
  `exports.downscaleToDataUrl = async () => "data:image/png;base64,x";\n`);

emit("features/workspace-apps/socialPublish.ts");
emit("features/workspace-apps/workspaceApps.ts");
const { workspaceApps } = require(featureRootRel("features/workspace-apps/workspaceApps.js"));
Object.assign(workspaceApps, { bindingQuote: workspaceAppsStub.bindingQuote, startRun: workspaceAppsStub.startRun });

const { SlotController } = require(featureRootRel("features/workspace-apps/screen/screenSlot.js"));
const { makePlanner } = require(featureRootRel("features/workspace-apps/screen/screenActions.js"));

// Fixture ctx. The original-download qualification fields below are the
// baseline assumption (optional, may be absent in the real ActionContext); the
// planner ignoring them is an honest RED, not a fixture failure.
const BASE_CTX = {
  installation: { install_id: "i1", name: "app", title: "App", version: "1", summary: "", digest: "d".repeat(64), catalog_generation: "g", storage_kind: "workspace", project_link: null, approved: true, executable: true, approval: { state: "ok" }, guide: "", files: [], capabilities: null, connection_slots: [],
    workflows: [{ name: "create-image", label: "Create image", capability_slots: ["image"], inputs: [{ name: "brief" }] }] },
  installId: "i1", contextId: "ctx1", runs: [], onChanged: () => {},
};
const QUALIFIED_CTX = BASE_CTX;
const CTX_REF = { ctx: QUALIFIED_CTX };
const CTX = () => CTX_REF.ctx;

// ---------- Node effect spies (restored in cleanup) --------------------------
const realFetch = globalThis.fetch;
const realCreateObjectURL = URL.createObjectURL;
const realRevokeObjectURL = URL.revokeObjectURL;
const realDocument = globalThis.document;
const effects = { fetch: [], objectUrls: [], revoked: [], links: [], clicks: 0 };
let fetchHandler = null;
function restoreGlobals() {
  globalThis.fetch = realFetch;
  URL.createObjectURL = realCreateObjectURL;
  URL.revokeObjectURL = realRevokeObjectURL;
  if (realDocument === undefined) delete globalThis.document; else globalThis.document = realDocument;
}
globalThis.fetch = async (url, init) => {
  effects.fetch.push({ url: String(url), init });
  return fetchHandler ? fetchHandler(url, init) : new Response("{}", { status: 500 });
};
URL.createObjectURL = blob => { const u = `blob:node/${effects.objectUrls.length + 1}`; effects.objectUrls.push({ u, blob }); return u; };
URL.revokeObjectURL = u => { effects.revoked.push(u); };
globalThis.document = {
  createElement: tag => {
    const link = { tag, href: "", download: "", clicked: false, removed: false,
      click() { link.clicked = true; effects.clicks++; },
      remove() { link.removed = true; } };
    effects.links.push(link);
    return link;
  },
  body: { appendChild() {}, removeChild() {} },
};

// ---------- helpers ----------------------------------------------------------
const sent = [];
const views = [];
let now = 0;
function makeController(ctx = CTX, token = "tok-live-1") {
  CTX_REF.ctx = ctx();
  const plan = makePlanner(() => CTX_REF.ctx);
  const c = new SlotController(m => sent.push(m), v => views.push(v), (verb, args) => plan(verb, args, { actionToken: token }), () => now);
  controllers.push(c);
  return c;
}
const ANCHOR = { x: 0, y: 0, w: 80, h: 40 };
const ARGS = { workflow: "create-image", inputs: { brief: "square hero" } };
const DL_ARGS = { asset_id: "med-0123456789abcdef0123456789abcdef" };
const settle = (n = 20) => new Promise(r => setTimeout(r, n));
const doneFor = (id) => sent.find(m => m.op === "slot-state" && m.id === id && m.state === "done");
const resetEffects = () => {
  effects.fetch.length = 0; effects.objectUrls.length = 0; effects.revoked.length = 0;
  effects.links.length = 0; effects.clicks = 0; sent.length = 0; views.length = 0;
};
const PNG = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47]), Buffer.alloc(64, 1)]);

// ---------- OLD positive control: real run.start guards ----------------------
console.log("CONTROL. existing run.start planner + SlotController guards");
{
  resetEffects();
  const c = makeController(() => QUALIFIED_CTX);
  const before = sent.length;
  await c.request({ v: 2, op: "slot", id: "s1", verb: "run.start", args: ARGS, anchor: ANCHOR });
  check(calls.startRun.length === 0, "proposal performs no native start");
  const view = c.view();
  check(!!view && typeof view.token === "string", "controller exposes a live view token");
  if (view) {
    now = 600;
    check(c.tap(view.token, false, now) === false && calls.startRun.length === 0, "untrusted tap refuses");
    const c2 = makeController(() => QUALIFIED_CTX);
    await c2.request({ v: 2, op: "slot", id: "s2", verb: "run.start", args: ARGS, anchor: ANCHOR });
    const v2 = c2.view();
    if (v2) check(c2.tap(v2.token, true, now) === false && calls.startRun.length === 0, "trusted but EARLY tap refuses (500 ms press guard)");
    now += 600;
    check(c.tap(view.token, true, now) === true && calls.startRun.length === 1, "one legal trusted tap starts native run once");
    check(c.tap(view.token, true, now - 200) === false && calls.startRun.length === 1, "second tap while busy cannot start again");
    await settle();
    check(!!sent.slice(before).find(m => m.op === "slot-state" && m.id === "s1" && m.state === "done"), "completed run yields slot-state done");
  }
}

// ---------- NEW: qualified original download (planner verb) ------------------
console.log("NEW. media.original.download protected slot");
{
  resetEffects();
  now = 0;
  const c = makeController(() => QUALIFIED_CTX);
  let pending;
  fetchHandler = () => new Response(PNG, { status: 200, headers: { "content-type": "image/png" } });
  await c.request({ v: 2, op: "slot", id: "d1", verb: "media.original.download", args: DL_ARGS, anchor: ANCHOR });
  const v = c.view();
  check(!!v && v.label === "Download original", "qualified download advertises static host label Download original");
  check(effects.fetch.length === 0, "proposal performs no fetch");

  if (v) {
    now = 1000;
    check(c.tap(v.token, false, now) === false && effects.fetch.length === 0, "untrusted tap starts no download");
    const cEarly = makeController(() => QUALIFIED_CTX);
    await cEarly.request({ v: 2, op: "slot", id: "d2", verb: "media.original.download", args: DL_ARGS, anchor: ANCHOR });
    const vEarly = cEarly.view();
    if (vEarly) check(cEarly.tap(vEarly.token, true, now) === false && effects.fetch.length === 0, "trusted EARLY tap starts no download (500 ms press guard)");

    now = 1600;
    const tapOk = c.tap(v.token, true, now);
    check(tapOk === true, "one legal trusted tap accepted");
    pending = c.tap(v.token, true, now);
    check(pending === false, "second tap while busy is refused (one-shot)");
    await settle();

    const post = effects.fetch[0];
    check(effects.fetch.length === 1 && post && post.url.endsWith("/api/app-media/original") && post.init?.method === "POST", "exactly one POST to /api/app-media/original");
    if (post) {
      let body = null; try { body = JSON.parse(post.init.body); } catch {}
      check(body && Object.keys(body).sort().join(",") === "action_token,asset_id" && body.asset_id === DL_ARGS.asset_id && body.action_token === "tok-live-1", "POST body is exactly {action_token, asset_id}");
    }
    check(effects.objectUrls.length === 1 && effects.clicks === 1, "parent creates one Blob URL and one link click");
    const link = effects.links[0];
    check(!!link && /^med-0123456789abcdef0123456789abcdef\.png$/.test(link.download), "link name is native asset id + MIME extension");
    check(effects.revoked.length === 1 && link?.removed === true, "link and object URL released after click");
    const done = doneFor("d1");
    check(!!done && !("receipt" in done) && !("data" in done), "valid download completes with slot-state done and NO receipt");
    const frameText = JSON.stringify(sent);
    check(!/blob:|action_token|tok-live-1|sha256|provider|s3:|https?:\/\//.test(frameText), "no frame message carries bytes, Blob URL, token, ref, hash or locator");
  }
}

// ---------- NEW: refusals before any fetch ----------------------------------
console.log("NEW. bad selector / absent qualification");
{
  for (const [label, args] of [
    ["uppercase asset_id", { asset_id: "med-0123456789ABCDEF0123456789ABCDEF" }],
    ["short asset_id", { asset_id: "med-0123" }],
    ["extra action_token key", { asset_id: DL_ARGS.asset_id, action_token: "x" }],
    ["missing asset_id", {}],
  ]) {
    resetEffects();
    const c = makeController(() => QUALIFIED_CTX);
    await c.request({ v: 2, op: "slot", id: "b", verb: "media.original.download", args, anchor: ANCHOR });
    const refused = sent.find(m => m.op === "slot-state" && m.id === "b" && m.state === "refused");
    check(!!refused && c.view() === null && effects.fetch.length === 0, `bad selector refused (${label}): no view, no fetch`);
  }
  resetEffects();
  const c = makeController(() => QUALIFIED_CTX, "");
  await c.request({ v: 2, op: "slot", id: "u", verb: "media.original.download", args: DL_ARGS, anchor: ANCHOR });
  check(c.view() === null && effects.fetch.length === 0 && sent.some(m => m.id === "u" && m.state === "refused"), "empty action token: download refused, no slot, no fetch");
}

// ---------- NEW: stale capture, bad responses, no handoff --------------------
console.log("NEW. stale / failed responses leave no link");
{
  // stale: closed while the fetch is in flight, then a valid response arrives
  resetEffects();
  now = 0;
  const c = makeController(() => QUALIFIED_CTX);
  await c.request({ v: 2, op: "slot", id: "st", verb: "media.original.download", args: DL_ARGS, anchor: ANCHOR });
  const v = c.view();
  let release;
  fetchHandler = () => new Promise(res => { release = () => res(new Response(PNG, { status: 200, headers: { "content-type": "image/png" } })); });
  now = 1000;
  if (v) c.tap(v.token, true, now);
  await settle(5);
  c.close();
  release?.();
  await settle();
  check(effects.clicks === 0 && effects.objectUrls.length === 0 && !doneFor("st"), "stale scope after close: no Blob URL, no link click, no done");

  const badCases = [
    ["HTTP 403 error", () => new Response(JSON.stringify({ error: "forbidden" }), { status: 403, headers: { "content-type": "application/json" } })],
    ["wrong MIME text/html", () => new Response("<html>", { status: 200, headers: { "content-type": "text/html" } })],
    ["oversize over 10 MiB", () => new Response(Buffer.alloc(10 * 1024 * 1024 + 1, 1), { status: 200, headers: { "content-type": "image/png" } })],
  ];
  for (const [label, make] of badCases) {
    resetEffects();
    now = 0;
    const cb = makeController(() => QUALIFIED_CTX);
    await cb.request({ v: 2, op: "slot", id: "bad", verb: "media.original.download", args: DL_ARGS, anchor: ANCHOR });
    const vb = cb.view();
    fetchHandler = () => Promise.resolve(make());
    now = 1000;
    if (vb) cb.tap(vb.token, true, now);
    await settle();
    check(effects.clicks === 0 && effects.objectUrls.length === 0 && !doneFor("bad"), `${label}: no Browser link, no done claim`);
  }
}

restoreGlobals();
console.log(`\n${total - failures}/${total} ok, ${failures} FAIL`);
finish(failures === 0 ? 0 : 1);
