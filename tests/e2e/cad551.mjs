// CAD-551: the board-chat UX evidence driver — the header session chips,
// the working row on a live turn, the steps disclosure, / commands and
// the new-messages pill, captured light and dark on a fake-pi master.
// One Chrome, one context (recorded — the run doubles as the turn GIF
// source), one page: the session cookie and the per-tab key live here.
//
// Run it against a scratch stack that is FULLY isolated — HOME, the
// XDG dirs, CADENCE_STATE_DIR AND CADENCE_PM_DIR all under one short
// /tmp root. `CADENCE_STATE_DIR` alone is not enough: without
// CADENCE_PM_DIR the daemon reads the real tracker (~/pm) and the
// screenshots leak real project/issue names — the production rule is
// never to touch ~/pm. Seed a tiny fake tracker instead:
//
//   ROOT=/tmp/cad551-e && WT=<this worktree>
//   mkdir -p $ROOT/{home,state,pm,repo,xdg/{config,data,state,cache},evidence}
//   git -C $ROOT/repo init -q -b main && git -C $ROOT/repo commit -qm init --allow-empty
//   ENV="env -i PATH=$PATH HOME=$ROOT/home XDG_CONFIG_HOME=$ROOT/xdg/config \
//        XDG_DATA_HOME=$ROOT/xdg/data XDG_STATE_HOME=$ROOT/xdg/state \
//        XDG_CACHE_HOME=$ROOT/xdg/cache CADENCE_STATE_DIR=$ROOT/state \
//        CADENCE_PM_DIR=$ROOT/pm"
//   # daemon (foreground, own shell) — fake-pi + an extra Landlock read
//   # grant for this directory so the confined master can exec it:
//   $ENV CADENCE_PI_COMMAND="python3 $WT/tests/e2e/fake-pi.py slow" \
//     CADENCE_MASTER_CONFINE_READ=$WT/tests/e2e $WT/target/debug/cadence daemon run
//   # every CLI that hits the daemon needs an operator-provable process —
//   # detach it (setsid -f) inside agent panes:
//   setsid -f $ENV $WT/target/debug/cadence issue init
//   setsid -f $ENV $WT/target/debug/cadence issue project add demo --prefix D --repo $ROOT/repo
//   setsid -f $ENV $WT/target/debug/cadence issue new "fake item" --project demo
//   setsid -f $ENV $WT/target/debug/cadence master start --provider pi
//   # the board too: it relays writes over its own daemon connection,
//   # so it must be detached as well:
//   setsid -f $ENV $WT/target/debug/cadence ui run --port <p> --dist $WT/ui/dist
//   setsid -f $ENV $WT/target/debug/cadence ui login --port <p> --json
//   # playwright resolves browsers/ffmpeg under HOME — symlink the real
//   # cache into $ROOT/home/.cache/ms-playwright.
//   setsid -f env -i PATH=<node-bin>:/usr/bin:/bin HOME=$ROOT/home \
//     E2E_URL=http://cadence-<p>.localhost:<p> E2E_LOGIN=<link> \
//     E2E_ARTIFACTS=$ROOT/evidence <node-bin>/node cad551.mjs
//
// `slow` fake-pi keeps the turn alive ~3 s so the working row is on
// screen when the page samples it. Screenshots land in E2E_ARTIFACTS;
// the page's video lands there too (chrome's webm — the GIF source).

import { chromium } from "playwright-core";
import fs from "node:fs";
import path from "node:path";

const base = process.env.E2E_URL;
const login = process.env.E2E_LOGIN;
const out = process.env.E2E_ARTIFACTS ?? ".";
if (!base || !login) {
  console.error("usage: E2E_URL=http://cadence-<p>.localhost:<p> E2E_LOGIN=<link> node cad551.mjs");
  process.exit(2);
}
const TIMEOUT = 30_000;
const shot = (page, name) => page.screenshot({ path: path.join(out, name), fullPage: false });

