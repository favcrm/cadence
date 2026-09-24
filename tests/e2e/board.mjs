// CAD-433/CAD-485: the headless board steps of the MVP journey — the
// operator's browser. One Chrome, one context, one page for the whole
// run: the operator's session is a cookie plus a per-tab key in
// sessionStorage (CAD-313), so the page that signed in must be the page
// every write rides on. tests/e2e_mvp.rs keeps this process alive and
// pipes one JSON request per stdin line:
//
//   {"step": "chat", "args": {"text": "...", "expect": "..."}}
//
// and reads one JSON line back per step: `{"step": ..., "ok": true,
// ...}` or `{"step": ..., "ok": false, "error": ...}` after which the
// process exits. A step's screenshot is <NN>-<step>.png under
// E2E_ARTIFACTS; on failure <NN>-<step>-fail.png and -fail.html too.
//
// Env: E2E_URL (the board's own name, http://cadence-<port>.localhost:
// <port> — the session cookie lives on that host, never on 127.0.0.1),
// E2E_ARTIFACTS, E2E_CHROME (optional browser binary; default: the
// installed Google Chrome channel — no browser download at run time).
//
// Every request the page makes to anything but the board is aborted and
// fails the step: the journey is offline by construction, and this
// proves the UI's part of it.

import { chromium } from "playwright-core";
import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";

