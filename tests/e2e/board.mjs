// CAD-433: the headless board steps of the MVP journey. One step per
// run — tests/e2e_mvp.rs calls it between the steps it drives itself:
//
//   node board.mjs <step> '<json args>'
//
// Env: E2E_URL (the sandbox board, http://127.0.0.1:<3110-3199>),
// E2E_ARTIFACTS (screenshots land here: <step>.png, and <step>-fail.png
// plus <step>-fail.html on failure), E2E_CHROME (optional browser
// binary; default: the installed Google Chrome channel — no browser
// download at run time).
//
// Every request the page makes to anything but the board is aborted and
// fails the step: the journey is offline by construction, and this
// proves the UI's part of it. Prints one JSON line `{step, ok, ...}` and
// exits 0, or prints the error and exits 1.

import { chromium } from "playwright-core";
import fs from "node:fs";
import path from "node:path";

const [step, rawArgs] = process.argv.slice(2);
const args = rawArgs ? JSON.parse(rawArgs) : {};
const base = process.env.E2E_URL;
const out = process.env.E2E_ARTIFACTS ?? ".";
if (!step || !base) {
  console.error("usage: E2E_URL=http://127.0.0.1:<port> node board.mjs <step> '<json>'");
  process.exit(2);
}
const TIMEOUT = 60_000;
const LAST_SEEN_KEY = "cadence-home-last-seen"; // ui/src/features/home/sinceLeft.ts

