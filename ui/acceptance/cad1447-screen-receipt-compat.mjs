// CAD-1447 — released-Social screen compatibility for the run-receipt marker.
// INDEPENDENT guard acceptance (authored by cad1447-swe2-spec-858f0, not the
// implementer). It transpiles the REAL product modules — screenProtocol.ts,
// screenSlot.ts, screenLifecycle.ts — with the lane's pinned ui TypeScript
// into a short-lived temp dir, then drives the REAL ScreenChannel through a
// real node:worker_threads MessageChannel pair: window-init -> port ready ->
// host PUSH. It asserts only on keys the real host emits, never on a fake
// host conditional.
//
// Acceptance (from CAD-1447, not from the implementation):
//   * A legacy child opting into ONLY screen.v2 + screen-actions.v1 (the
//     released Social 0.9.9 handshake) must receive NO `run_receipts` field
//     even with an operator planner and `run.start` in the projected action
//     list — its strict closed receiver would otherwise refuse the push.
//   * An explicitly opted-in screen-media.v1 child (the Designer/media
//     contract CAD-1365 shipped) still receives `run_receipts: 1` under the
//     unchanged real conditions: planner present AND `run.start` projected.
//   * A forged `run_receipts` on the incoming projection is discarded; a
//     media opt-in without planner or without `run.start` gets no marker;
//     an unknown accepts token or an extra child field still closes.
//
// Baseline note: on the unpatched baseline (d0944b98) the very first group
// goes genuinely RED — the real host emits `run_receipts: 1` to a child
// that never negotiated it.
//
// Run (from the worktree root, inside a build slot):
//   cadence build-slot run test -- node ui/acceptance/cad1447-screen-receipt-compat.mjs

import { createRequire } from "node:module";
import { MessageChannel } from "node:worker_threads";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const UI = path.resolve(HERE, "..");
const WORK = fs.mkdtempSync(path.join(os.tmpdir(), "cad1447-screen-receipt-"));
const OUT = path.join(WORK, "out");
fs.mkdirSync(OUT, { recursive: true });

let failures = 0;
function check(value, label) {
  if (value) { console.log(`  ok    ${label}`); return true; }
  failures++; console.log(`  FAIL  ${label}`); return false;
}
const opened = [];
function cleanup() {
  for (const m of opened.splice(0)) {
    try { m.channel.close(); } catch {}
    try { m.child.close(); } catch {}
  }
  try { fs.rmSync(WORK, { recursive: true, force: true }); } catch {}
}
function finish(code) { cleanup(); process.exit(code); }
process.on("uncaughtException", e => { console.error(`DRIVER ERROR: ${e.stack}`); finish(2); });
process.on("unhandledRejection", e => { console.error(`DRIVER ERROR: ${e?.stack ?? e}`); finish(2); });

// ---------- transpile the real production modules with the pinned toolchain --
const require = createRequire(path.join(UI, "package.json"));
let tscBin;
try { tscBin = path.join(path.dirname(require.resolve("tsrs/package.json")), "bin", "tsrs"); }
catch (e) { console.error(`DRIVER ERROR — pinned ui tsrs not resolvable: ${e.message}`); finish(2); }
const srcDir = path.join(UI, "src", "features", "workspace-apps", "screen");
const tsc = spawnSync(process.execPath, [tscBin, "--ignoreConfig",
  "--target", "ES2022", "--module", "commonjs", "--moduleResolution", "bundler",
  "--strict", "--skipLibCheck", "--esModuleInterop", "--lib", "ES2023,DOM",
  "--outDir", OUT, "--rootDir", srcDir,
  path.join(srcDir, "screenProtocol.ts"), path.join(srcDir, "screenSlot.ts"),
  path.join(srcDir, "screenLifecycle.ts")],
  { encoding: "utf8" });
if (tsc.status !== 0) {
  console.error("DRIVER ERROR — production modules did not transpile (toolchain error, not baseline RED):");
  console.error(tsc.stdout); console.error(tsc.stderr);
  finish(2);
}
const load = createRequire(path.join(OUT, "index.js"));
const protocol = load(path.join(OUT, "screenProtocol.js"));
const lifecycle = load(path.join(OUT, "screenLifecycle.js"));
const { parseChild } = protocol;
const { parseMount, ScreenChannel } = lifecycle;

