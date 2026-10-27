// CAD-1365 guarded media bridge — INDEPENDENT guard acceptance driver.
// Authored against design-plans/designer-media-bridge-v1.md, not against any
// implementation. It compiles and loads the REAL production modules
// (screenProtocol.ts / screenLifecycle.ts) with the pinned ui TypeScript, then
// drives the real ScreenChannel through a real node:worker_threads
// MessageChannel port pair with a controlled ScreenActions dispatch spy.
//
// Scope: this is a transport check — a spy proves the host dispatch/gating,
// not server authorization (the immutable native/HTTP media checks own that).
// On the current baseline (no media bridge implemented) the driver must FAIL
// on a genuine baseline RED — a missing media field in parseMount, an unknown
// media.* verb or an unknown screen-media.v1 opt-in refused by the real
// parser. That RED is the expected clean result, distinct from a driver or
// transpile error.
//
// Run (from the worktree root, inside a build slot):
//   node ui/acceptance/cad1365-media-bridge.mjs

import { createRequire } from "node:module";
import { MessageChannel } from "node:worker_threads";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const UI = path.resolve(HERE, "..");
const WORK = fs.mkdtempSync(path.join(os.tmpdir(), "designer-media-bridge-acceptance-haiku-d1a14-"));
const OUT = path.join(WORK, "out");
fs.mkdirSync(OUT, { recursive: true });

let failures = 0;
function check(value, label) {
  if (value) { console.log(`  ok    ${label}`); return true; }
  failures++; console.log(`  FAIL  ${label}`); return false;
}
// Every channel/port/scratch dir this driver creates, closed by name in `finally`.
const opened = [];
function cleanup() {
  for (const m of opened.splice(0)) {
    try { m.channel.close(); } catch {}
    try { m.child.close(); } catch {}
  }
  try { fs.rmSync(WORK, { recursive: true, force: true }); } catch {}
}
function finish(code) { cleanup(); process.exit(code); }

// ---------- compile the real production modules with the pinned toolchain ----
// The worktree carries no node_modules; the pinned ui install in the main
// checkout supplies tsrs (TypeScript 7). Only screenProtocol.ts and
// screenLifecycle.ts are compiled — both are leaf modules with no imports
// outside themselves, preserving the real classes.
const require = createRequire(path.join(UI, "package.json"));
let tscBin;
try { tscBin = path.join(path.dirname(require.resolve("tsrs/package.json")), "bin", "tsrs"); }
catch { tscBin = "/home/ubuntu/Project/cadence/ui/node_modules/tsrs/bin/tsrs"; }
const srcDir = path.join(UI, "src", "features", "workspace-apps", "screen");
const tsc = spawnSync(process.execPath, [tscBin, "--ignoreConfig",
  "--target", "ES2022", "--module", "commonjs", "--moduleResolution", "bundler",
  "--strict", "--skipLibCheck", "--esModuleInterop", "--lib", "ES2023,DOM",
  "--outDir", OUT, "--rootDir", srcDir,
  path.join(srcDir, "screenProtocol.ts"), path.join(srcDir, "screenSlot.ts"),
  path.join(srcDir, "screenLifecycle.ts")],
  { encoding: "utf8" });
if (tsc.status !== 0) {
  console.error("DRIVER ERROR — production modules did not transpile (this is a driver/toolchain error, not baseline RED):");
  console.error(tsc.stdout); console.error(tsc.stderr);
  finish(2);
}
const load = createRequire(path.join(OUT, "index.js"));
const protocol = load(path.join(OUT, "screenProtocol.js"));
const lifecycle = load(path.join(OUT, "screenLifecycle.js"));
const { parseChild } = protocol;
const { parseMount, ScreenChannel } = lifecycle;

// ---------- fixtures ---------------------------------------------------------
const NONCE = "a".repeat(64);
const CAP = "b".repeat(64);
const TOKEN = "c".repeat(64);
const RECEIPT_BASE = { mount: `/api/app-screen/${CAP}`, bridge_nonce: NONCE, generation: 2, tag: "main" };
const INIT = { v: 1, op: "init", tag: "main", bridge_nonce: NONCE, generation: 2 };
const MEDIA_RECEIPT = { contract: "screen-media.v1", may: ["media.read", "media.publish"] };
// Canonical native asset id grammar (designer-media-publish-contract-v1):
// `med-` + 32 lowercase hex. Request ids: ASCII letters/digits/-/_ ≤ 128.
const ASSET = `med-${"1".repeat(32)}`;
const ASSET_B = `med-${"2".repeat(32)}`;
const REQ = "req-cad1365-bridge";
const PUSH = { v: 1, op: "screen", install_id: "i", digest: "d", tag: "main", context_id: "c",
  installation: { install_id: "i", digest: "d", title: "t", name: "n", version: "v", summary: "" },
  contexts: [{ id: "c", label: "context" }], runs: [], outbox: [], now: 1,
  publish_intents: { status: "ok", withheld: 0, rows: [] },
  defaults: { context_id: "c", revision: 0, values: {} }, readiness: { ok: true, blockers: [] },
  actions: ["media.read", "media.publish"] };