/** `text` must show up inside `locator` within the timeout. */
async function expectText(locator, text, what) {
  const deadline = Date.now() + TIMEOUT;
  let seen = "";
  while (Date.now() < deadline) {
    seen = (await locator.innerText().catch(() => "")) ?? "";
    if (seen.includes(text)) return;
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`${what}: "${text}" never appeared; last text:\n${seen.slice(0, 3000)}`);
}

/** `selector` (on the page) must appear — returns its locator. */
async function appear(page, selector, what) {
  const loc = page.locator(selector).first();
  try {
    await loc.waitFor({ state: "visible", timeout: TIMEOUT });
  } catch {
    throw new Error(`${what}: ${selector} never appeared`);
  }
  return loc;
}

/** Type into the composer and press Enter. */
async function send(page, text) {
  const box = page.getByLabel("message to the master");
  await box.fill(text);
  await box.press("Enter");
}

async function main() {
  fs.mkdirSync(out, { recursive: true });
  const browser = await chromium.launch({
    headless: true,
    channel: "chrome",
    args: ["--host-resolver-rules=MAP *.localhost 127.0.0.1"],
  });
  const context = await browser.newContext({
    viewport: { width: 1400, height: 1000 },
    colorScheme: "dark",
    recordVideo: { dir: out, size: { width: 1400, height: 1000 } },
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  try {
    // Sign this tab in (the link's exchange drops cookie + session key).
    await page.goto(login);
    await expectText(
      page.locator('section[aria-live="polite"]'),
      "Signed in as the operator.",
      "the login link signs this tab in",
    );

    // ---- Header: status + session chips ----
    await page.goto(base);
    const h1 = await appear(page, "section[aria-label='master thread'] h1", "the Master header");
    // Chips arrive with /api/master/state — Fake Model from the live
    // fake-pi session, the effort level, and context use.
    await appear(page, ".chip:has-text('Fake Model')", "the model chip");
    await appear(page, ".chip:has-text('effort medium')", "the effort chip");
    await appear(page, ".chip:has-text('% ctx')", "the context chip");
    // r2: the bootstrap briefing is a collapsed disclosure, never a
    // centred markdown wall — closed on load, opens to left-aligned GFM.
    const brief = await appear(page, '[data-kind="briefing"]', "the Session briefing disclosure");
    if (await brief.getAttribute("data-open")) {
      throw new Error("the briefing disclosure must start closed");
    }
    await h1.scrollIntoViewIfNeeded();
    await page.evaluate(() => window.scrollBy(0, -60));
    await page.waitForTimeout(400); // let chip-in entrances finish
    await shot(page, "01-header-chips.png");
    // Open it once to prove the body is GFM, not raw `**`/`#` text.
    await brief.locator(".steps-head").click();
    const briefBody = page.locator("[data-briefing-body]");
    await appear(page, '[data-kind="briefing"][data-open]', "the briefing opens");
    if (!(await briefBody.locator("strong, h1, h2, h3, p, ul, ol").first().count())) {
      throw new Error("the briefing body renders no markdown structure");
    }
    const centered = await briefBody.evaluate(
      (el) => getComputedStyle(el).textAlign === "center",
    );
    if (centered) throw new Error("the briefing body must not be centred");
    await brief.locator(".steps-head").click();
    // Back to the tail — the frame scroll unpins smart-scroll, and we want
    // a clean follow-state for the rest of the run.
    // "The tail" for framing: the thread section's bottom edge — the
    // rail beside it can run taller than the thread, and then
    // scrollHeight strands the shot below the conversation entirely.
    const tailY = () =>
      page.evaluate(() => {
        const s = document.querySelector("section[aria-label='master thread']");
        return s
          ? Math.max(0, s.getBoundingClientRect().bottom + window.scrollY - window.innerHeight)
          : document.documentElement.scrollHeight;
      });
    await page.evaluate(async (y) => window.scrollTo(0, y), await tailY());

    // ---- Slash autocomplete + command cards ----
    const box = page.getByLabel("message to the master");
    await box.fill("/");
    const menu = await appear(page, ".slashmenu", "the slash menu");
    await page.waitForTimeout(400); // menu-in
    await expectText(menu, "Interrupt the running turn", "the menu lists commands");
    await shot(page, "02-slash-menu.png");
    // Escape closes it; a verb + Enter runs it and lands a card.
    await box.press("Escape");
    await send(page, "/state");
    const stateCard = await appear(page, ".cmdcard[data-cmd='state']", "the /state card");
    // r2: a JSON answer is a field list — model/effort/session/context —
    // never raw pretty-JSON; the wire names live behind the raw toggle.
    await appear(stateCard, ".kvrow", "the /state card lists fields");
    await expectText(stateCard, "effort", "the /state card labels the effort row");
    await expectText(stateCard, "session", "the /state card labels the session row");
    const rawToggle = stateCard.getByRole("button", { name: "raw" });
    await rawToggle.waitFor({ state: "visible", timeout: 5000 });
    await rawToggle.click();
    await expectText(stateCard, "sessionId", "the raw toggle reveals the wire keys");
    await stateCard.getByRole("button", { name: "fields" }).click();
    await send(page, "/help");
    const helpCard = await appear(page, ".cmdcard[data-cmd='help']", "the /help card");
    await expectText(helpCard, "Commands", "/help renders the catalog");
    // r2: the command column keeps real padding — `/model provider/id`
    // must not run into its description.
    const modelCell = helpCard.locator("td", { hasText: "/model provider/id" }).first();
    await modelCell.waitFor({ state: "visible", timeout: 5000 });
    // Cell borders touch by definition — measure the gap between the two
    // cells' *text* edges (what the eye sees, what r1 got wrong).
    const gap = await modelCell.evaluate((el) => {
      const edge = (node, last) => {
        const r = document.createRange();
        r.selectNodeContents(node);
        const rects = r.getClientRects();
        if (!rects.length) return null;
        return last ? rects[rects.length - 1].right : rects[0].left;
      };
      const next = el.nextElementSibling;
      if (!next) return -1;
      const right = edge(el, true);
      const left = edge(next, false);
      return right == null || left == null ? -1 : left - right;
    });
    if (gap < 8) throw new Error(`/help table columns collide (text gap ${gap}px)`);
    // An unknown verb is a local error card — never a chat message.
    await send(page, "/bogus");
    const bad = await appear(page, ".cmdcard[data-cmd='bogus']", "the unknown-command card");
    await expectText(bad, "Unknown command", "/bogus is refused locally");
    // A model switch goes to the daemon and lands its durable line.
    await send(page, "/model fake/model-2");
    await expectText(
      page.locator("section[aria-label='master thread']"),
      "/model fake/model-2",
      "the /model command card",
    );
    // r2: /stats lists context + messages as fields too.
    await send(page, "/stats");
    const statsCard = await appear(page, ".cmdcard[data-cmd='stats']", "the /stats card");
    await expectText(statsCard, "context", "the /stats card folds context usage");
    await shot(page, "03-command-cards.png");

    // ---- A working turn: the row, the step, then the reply ----
    await send(page, "run tool cadence status, then report");
    // The operator's own message enters the thread.
    const thread = page.locator('ol[aria-label="messages"]');
    await expectText(thread, "run tool cadence status", "the operator's message lands");
    // The working row: pulse + elapsed + the live step while the turn runs.
    const row = await appear(page, '.workrow[data-turn="working"]', "the working row");
    await expectText(row, "Working", "the working row says Working");
    const stepText = await row.locator(".stepfade").innerText().catch(() => "");
    if (!/cadence status|bash/i.test(stepText)) {
      throw new Error(`the working row should name the live step, shows "${stepText}"`);
    }
    await appear(page, ".workrow .stopbtn", "the Stop button");
    // The row sits above the composer — frame it at the tail. Reaching
    // it also clears the pill (tail seen = all seen); wait out the frame.
    await page.evaluate(async (y) => window.scrollTo(0, y), await tailY());
    await page.locator(".newpill").waitFor({ state: "detached", timeout: 5000 });
    await shot(page, "04-working.png");
    // The reply handoff: the row leaves, the answer bubble lands.
    await expectText(thread, "fake-pi reply", "the master's reply lands");
    try {
      await page.locator('.workrow[data-turn="working"]').waitFor({ state: "detached", timeout: 15_000 });
    } catch {
      throw new Error("the working row must clear when the reply lands");
    }

    // ---- The steps disclosure ----
    const steps = page.locator(".steps").last();
    await expectText(steps, "tool step", "the group counts its steps");
    await steps.locator(".steps-head").click();
    await page.locator(".steps[data-open] .step-row:has-text('cadence status')").waitFor({
      state: "visible",
      timeout: 5000,
    });
    await page.waitForTimeout(350); // steps-body height transition
    await steps.scrollIntoViewIfNeeded();
    await shot(page, "05-steps.png");

    // ---- Smart scroll: the pill appears only off-bottom ----
    // fill() scrolls the composer into view (re-pinning the tail), so
    // the scroll-away must come after the fill and before Enter.
    const box2 = page.getByLabel("message to the master");
    await box2.fill("one more while I read history");
    await page.evaluate(() => window.scrollTo(0, 0));
    await box2.press("Enter");
    const pill = await appear(page, ".newpill", "the new-messages pill while scrolled up");
    await page.waitForTimeout(400); // pill-in
    await expectText(pill, "new", "the pill counts arrivals");
    await shot(page, "06-new-pill.png");
    await pill.click();
    // The pill jumps to the tail — the section's bottom edge in view.
    await page.waitForFunction(
      () => {
        const s = document.querySelector("section[aria-label='master thread']");
        if (!s) return false;
        const b = s.getBoundingClientRect().bottom;
        return b > 0 && b <= window.innerHeight + 20;
      },
      { timeout: TIMEOUT },
    );

    // ---- Reduced motion: no animation names on live classes ----
    await page.emulateMedia({ reducedMotion: "reduce" });
    await send(page, "reduced-motion check, run tool too");
    await page.waitForTimeout(300);
    const anim = await page.evaluate(() => {
      const el = document.querySelector(".msg-in") ?? document.querySelector(".workrow");
      return el ? getComputedStyle(el).animationName : "no-target";
    });
    if (anim !== "none" && anim !== "no-target") {
      throw new Error(`prefers-reduced-motion still animates: ${anim}`);
    }
    await page.emulateMedia({ reducedMotion: "no-preference" });

    // ---- Light theme capture ----
    await page.evaluate(() => localStorage.setItem("cadence-theme", "light"));
    await page.emulateMedia({ colorScheme: "light" });
    await page.reload();
    await expectText(thread, "fake-pi reply", "the thread survives the theme swap");
    await appear(page, ".steps", "the steps group in light");
    await page.evaluate(async (y) => window.scrollTo(0, y), await tailY());
    await shot(page, "07-light.png");
    await page.evaluate(() => window.scrollTo(0, 0));
    await shot(page, "08-light-top.png");

    console.log("cad551: all evidence steps passed");
  } finally {
    const v = page.video();
    await context.close();
    await browser.close();
    if (v) {
      const webm = await v.path().catch(() => null);
      if (webm && fs.existsSync(webm)) {
        fs.renameSync(webm, path.join(out, "cad551-turn.webm"));
      }
    }
    if (errors.length) {
      console.error("page errors:", errors.join("\n"));
      process.exitCode = 1;
    }
  }
}

main().catch((e) => {
  console.error(e.message ?? e);
  process.exit(1);
});