// ---------- fixtures ----------------------------------------------------------
const NONCE = "a".repeat(64);
const CAP = "b".repeat(64);
const TOKEN = "c".repeat(64);
const RECEIPT_BASE = { mount: `/api/app-screen/${CAP}`, bridge_nonce: NONCE, generation: 2, tag: "main" };
const INIT = { v: 1, op: "init", tag: "main", bridge_nonce: NONCE, generation: 2 };
const MEDIA_DECL = { contract: "screen-media.v1", may: ["media.read", "media.publish"] };
// A v2 projection like the operator board's screenProjection emits: nonempty
// generic action list including the spend verb the marker qualifies on.
const PUSH = { v: 1, op: "screen", install_id: "i", digest: "d", tag: "main", context_id: "c",
  installation: { install_id: "i", digest: "d", title: "t", name: "n", version: "v", summary: "" },
  contexts: [{ id: "c", label: "context" }],
  runs: [{ id: "r1", state: "ready", title: "t", snapshot_digest: "s", context_id: "c",
    workflow: { title: "t" }, phase: "ready" }],
  outbox: [{ effect_id: "e1", state: "done", title: "t", run_id: "r1", context_id: "c" }],
  now: 1,
  publish_intents: { status: "ok", withheld: 0, rows: [] },
  defaults: { context_id: "c", revision: 0, values: {} },
  readiness: { ok: true, blockers: [] },
  actions: ["read.run", "run.start"] };
const RELEASED_SOCIAL_ACCEPTS = ["screen.v2", "screen-actions.v1"];
const DESIGNER_MEDIA_ACCEPTS = ["screen.v2", "screen-media.v1"];
// Wait for actual delivery (or a real channel refusal), not a timing guess.
async function flush(m) {
  let timer;
  try {
    await Promise.race([m.settled, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("channel neither pushed nor refused within 2s")), 2000);
    })]);
  } finally { clearTimeout(timer); }
}
if (!protocol.shapeFor(PUSH, "v2")) throw new Error("invalid acceptance PUSH fixture");

// A real ScreenActions surface with a REAL planner function (not a fake host:
// the channel itself decides the marker; the spy only proves what it emits).
function makeActions() {
  return {
    calls: [],
    call: async (verb) => { return { ok: true, data: { echo: verb } }; },
    planner: async () => ({ code: "unplanned", text: "not planned" }),
    onSlot: () => {}, onLink: () => {},
  };
}

// Mount the real channel on a real port pair. `receiptExtra` adds host-held
// minted fields (action_token / media declaration); the returned `sent`
// collects everything the host actually posted to the child.
function mount(receiptExtra = {}, push = PUSH, actions = makeActions()) {
  const receipt = { ...RECEIPT_BASE, ...receiptExtra };
  const parsed = parseMount(receipt, "main");
  if (!parsed) return { parsed: null };
  const { port1, port2 } = new MessageChannel();
  const sent = [];
  let settle;
  const settled = new Promise(resolve => { settle = resolve; });
  port2.on("message", msg => { sent.push(msg); settle(); });
  const counts = { removed: 0, failed: 0, ready: 0 };
  const source = {};
  const channel = new ScreenChannel(source, parsed, push,
    () => ++counts.removed, () => { ++counts.failed; settle(); }, () => ++counts.ready,
    undefined, actions);
  channel.receive({ source, origin: "null", data: INIT, ports: [port1] });
  const m = { parsed, channel, child: port2, sent, counts, settled };
  opened.push(m);
  return m;
}
const lastPush = m => m.sent.filter(x => x && x.op === "screen").at(-1);

// ---------- 1. released-Social handshake must not get the extension -----------
console.log("1. legacy screen.v2+screen-actions.v1 child (released Social 0.9.9)");
{
  const m = mount({ action_token: TOKEN });
  check(!!m.parsed, "operator mount receipt parses");
  if (m.parsed) {
    m.child.postMessage({ v: 1, op: "ready", accepts: RELEASED_SOCIAL_ACCEPTS });
    await flush(m);
    const push = lastPush(m);
    check(!!push && push.v === 2 && m.counts.ready === 1 && m.counts.failed === 0,
      "legacy child completes ready and receives a v2 push without closing");
    check(!!push && !("run_receipts" in push),
      "no run_receipts field reaches a child that only negotiated screen-actions.v1");
    // The strict closed receiver treats ANY unknown root key as a refusal;
    // prove no other host-derived extension leaked either.
    check(!!push && !("original_downloads" in push),
      "no original_downloads field reaches the non-media child either");
    check(!!push && Array.isArray(push.actions) && push.actions.includes("run.start"),
      "the operator action list itself is still delivered (only the extension is gated)");
  }
}