const flush = () => new Promise(resolve => setTimeout(resolve, 25));

// One mount on a REAL MessageChannel pair: `childPort` is what the child's code
// would hold; the channel owns `port1` after the init event.
function mount(receiptExtra = {}, push = PUSH, spy = null, source = {}) {
  const receipt = { ...RECEIPT_BASE, ...receiptExtra };
  const parsed = parseMount(receipt, "main");
  if (!parsed) return { parsed: null };
  const { port1, port2 } = new MessageChannel();
  const sent = [];
  port2.on("message", msg => sent.push(msg));
  const counts = { removed: 0, failed: 0, ready: 0 };
  const channel = new ScreenChannel(source, parsed, push,
    () => ++counts.removed, () => ++counts.failed, () => ++counts.ready,
    undefined, spy ?? undefined);
  channel.receive({ source, origin: "null", data: INIT, ports: [port1] });
  const m = { parsed, channel, child: port2, sent, counts };
  opened.push(m);
  return m;
}
// A dispatch spy implementing the ScreenActions surface. The contract: the
// media bridge must reach the REAL dispatcher call path with the host-held
// action token — recorded here as `ui.actionToken`.
function makeSpy() {
  const calls = [];
  return {
    calls,
    call: async (verb, args, ui) => { calls.push({ verb, args, actionToken: ui?.actionToken }); return { ok: true, data: { echo: verb } }; },
    planner: async () => ({ code: "unplanned", text: "not planned" }),
    onSlot: () => {}, onLink: () => {},
  };
}

// ---------- Group A: strict parsing of the new host-held receipt field -------
console.log("A. mount receipt media declaration (real parseMount)");
check(!parseMount({ ...RECEIPT_BASE, media: "screen-media.v1" }, "main"),
  "receipt media as bare string refuses (malformed metadata must not parse)");
check(!parseMount({ ...RECEIPT_BASE, media: { contract: "screen-media.v1" } }, "main"),
  "receipt media without may[] refuses");
check(!parseMount({ ...RECEIPT_BASE, media: { contract: "screen-media.v1", may: ["media.read"], extra: 1 } }, "main"),
  "receipt media with an unknown field refuses (no silent strip)");
check(!parseMount({ ...RECEIPT_BASE, media: { contract: "screen-media.v1", may: ["media.read", "media.read"] } }, "main"),
  "receipt media with duplicate permissions refuses");
check(!parseMount({ ...RECEIPT_BASE, media: { contract: "screen-media.v1", may: ["media.read", "admin.grant"] } }, "main"),
  "receipt media naming an undeclared permission refuses");
check(!parseMount({ ...RECEIPT_BASE, media: { contract: "screen-media.v2", may: ["media.read"] } }, "main"),
  "receipt media with a foreign contract refuses");
check(!parseMount({ ...RECEIPT_BASE, media: { contract: "screen-media.v1", may: [] } }, "main"),
  "receipt media with an empty permission set refuses");
const parsedMedia = parseMount({ ...RECEIPT_BASE, action_token: TOKEN, media: MEDIA_RECEIPT }, "main");
check(!!parsedMedia, "receipt with media {contract:screen-media.v1, may:[read,publish]} parses");
check(parsedMedia && JSON.stringify(parsedMedia.media) === JSON.stringify(MEDIA_RECEIPT),
  "parsed receipt preserves the declared media permissions verbatim");

// ---------- Group B: closed verb set / opt-in on the real parser -------------
console.log("B. real parseChild verb + opt-in gates");
check(!!parseChild({ v: 2, op: "call", id: "m1", verb: "media.read", args: { kind: "catalog" } }),
  "call verb media.read parses on the closed call envelope");
check(!!parseChild({ v: 2, op: "call", id: "m2", verb: "media.publish", args: { asset_id: ASSET, expected_revision: 1, request_id: REQ } }),
  "call verb media.publish parses with a canonical med- asset id");