/** `text` must show up inside `locator` (its full text), within the timeout. */
async function expectText(locator, text, what) {
  const deadline = Date.now() + TIMEOUT;
  let seen = "";
  while (Date.now() < deadline) {
    seen = (await locator.innerText().catch(() => "")) ?? "";
    if (seen.includes(text)) return;
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error(`${what}: "${text}" never appeared; last text:\n${seen.slice(0, 4000)}`);
}

/** Open the Needs-you row of `kind` whose title mentions `issue`. */
async function needRow(page, kind, issue) {
  const row = page.locator(`section[aria-label="needs you"] li[data-need="${kind}"]`, {
    hasText: issue,
  });
  await row.first().waitFor({ timeout: TIMEOUT });
  return row.first();
}

const steps = {
  // Use case 2: the setup page lists the checks, with the detected CLI.
  async setup(page) {
    await page.goto(`${base}/setup`);
    const main = page.locator("main");
    await expectText(main, "Setup", "setup page");
    for (const step of ["Environment", "Agent CLIs", "Master agent", "First project"]) {
      await expectText(main, step, "setup page lists its steps");
    }
    await main.getByText("Agent CLIs", { exact: true }).first().click();
    await expectText(main, "signed in", "setup page shows the detected claude CLI signed in");
    await expectText(main, "claude", "setup page lists the detected claude CLI");
    // CAD-448: the master step offers the signed-in provider's exact
    // start command and the master's own login command (CAD-439).
    await main.getByText("Master agent", { exact: true }).first().click();
    await expectText(
      main,
      "master start --provider claude",
      "setup offers claude's exact master start command",
    );
    await expectText(main, "claude auth login", "setup shows the master's own login command");
    // The step must not overflow a phone-width viewport.
    await page.setViewportSize({ width: 390, height: 800 });
    const overflow = await page.evaluate(
      () =>
        document.documentElement.scrollWidth > document.documentElement.clientWidth ||
        document.body.scrollWidth > document.documentElement.clientWidth,
    );
    if (overflow) throw new Error("the master step overflows a 390px viewport");
    return {};
  },

  // Use case 3: the operator asks for work in the master's thread.
  async chat(page, { text, expect }) {
    await page.goto(base);
    const box = page.getByLabel("message to the master");
    await box.waitFor({ timeout: TIMEOUT });
    await box.fill(text);
    await page.locator("form[data-composer] button[type=submit]").click();
    const thread = page.locator('ol[aria-label="messages"]');
    await expectText(thread, text, "the operator's message in the thread");
    await expectText(thread, expect, "the master's reply in the thread");
    return {};
  },

  // Use cases 3-4: the plan card sits in the thread under the master's
  // reply; Approve decides it.
  async approve(page, { epic, tickets }) {
    await page.goto(base);
    const card = page.locator(`ol[aria-label="messages"] section[data-plan-card="${epic}"]`);
    await card.waitFor({ timeout: TIMEOUT });
    for (const t of tickets) await expectText(card, t, "the plan card lists its tickets");
    await expectText(card, "acceptance check", "the plan card shows acceptance");
    await expectText(card, "awaiting you", "a proposed plan awaits the operator");
    await card.getByRole("button", { name: "Approve plan" }).click();
    await expectText(card, "approved", "the plan card after Approve");
    const bar = card.getByRole("progressbar");
    await bar.waitFor({ timeout: TIMEOUT });
    return { progress: await bar.getAttribute("aria-valuenow") };
  },

  // Use case 5: the project board and the agents screen show the work.
  async watch(page, { project, issue, agent }) {
    await page.goto(`${base}/projects/${project}`);
    await expectText(page.locator("main"), issue, "the project board shows the ticket");
    await page.goto(`${base}/agents`);
    await expectText(page.locator("main"), agent, "the agents screen shows the worker");
    return {};
  },

  // Use case 6: the escalated question is a card with options.
  async answer(page, { issue, option, summary }) {
    await page.goto(base);
    const row = await needRow(page, "question", issue);
    await row.getByRole("button", { name: /^Answer/ }).click();
    if (summary) await expectText(row, summary, "the master's summary on the question");
    await row.getByRole("button", { name: option, exact: true }).click();
    await expectText(row, `answered: ${option}`, "the question after answering");
    return {};
  },

  // Use case 7: the merge decision names the reviewer and the pinned
  // head; Merge enqueues it.
  async merge(page, { issue, reviewer, sha, pr, verdict }) {
    await page.goto(base);
    const row = await needRow(page, "merge_decision", issue);
    await expectText(row, `PASS by ${reviewer}`, "the merge row names the reviewer");
    await row.getByRole("button", { name: "Review merge" }).click();
    await expectText(row, pr, "the merge row names the PR");
    await expectText(row, verdict, "the merge row carries the verdict");
    await expectText(row, sha.slice(0, 12), "the merge is pinned to the reviewed head");
    await row.getByRole("button", { name: "Merge", exact: true }).click();
    await expectText(row, "merge enqueued", "the merge row after Merge");
    return {};
  },

  // Use case 8: the operator closed the browser and comes back two
  // hours later — a fresh browser whose storage says Home was last seen
  // then (set before the app's scripts run: a live Home rewrites it on
  // every visit and on pagehide). The since-you-left card and the thread
  // are both there.
  async since(page, { expect, counts, thread }) {
    const away = Math.floor(Date.now() / 1000) - 2 * 3600;
    await page.addInitScript(
      ([k, v]) => {
        if (!sessionStorage.getItem("e2e-away-set")) {
          localStorage.setItem(k, String(v));
          sessionStorage.setItem("e2e-away-set", "1");
        }
      },
      [LAST_SEEN_KEY, away],
    );
    await page.goto(base);
    const card = page.locator("section[data-since-card]");
    await card.waitFor({ timeout: TIMEOUT });
    for (const t of expect) await expectText(card, t, "since you left");
    // Section sizes, not which rows made the three-row cut: rows sharing
    // a second (report `at` is second-precision) sort in any order.
    for (const [label, n] of Object.entries(counts ?? {})) {
      const dt = card.locator("dt", { hasText: new RegExp(`^\\s*${label}\\b`, "i") });
      await dt.first().waitFor({ timeout: TIMEOUT });
      const got = (await dt.first().innerText()).replace(/\s+/g, " ").trim();
      if (!new RegExp(`^${label} ${n}$`, "i").test(got)) {
        throw new Error(`since you left: section ${label} should count ${n}, shows "${got}"`);
      }
    }
    const messages = page.locator('ol[aria-label="messages"]');
    for (const t of thread) await expectText(messages, t, "the thread survives the return");
    return {};
  },
};

async function main() {
  const run = steps[step];
  if (!run) throw new Error(`unknown step ${step}`);
  fs.mkdirSync(out, { recursive: true });
  const launch = { headless: true };
  if (process.env.E2E_CHROME) launch.executablePath = process.env.E2E_CHROME;
  else launch.channel = "chrome";
  const browser = await chromium.launch(launch);
  const origin = new URL(base).origin;
  const offsite = [];
  const context = await browser.newContext({ viewport: { width: 1400, height: 1000 } });
  await context.route("**/*", (route) => {
    const url = route.request().url();
    let same = false;
    try {
      same = new URL(url).origin === origin;
    } catch {
      same = false;
    }
    if (same || url.startsWith("data:") || url.startsWith("blob:")) {
      return route.continue();
    }
    offsite.push(url);
    return route.abort();
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  try {
    const result = await run(page, args);
    await page.screenshot({ path: path.join(out, `${step}.png`), fullPage: true });
    if (offsite.length) throw new Error(`the board reached off the host: ${offsite.join(", ")}`);
    if (errors.length) throw new Error(`page errors: ${errors.join(" | ")}`);
    console.log(JSON.stringify({ step, ok: true, ...result }));
  } catch (e) {
    await page.screenshot({ path: path.join(out, `${step}-fail.png`), fullPage: true }).catch(() => {});
    fs.writeFileSync(path.join(out, `${step}-fail.html`), await page.content().catch(() => ""));
    throw e;
  } finally {
    await browser.close();
  }
}

main().catch((e) => {
  console.error(`board step ${step} failed: ${e?.stack ?? e}`);
  process.exit(1);
});