const base = process.env.E2E_URL;
const out = process.env.E2E_ARTIFACTS ?? ".";
if (!base) {
  console.error("usage: E2E_URL=http://cadence-<port>.localhost:<port> node board.mjs");
  process.exit(2);
}
const TIMEOUT = 60_000;
const LAST_SEEN_KEY = "cadence-home-last-seen"; // ui/src/features/home/sinceLeft.ts
const SESSION_KEY = "cadence.sessionKey"; // ui/src/lib/sessionKey.ts

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
  async setup({ page }) {
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
    await page.setViewportSize({ width: 1400, height: 1000 });
    return {};
  },

  // CAD-313: the link `cadence ui login` prints, opened in this tab.
  // The exchange sets the cookie (board host, HttpOnly) and returns the
  // per-tab key, which lands in sessionStorage — every later write
  // rides on this page. After it the header offers sign out.
  async login({ page }, { link }) {
    await page.goto(link);
    await expectText(
      page.locator('section[aria-live="polite"]'),
      "Signed in as the operator.",
      "the login link signs this tab in",
    );
    if (page.url().includes("#n=")) {
      throw new Error(`the login nonce was not stripped from the URL: ${page.url()}`);
    }
    const session = await page.evaluate(async (keyName) => {
      const key = sessionStorage.getItem(keyName);
      const r = await fetch("/api/meta", {
        headers: { "X-Cadence-Session": key ?? "" },
      });
      return { hasKey: !!key, status: r.status, meta: await r.json().catch(() => null) };
    }, SESSION_KEY);
    if (!session.hasKey) {
      throw new Error(`no ${SESSION_KEY} in this tab's sessionStorage`);
    }
    if (session.meta?.signed_in !== true) {
      throw new Error(`/api/meta does not honour the session: ${JSON.stringify(session)}`);
    }
    await page.goto(base);
    await page
      .getByRole("button", { name: /sign out/ })
      .waitFor({ timeout: TIMEOUT })
      .catch(() => {
        throw new Error("the board does not show this tab signed in (no sign-out chip)");
      });
    return {};
  },

  // The same link, a second time — in another tab this round. It was
  // spent by the first: the board refuses it `already_used`, and that
  // tab holds no session even though the shared context gave it the
  // cookie. A link signs one tab in, once.
  async "login-replay"({ context }, { link }) {
    const p2 = await context.newPage();
    p2.on("pageerror", (e) => errors.push(String(e)));
    try {
      await p2.goto(link);
      await expectText(
        p2.locator('section[aria-live="polite"]'),
        "already used",
        "a spent link is refused",
      );
      const meta = await p2.evaluate(() =>
        fetch("/api/meta").then((r) => r.json()),
      );
      if (meta?.signed_in !== false || meta?.tab_signed_out !== true) {
        throw new Error(`the replayed tab holds a session: ${JSON.stringify(meta)}`);
      }
      await p2.screenshot({ path: path.join(out, "login-replay-tab.png"), fullPage: true });
    } finally {
      await p2.close();
    }
    return {};
  },

  // A tab that carries the session cookie but no key — it never ran
  // the login exchange — writes nothing: the UI says so, and the API
  // refuses the write before its handler runs (403
  // operator_session_required).
  async "unsigned-write"({ context }) {
    const p2 = await context.newPage();
    p2.on("pageerror", (e) => errors.push(String(e)));
    try {
      await p2.goto(base);
      await expectText(
        p2.locator("body"),
        "Sign in this tab with",
        "a tab without the session key is told to sign in",
      );
      const res = await p2.evaluate(async () => {
        const r = await fetch("/api/threads/master/messages", {
          method: "POST",
          headers: { "Content-Type": "application/json", "X-Cadence-Board": "1" },
          body: JSON.stringify({ text: "a tab without the session key" }),
        });
        return { status: r.status, body: await r.json().catch(() => null) };
      });
      if (res.status !== 403 || res.body?.check !== "operator_session_required") {
        throw new Error(
          `a cookie-only tab's write must be refused 403 operator_session_required: ${JSON.stringify(res)}`,
        );
      }
      await p2.screenshot({ path: path.join(out, "unsigned-write-tab.png"), fullPage: true });
    } finally {
      await p2.close();
    }
    return {};
  },

  // Use case 3: the operator asks for work in the master's thread.
  async chat({ page }, { text, expect }) {
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
  async approve({ page }, { epic, tickets }) {
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
  async watch({ page }, { project, issue, agent }) {
    await page.goto(`${base}/projects/${project}`);
    await expectText(page.locator("main"), issue, "the project board shows the ticket");
    await page.goto(`${base}/agents`);
    await expectText(page.locator("main"), agent, "the agents screen shows the worker");
    return {};
  },

  // CAD-432: the epic on the Projects screen — its stage chip,
  // size-weighted progress and health, then its children and the stage
  // history once opened. `children` maps ticket id to the status text
  // the child row shows ("blocked" for a waiting one).
  async epics({ page }, { project, epic, stage, label, percent, health, children }) {
    await page.goto(`${base}/projects/${project}/epics`);
    const row = page.locator(`main[aria-label="epics"] li[data-epic="${epic}"]`);
    await row.waitFor({ timeout: TIMEOUT });
    await expectText(row, stage, "the epic's stage chip");
    await expectText(row, health, "the epic's health");
    await expectText(row, label, "the epic's weighted progress");
    const bar = row.getByRole("progressbar");
    await bar.waitFor({ timeout: TIMEOUT });
    const now = await bar.getAttribute("aria-valuenow");
    if (now !== String(percent)) {
      throw new Error(`${epic} progress should be ${percent}%, reads ${now}%`);
    }
    if (children) {
      await row.getByRole("button", { name: `open ${epic}` }).click();
      const kids = row.locator(`section[aria-label="${epic} children"] ul li`);
      for (const [id, status] of Object.entries(children)) {
        const kid = kids.filter({ hasText: id }).first();
        await kid.waitFor({ timeout: TIMEOUT });
        await expectText(kid, status, `${id} in the epic's children`);
      }
      // innerText is the rendered text: `slabel` styles it uppercase.
      await expectText(row, "STAGE HISTORY", "the epic's stage history");
    }
    return {};
  },

  // Use case 6: the escalated question is a card with options.
  async answer({ page }, { issue, option, summary }) {
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
  async merge({ page }, { issue, reviewer, sha, pr, verdict }) {
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
  // hours later — storage that says Home was last seen then (set before
  // the app's scripts run: a live Home rewrites it on every visit and
  // on pagehide). The since-you-left card and the thread are both there.
  async since({ page }, { expect, counts, thread }) {
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

const errors = [];

async function main() {
  fs.mkdirSync(out, { recursive: true });
  const launch = {
    headless: true,
    // `*.localhost` resolves to loopback in Chrome already; the rule
    // keeps it true where a resolver would not (CI, a proxy env).
    args: ["--host-resolver-rules=MAP *.localhost 127.0.0.1"],
  };
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
  page.on("pageerror", (e) => errors.push(String(e)));

  const write = (v) => process.stdout.write(JSON.stringify(v) + "\n");
  const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  let seq = 0;
  for await (const line of rl) {
    if (!line.trim()) continue;
    let req;
    try {
      req = JSON.parse(line);
    } catch (e) {
      write({ ok: false, error: `bad request line: ${e}` });
      continue;
    }
    const run = steps[req.step];
    seq += 1;
    const errStart = errors.length;
    const offStart = offsite.length;
    try {
      if (!run) throw new Error(`unknown step ${req.step}`);
      const result = await run({ page, context }, req.args ?? {});
      await page.screenshot({ path: path.join(out, `${seq}-${req.step}.png`), fullPage: true });
      const newOff = offsite.slice(offStart);
      if (newOff.length) throw new Error(`the board reached off the host: ${newOff.join(", ")}`);
      const newErrs = errors.slice(errStart);
      if (newErrs.length) throw new Error(`page errors: ${newErrs.join(" | ")}`);
      write({ step: req.step, ok: true, ...result });
    } catch (e) {
      await page
        .screenshot({ path: path.join(out, `${seq}-${req.step}-fail.png`), fullPage: true })
        .catch(() => {});
      fs.writeFileSync(
        path.join(out, `${seq}-${req.step}-fail.html`),
        await page.content().catch(() => ""),
      );
      write({ step: req.step, ok: false, error: String(e?.stack ?? e) });
      await browser.close().catch(() => {});
      process.exit(1);
    }
  }
  await browser.close();
}

main().catch((e) => {
  console.error(`board driver failed: ${e?.stack ?? e}`);
  process.exit(1);
});