// ---------- 2. opted-in Designer/media child keeps the marker -----------------
console.log("2. explicitly opted-in screen-media.v1 child (Designer/media)");
{
  const m = mount({ action_token: TOKEN, media: MEDIA_DECL });
  check(!!m.parsed, "media mount receipt parses");
  if (m.parsed) {
    m.child.postMessage({ v: 1, op: "ready", accepts: DESIGNER_MEDIA_ACCEPTS });
    await flush(m);
    const push = lastPush(m);
    check(!!push && push.v === 2 && m.counts.ready === 1,
      "media child completes ready and receives a v2 push");
    check(!!push && push.run_receipts === 1,
      "opted-in child still receives run_receipts:1 with planner + run.start");
  }
}

// ---------- 3. the marker stays host-derived, never frame-controlled ----------
console.log("3. marker derivation stays strict (forged input, planner, run.start)");
{
  // A forged run_receipts on the incoming projection is discarded even on
  // an opted-in mount — the child can only ever see the host-derived value.
  const forged = mount({ action_token: TOKEN, media: MEDIA_DECL },
    { ...PUSH, run_receipts: 7, original_downloads: 9 });
  check(!!forged.parsed, "projection carrying forged markers still mounts");
  if (forged.parsed) {
    forged.child.postMessage({ v: 1, op: "ready", accepts: DESIGNER_MEDIA_ACCEPTS });
    await flush(forged);
    const push = lastPush(forged);
    check(!!push && push.run_receipts === 1,
      "forged run_receipts on the projection is overwritten by host derivation, not forwarded");
  }
  // Media opt-in but NO planner on this mount: no marker.
  const noPlanner = mount({ action_token: TOKEN, media: MEDIA_DECL }, { ...PUSH, run_receipts: 1 },
    { call: async () => ({ ok: true }), onSlot: () => {}, onLink: () => {} });
  check(!!noPlanner.parsed, "plannerless media mount parses");
  if (noPlanner.parsed) {
    noPlanner.child.postMessage({ v: 1, op: "ready", accepts: DESIGNER_MEDIA_ACCEPTS });
    await flush(noPlanner);
    const push = lastPush(noPlanner);
    check(!!push && !("run_receipts" in push),
      "media opt-in without a real planner receives no run_receipts");
  }
  // Planner present but run.start NOT in the projected actions: no marker.
  const noRunStart = mount({ action_token: TOKEN, media: MEDIA_DECL },
    { ...PUSH, actions: ["read.run"], run_receipts: 1 });
  check(!!noRunStart.parsed, "run.start-less projection mounts");
  if (noRunStart.parsed) {
    noRunStart.child.postMessage({ v: 1, op: "ready", accepts: DESIGNER_MEDIA_ACCEPTS });
    await flush(noRunStart);
    const push = lastPush(noRunStart);
    check(!!push && !("run_receipts" in push),
      "media opt-in without a projected run.start action receives no run_receipts");
  }
}

// ---------- 4. strict child vocabulary stays fail-closed ----------------------
console.log("4. closed handshake vocabulary unchanged");
{
  check(!parseChild({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1", "run-receipts.v1"] }),
    "an invented run-receipts opt-in token is refused by the real parser");
  check(!parseChild({ v: 1, op: "ready", accepts: ["screen.v2", "screen-actions.v1", "screen-media.v1", "extra.v1"] }),
    "accepts beyond the closed triple bound is refused");
  const forgedField = mount({ action_token: TOKEN });
  forgedField.child.postMessage({ v: 1, op: "ready", accepts: RELEASED_SOCIAL_ACCEPTS, run_receipts: 1 });
  await flush(forgedField);
  check(forgedField.counts.failed === 1 && forgedField.sent.length === 0,
    "a ready carrying an extra forged field closes the mount with nothing sent");
  // An operator-less projection (empty actions) never derives the marker even
  // for a fully opted-in child.
  const viewer = mount({ action_token: TOKEN, media: MEDIA_DECL },
    { ...PUSH, actions: [] });
  check(!!viewer.parsed, "empty-actions mount parses");
  if (viewer.parsed) {
    viewer.child.postMessage({ v: 1, op: "ready", accepts: DESIGNER_MEDIA_ACCEPTS });
    await flush(viewer);
    const push = lastPush(viewer);
    check(!!push && !("run_receipts" in push) && !("original_downloads" in push),
      "a non-operator projection emits no host-derived extension markers");
  }
}

// ---------- result -------------------------------------------------------------
console.log(`\ncad1447-screen-receipt-compat: ${failures === 0 ? "ALL CHECKS PASS" : `${failures} check(s) FAILED`}`);
if (failures) {
  console.log("On the unpatched baseline the group-1 failures are the expected clean RED:");
  console.log("the real host emits run_receipts to a child that only negotiated");
  console.log("screen.v2+screen-actions.v1 (released Social), which its strict");
  console.log("receiver refuses. Re-run after the wantsMedia gate lands.");
}
finish(failures === 0 ? 0 : 1);