check(!parseChild({ v: 2, op: "call", id: "m3", verb: "media.generate", args: {} }),
  "unadmitted media verb (generation scope) refuses");
check(!parseChild({ v: 1, op: "ready", accepts: ["screen.v2", "screen-media.v9"] }),
  "foreign media contract opt-in refuses");
check(!!parseChild({ v: 1, op: "ready", accepts: ["screen.v2", "screen-media.v1"] }),
  "child opt-in accepts screen.v2 + screen-media.v1");
// Malformed media metadata in args must refuse, never silently strip.
check(!parseChild({ v: 2, op: "call", id: "m4", verb: "media.read", args: { kind: "catalog", cursor: 7 } }),
  "media.read with non-string cursor refuses (no silent strip)");
check(!parseChild({ v: 2, op: "call", id: "m5", verb: "media.read", args: { kind: "detail", asset_id: ASSET } }),
  "media.read with an unadmitted kind refuses");
check(!parseChild({ v: 2, op: "call", id: "m6", verb: "media.publish", args: { asset_id: ASSET, expected_revision: 1 } }),
  "media.publish missing request_id refuses");
check(!parseChild({ v: 2, op: "call", id: "m7", verb: "media.publish",
    args: { asset_id: ASSET, expected_revision: 1, request_id: REQ, url: "https://evil.example" } }),
  "media.publish carrying a frame-supplied URL refuses");

// ---------- Group C: positive negotiated dispatch ---------------------------
console.log("C. negotiated + declared media rights dispatch to the real call path");
try {
  const spy = makeSpy();
  const m = mount({ action_token: TOKEN, media: MEDIA_RECEIPT }, PUSH, spy);
  check(!!m.parsed, "media receipt accepted by channel mount");
  if (!m.parsed) throw new Error("receipt refused: skipping positive dispatch");
  m.child.postMessage({ v: 1, op: "ready", accepts: ["screen.v2", "screen-media.v1"] });
  await flush();
  check(m.sent.some(x => x && x.op === "screen" && x.v === 2), "negotiated v2 push after media opt-in");
  m.child.postMessage({ v: 2, op: "call", id: "r1", verb: "media.read", args: { kind: "catalog", limit: 5 } });
  m.child.postMessage({ v: 2, op: "call", id: "p1", verb: "media.publish", args: { asset_id: ASSET, expected_revision: 3, request_id: REQ } });
  await flush();
  const read = spy.calls.find(c => c.verb === "media.read");
  const pub = spy.calls.find(c => c.verb === "media.publish");
  check(!!read && read.args.kind === "catalog" && read.args.limit === 5, "media.read reached dispatcher with exact catalog args");
  check(!!pub && pub.args.asset_id === ASSET && pub.args.expected_revision === 3 && pub.args.request_id === REQ,
    "media.publish reached dispatcher with the canonical publish intent (med- id)");
  check(!!read && read.actionToken === TOKEN && !!pub && pub.actionToken === TOKEN,
    "dispatcher received the host-held mount action token, never the frame's");
  check(m.sent.some(x => x && x.op === "reply" && x.id === "r1" && x.ok === true) &&
        m.sent.some(x => x && x.op === "reply" && x.id === "p1" && x.ok === true),
    "bounded replies returned on the real channel");
  check(m.counts.failed === 0, "media mount stayed up through positive dispatch");
} catch (error) { failures++; console.log(`  FAIL  positive dispatch aborted: ${error.message}`); }

// ---------- Group D: refusal before dispatch ----------------------------------
console.log("D. denied cases: spy dispatch count must stay 0");
async function denied(label, receiptExtra, accepts, call, push = PUSH) {
  const spy = makeSpy();
  const m = mount(receiptExtra, push, spy);
  if (!m.parsed) { check(false, `${label}: mount receipt unexpectedly refused`); return; }
  m.child.postMessage({ v: 1, op: "ready", accepts });
  await flush();
  m.child.postMessage(call);
  await flush();
  check(spy.calls.length === 0, `${label}: no dispatch`);
  check(!m.sent.some(x => x && x.op === "reply" && x.ok === true),
    `${label}: no ok reply to a denied media call`);
}
await denied("read-only declaration rejects media.publish",
  { action_token: TOKEN, media: { contract: "screen-media.v1", may: ["media.read"] } },
  ["screen.v2", "screen-media.v1"],
  { v: 2, op: "call", id: "d1", verb: "media.publish", args: { asset_id: ASSET_B, expected_revision: 1, request_id: REQ } });
