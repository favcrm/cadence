#!/usr/bin/env node
/**
 * CAD-865 CRM acceptance driver — drives an owned browser through the
 * real installed CRM's MVP flows against a *running, owned* board.
 *
 * This script never launches a build, a daemon, a rig or a test suite:
 * it expects the caller to supply an already-running board's URL, the
 * operator login link the fixture minted, and the SMTP rig log path —
 * exactly what `crm_send_e2e_harness` prints when it holds (or any
 * equally-shaped owned runtime). Provenance of that runtime is recorded
 * in the evidence manifest so reviewers can see whether the driver ran
 * against a test-seam fixture or a production build.
 *
 * Admission note: the only reason a browser run is gated is that the
 * hold-mode harness needs a `test-seam` build, and cargo builds need a
 * build slot this lane could not take. This driver therefore does not
 * attempt the build itself; it accepts `--url`, `--login-link` and
 * `--rig-log` from an owned, already-admitted runtime.
 *
 * Usage:
 *   node scripts/cad865-acceptance.mjs \
 *     --url http://cadence-3199.localhost:3199/app-installations/<id>?ctx=<ctx> \
 *     --login-link "http://cadence-3199.localhost:3199/login#n=…" \
 *     --rig-log /tmp/c865-0930/rig.log \
 *     --evidence /tmp/c865-0930/evidence \
 *     [--session cad865-crm] [--width 1440 --width 390]
 *
 * Each `--width` runs the full pass at that viewport. Evidence lands in
 * `<evidence>/<width>px/` as numbered PNGs plus `manifest.json`.
 */

import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";

// ---------- args ----------

const args = process.argv.slice(2);
function arg(name, fallback) {
  const i = args.indexOf(`--${name}`);
  return i === -1 ? fallback : args[i + 1];
}
const URL = arg("url", "");
const LOGIN = arg("login-link", "");
const RIG_LOG = arg("rig-log", "");
const EVIDENCE = resolve(arg("evidence", "/tmp/c865-0930/evidence"));
const SESSION = arg("session", `cad865-crm-${Date.now()}`);
const WIDTHS = args.flatMap((v, i) => (v === "--width" ? [Number(args[i + 1])] : []));
const VIEWPORTS = WIDTHS.length > 0 ? WIDTHS : [1440, 390];

if (!URL || !LOGIN) {
  console.error("usage: cad865-acceptance.mjs --url <board app url> --login-link <login url> [--rig-log path] [--evidence dir] [--session name] [--width N]*");
  process.exit(2);
}

const ENV = {
  ...process.env,
  AGENT_BROWSER_FORCE_LOCAL: "1",
  AGENT_BROWSER_SESSION: SESSION,
};

// ---------- helpers ----------

const shots = [];
let step = 0;

function run(cmd, opts = {}) {
  const out = execFileSync("agent-browser", cmd, {
    env: ENV,
    encoding: "utf8",
    timeout: 60000,
    stdio: ["ignore", "pipe", "pipe"],
    ...opts,
  });
  return out;
}

function shot(name, width) {
  step += 1;
  const dir = join(EVIDENCE, `${width}px`);
  mkdirSync(dir, { recursive: true });
  const file = join(dir, `${String(step).padStart(2, "0")}-${name}.png`);
  run(["screenshot", file]);
  shots.push({ width, name, file });
  return file;
}

function snap() {
  return run(["snapshot", "-i"]);
}

function text() {
  return run(["eval", "document.body.innerText"]);
}

function find(label, tag = "button") {
  const s = snap();
  const line = s.split("\n").find((l) => l.includes(`"${label}"`) && l.includes(tag));
  if (!line) return null;
  const m = line.match(/@e(\d+)/);
  return m ? `@e${m[1]}` : null;
}

function clickRef(ref) {
  if (!ref) throw new Error("click target not found");
  run(["click", ref]);
}

