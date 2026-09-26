/**
 * CAD-571 N4: the operator-only ledger reads are fetched only when the
 * board proves the operator — an unproven viewer makes no request at
 * all, so no 403 lands in the console (CAD-563 r2). The guard lives in
 * the component source, and these UI tests run in plain node with no
 * DOM, so the pin reads the components as text: every mention of an
 * operator-only store must sit on the guarded line.
 */
declare const require: (name: string) => { readFileSync: (p: string, enc: string) => string };

const fs = require("fs");
const source = (rel: string) => fs.readFileSync(rel, "utf8");

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

/** Every line naming `needle` must also carry the proof `guard`. */
function guarded(text: string, needle: string, guard: string, what: string): void {
  let seen = 0;
  for (const line of text.split("\n")) {
    if (!line.includes(needle)) continue;
    seen += 1;
    equal(line.includes(guard), true, `${what} guarded: ${line.trim()}`);
  }
  equal(seen > 0, true, `${what} names the store`);
}

// The Outbox — `GET /api/outbox` is the operator's read.
const outbox = source("src/features/outbox/Outbox.tsx");
guarded(outbox, "resources.outbox", "operator ?", "Outbox");
equal(outbox.includes("api.outbox"), false, "Outbox goes through the guarded store");

// The app page — `GET /api/apps/<project>/<name>/outputs` is the same
// operator-only ledger, narrowed to the app's runs.
const appDetail = source("src/features/apps/AppDetail.tsx");
guarded(appDetail, "resources.appOutputs(", "viewer.operator ?", "AppDetail");
equal(appDetail.includes("api.appOutputs"), false, "AppDetail goes through the guarded store");

console.log("operator-fetch checks passed");