await denied("legacy receipt (no media) rejects media.read",
  { action_token: TOKEN },
  ["screen.v2"],
  { v: 2, op: "call", id: "d2", verb: "media.read", args: { kind: "catalog" } });
await denied("unnegotiated child (v2 only, no screen-media.v1 opt-in) rejects media.read",
  { action_token: TOKEN, media: MEDIA_RECEIPT },
  ["screen.v2"],
  { v: 2, op: "call", id: "d3", verb: "media.read", args: { kind: "catalog" } });
await denied("media receipt without action_token cannot dispatch",
  { media: MEDIA_RECEIPT },
  ["screen.v2", "screen-media.v1"],
  { v: 2, op: "call", id: "d4", verb: "media.read", args: { kind: "catalog" } });
await denied("non-operator projection (empty actions) cannot dispatch media.read",
  { action_token: TOKEN, media: MEDIA_RECEIPT },
  ["screen.v2", "screen-media.v1"],
  { v: 2, op: "call", id: "d5", verb: "media.read", args: { kind: "catalog" } },
  { ...PUSH, actions: [] });

// Forged authority fields in args: the frozen contract is CLOSED — a call
// carrying body-authority selectors is refused outright, never silently
// stripped or merged. Dispatch must not happen and no ok reply may flow.
try {
  const spy = makeSpy();
  const m = mount({ action_token: TOKEN, media: MEDIA_RECEIPT }, PUSH, spy);
  if (!m.parsed) throw new Error("receipt refused");
  m.child.postMessage({ v: 1, op: "ready", accepts: ["screen.v2", "screen-media.v1"] });
  await flush();
  for (const [i, field] of ["action_token", "token", "key", "origin", "workspace", "creator", "actor"].entries()) {
    m.child.postMessage({ v: 2, op: "call", id: `f${i}`, verb: "media.publish",
      args: { asset_id: ASSET, expected_revision: 1, request_id: REQ, [field]: field === "origin" ? "https://evil.example" : "forged" } });
  }
  await flush();
  check(spy.calls.length === 0, "a media call carrying any body-authority selector is refused — never dispatched or silently stripped");
  check(!m.sent.some(x => x && x.op === "reply" && /^f\d$/.test(x.id) && x.ok === true),
    "no ok reply to a forged-authority media call");
} catch (error) { failures++; console.log(`  FAIL  forged-authority check aborted: ${error.message}`); }

// ---------- Group E: closed/superseded reply controls (existing channel) ------
console.log("E. teardown: no late reply after close");
try {
  // Deferred dispatcher: `started` resolves the moment the spy is invoked,
  // so the close happens only after the call is genuinely in flight.
  let started, release;
  const startedP = new Promise(r => { started = r; });
  const gate = new Promise(r => { release = r; });
  const spy = { calls: [], call: async (verb, args, ui) => { spy.calls.push({ verb, args, actionToken: ui?.actionToken });
      started(); await gate; return { ok: true, data: {} }; },
    planner: async () => ({ code: "x", text: "x" }), onSlot: () => {}, onLink: () => {} };
  const m = mount({ action_token: TOKEN, media: MEDIA_RECEIPT }, PUSH, spy);
  if (!m.parsed) throw new Error("receipt refused");
  m.child.postMessage({ v: 1, op: "ready", accepts: ["screen.v2", "screen-media.v1"] });
  await flush();
  m.child.postMessage({ v: 2, op: "call", id: "s1", verb: "media.read", args: { kind: "catalog" } });
  const dispatched = await Promise.race([startedP.then(() => true), new Promise(r => setTimeout(() => r(false), 500))]);
  check(dispatched && spy.calls.length === 1, "in-flight call dispatched once");
  if (dispatched) {
    m.channel.close();
    release();
    await flush();
    check(!m.sent.some(x => x && x.op === "reply" && x.id === "s1"), "a destroyed mount receives no late reply");
  } else { release(); failures++; console.log("  FAIL  teardown check aborted: call never dispatched (no timing assumption)"); }
} catch (error) { failures++; console.log(`  FAIL  teardown check aborted: ${error.message}`); }

// ---------- result -------------------------------------------------------------
console.log(`\ncad1365-media-bridge: ${failures === 0 ? "ALL CHECKS PASS" : `${failures} check(s) FAILED`}`);
if (failures) {
  console.log("On the un-implemented baseline the failures above are the expected clean RED:");
  console.log("parseMount rejecting `media`, parseChild rejecting media.* verbs / screen-media.v1,");
  console.log("or no media dispatch path in ScreenChannel. Re-run after the bridge lands.");
}
finish(failures === 0 ? 0 : 1);