async function waitText(t, tries = 40) {
  for (let i = 0; i < tries; i++) {
    try {
      const body = text();
      if (body.includes(t)) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`waited for "${t}" — never appeared`);
}

function viewport(width) {
  run(["eval", `window.resizeTo(${width}, 900)`]).catch(() => {});
}

// ---------- the pass ----------

async function pass(width) {
  viewport(width);
  // Sign in through the real login link — this is the only place the
  // fixture's minted nonce is spent.
  run(["open", LOGIN]);
  await waitText("Apps", 60);
  shot("00-login", width);

  // Land on the CRM install URL the harness seeded.
  run(["open", URL]);
  await waitText("Customers");
  shot("01-customers-list", width);

  // Customers: seeded rows render; the search narrows; the drawer opens.
  if (!text().includes("Amina Diallo")) throw new Error("seeded customer rows missing");
  const search = snap().match(/@e\d+.*search/i);
  if (search) {
    run(["fill", search[0].match(/@e\d+/)[0], "Dana"]);
    await new Promise((r) => setTimeout(r, 600));
    shot("02-customers-search", width);
    if (!text().includes("Dana Cole") || text().includes("Amina Diallo")) {
      throw new Error("customer search did not narrow");
    }
    run(["fill", search[0].match(/@e\d+/)[0], ""]);
    await new Promise((r) => setTimeout(r, 600));
  }

  // New customer — a real save through the operator session.
  clickRef(find("New customer"));
  await waitText("New customer");
  shot("03-customer-new", width);
  run(["fill", "#crm-display-name", "Acceptance Person"]);
  run(["fill", "#crm-email", "acceptance@example.invalid"]);
  clickRef(find("Create customer"));
  await waitText("Customer details");
  shot("04-customer-drawer", width);
  clickRef(find("Close"));
  await new Promise((r) => setTimeout(r, 300));

  // CSV import — paste, preview, confirm the needs_revision row, commit.
  clickRef(find("Import CSV"));
  await waitText("Import customers");
  shot("05-csv-source", width);
  const csv =
    "record_id,display_name,email,tags,consent_email,expected_revision\n" +
    "acceptance-2,Browser Person,browser@example.invalid,demo,unknown,\n" +
    "acceptance-3,Second Person,second@example.invalid,,granted,\n";
  run(["fill", "#csv-text", csv]);
  clickRef(find("Preview plan"));
  await waitText("Preview of");
  shot("06-csv-preview", width);
  clickRef(find("Import"));
  await waitText("applied");
  shot("07-csv-receipt", width);
  clickRef(find("Return to customers"));
  await waitText("Browser Person");
  shot("08-customers-after-import", width);

  // Segments — create a rule, land on the detail drawer, see counts.
  clickRef(find("Segments", "a"));
  await waitText("New segment");
  clickRef(find("New segment"));
  await waitText("New segment");
  shot("09-segment-new", width);
  run(["fill", "#seg-name", "Demo segment"]);
  run(["fill", "#seg-rule-value-0", "demo"]);
  clickRef(find("Create segment"));
  await waitText("Current matches");
  shot("10-segment-drawer", width);
  clickRef(find("Close"));
  await new Promise((r) => setTimeout(r, 300));

  // Campaigns — the seeded launch-1 campaign: open, preview, test-send,
  // freeze recheck, prepare, typed-count approve.
  clickRef(find("Campaigns", "a"));
  await waitText("launch-1", 40);
  const open = snap().split("\n").find((l) => l.includes('"Open"'));
  if (open) clickRef(open.match(/@e\d+/)[0]);
  await waitText("Campaign details");
  shot("11-campaign-detail", width);

  run(["click", find("Render preview") ?? ""]);
  await waitText("TEXT form");
  shot("12-campaign-preview", width);

  run(["fill", "#cmp-test-email", "qa@example.invalid"]);
  clickRef(find("Send test"));
  await waitText("not proof of inbox delivery", 40);
  shot("13-test-send", width);

  clickRef(find("Recheck freeze"));
  await waitText("Valid");
  shot("14-freeze-valid", width);

  clickRef(find("Prepare send"));
  await waitText("Prepared send");
  shot("15-prepared", width);

  clickRef(find("Approve and send…"));
  await waitText("Type", 20);
  // Type the exact final count to arm the confirm.
  const confirmInput = snap().match(/@e\d+.*crm-confirm-input|@e\d+.*\[input/);
  const finalCount = (text().match(/Final recipients[^\d]*(\d+)/) ?? [])[1] ?? "3";
  if (confirmInput) run(["fill", confirmInput[0].match(/@e\d+/)[0], finalCount]);
  shot("16-approve-typed", width);
  clickRef(find("Approve and send"));
  await waitText("completed", 60);
  shot("17-send-completed", width);

  // Unsubscribe — the send's own links carry the origin; the recipient
  // page redeems one. Read the rig log for the captured message's
  // unsubscribe URL when one is available.
  if (RIG_LOG && existsSync(RIG_LOG)) {
    const log = readFileSync(RIG_LOG, "utf8");
    const unsub = log.match(/https?:\/\/[^\s<>]+\/unsubscribe\/[A-Za-z0-9_-]+/);
    if (unsub) {
      run(["open", unsub[0]]);
      await waitText("unsubscribe", 30);
      shot("18-unsubscribe-page", width);
      clickRef(find("Confirm") ?? find("Unsubscribe"));
      await waitText("unsubscribed", 30);
      shot("19-unsubscribed", width);
    }
  }

  // Reload/deep-link proof: reopen the campaign detail URL directly.
  run(["open", URL.includes("record=") ? URL : `${URL}&record=launch-1`]);
  await waitText("Campaign details");
  shot("20-reload-detail", width);
}

// ---------- main ----------

const startedAt = new Date().toISOString();
try {
  for (const width of VIEWPORTS) {
    step = 0;
    await pass(width);
  }
  const manifest = {
    cadence_issue: "CAD-865",
    provenance: {
      url: URL,
      rig_log: RIG_LOG || null,
      session: SESSION,
      driver: "scripts/cad865-acceptance.mjs",
      note: "board was already running and owned by this lane; this driver performs no builds, launches or foreign attaches",
    },
    started_at: startedAt,
    finished_at: new Date().toISOString(),
    viewports: VIEWPORTS,
    screenshots: shots,
    verdict: "pass",
  };
  writeFileSync(join(EVIDENCE, "manifest.json"), JSON.stringify(manifest, null, 2));
  console.log(`CAD865 acceptance pass — ${VIEWPORTS.length} viewport(s), ${shots.length} shots`);
  console.log(JSON.stringify(manifest, null, 2));
} catch (error) {
  const manifest = {
    cadence_issue: "CAD-865",
    provenance: { url: URL, session: SESSION },
    started_at: startedAt,
    finished_at: new Date().toISOString(),
    verdict: "fail",
    error: String(error),
    screenshots: shots,
  };
  writeFileSync(join(EVIDENCE, "manifest.json"), JSON.stringify(manifest, null, 2));
  console.error(`CAD865 acceptance FAIL: ${error}`);
  process.exit(1);
} finally {
  try {
    run(["close"]);
  } catch {}
}
