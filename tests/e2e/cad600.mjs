// CAD-600: the Home conversation as a full-height panel with a floating
// composer dock — the before/after evidence driver. One Chrome, one
// context; every request carries the fixture seam assertion headers
// (`X-Cadence-Test-As`/`X-Cadence-Test-Token`, CAD-482), so writes are
// the operator's on a seam-armed board without leaving the pane.
//
// Run it against a scratch stack that is FULLY isolated — HOME, the XDG
// dirs, CADENCE_STATE_DIR and CADENCE_PM_DIR under one short /tmp root
// — on a `--features test-seam` build. From an agent pane the seam is
// the sanctioned path; nothing here detaches or clears the environment:
//
//   ROOT=/tmp/cad600-e; WT=<this worktree>; BIN=<test-seam cadence>
//   mkdir -p $ROOT/{home,state,pm,repo,xdg/{config,data,state,cache},evidence}
//   git -C $ROOT/repo init -q -b main && \
//     git -C $ROOT/repo -c user.name=t -c user.email=t@t commit -qm init --allow-empty
//   ENV="env -u CADENCE_ALIAS HOME=$ROOT/home XDG_CONFIG_HOME=$ROOT/xdg/config \
//        XDG_DATA_HOME=$ROOT/xdg/data XDG_STATE_HOME=$ROOT/xdg/state \
//        XDG_CACHE_HOME=$ROOT/xdg/cache CADENCE_STATE_DIR=$ROOT/state \
//        CADENCE_PM_DIR=$ROOT/pm"
//   $ENV CADENCE_TEST_SEAM=1 CADENCE_PI_COMMAND="python3 $WT/tests/e2e/fake-pi.py slow" \
//     CADENCE_MASTER_CONFINE_READ=$WT/tests/e2e $BIN daemon run &        # own shell
//   $ENV CADENCE_TEST_AS=operator $BIN issue init
//   $ENV CADENCE_TEST_AS=operator $BIN issue project add demo --prefix D --repo $ROOT/repo
//   $ENV CADENCE_TEST_SEAM=1 CADENCE_TEST_AS=operator $BIN ui run --port 3120 --dist $WT/ui/dist &
//   $ENV CADENCE_TEST_AS=operator $BIN ui login --port 3120 --json    # the link
//   # not-started shots first (no master), then:
//   $ENV CADENCE_TEST_AS=operator $BIN master start --provider pi
//   # one run per viewport, each with its own fresh login link:
//   run() { E2E_URL=http://cadence-3120.localhost:3120 E2E_LOGIN=$1 \
//     E2E_SEAM_TOKEN=$(cat $ROOT/state/seam/token) E2E_ARTIFACTS=$ROOT/evidence \
//     E2E_TAG=$2 E2E_VIEWPORT=$3 E2E_PHASE=$4 E2E_ASSERT=$5 node cad600.mjs; }
//
// `E2E_TAG` names the run (before/after) in every file; `E2E_VIEWPORT`
// picks the size (desktop 1440×900, phone 390×844); `E2E_ASSERT=1` turns
// on the layout checks (after only — the before layout is the short box
// this ticket removes). On a phone those checks also require the last
// message and the jump pill to stay clear of Needs-you, and the home
// shell to be `100dvh` with overflow hidden. A shorter dynamic viewport
// (100vh taller than 100dvh) cannot be emulated: headless Chrome reports
// one used height for vh and dvh, and the device-metrics override scales
// them together. Screenshots land in E2E_ARTIFACTS.

import { chromium } from "playwright-core";
import fs from "node:fs";
import path from "node:path";

const base = process.env.E2E_URL;
const login = process.env.E2E_LOGIN;
const out = process.env.E2E_ARTIFACTS ?? ".";
const tag = process.env.E2E_TAG ?? "after";
const phase = process.env.E2E_PHASE ?? "live";
const token = process.env.E2E_SEAM_TOKEN ?? "";
const asserts = process.env.E2E_ASSERT === "1";
if (!base || !login) {
  console.error("usage: E2E_URL=http://cadence-<p>.localhost:<p> E2E_LOGIN=<link> node cad600.mjs");
  process.exit(2);
}
const TIMEOUT = 30_000;
const DESKTOP = { width: 1440, height: 900 };
const PHONE = { width: 390, height: 844 };
const viewport = process.env.E2E_VIEWPORT === "phone" ? PHONE : DESKTOP;
const size = process.env.E2E_VIEWPORT === "phone" ? "phone" : "desktop";
// The board's write guard wants its own marker; the seam headers say
// which caller the fixture should resolve (CAD-482).
const seam = {
  "X-Cadence-Test-As": "operator",
  "X-Cadence-Test-Token": token,
  "X-Cadence-Board": "1",
};
const shot = (page, name) => page.screenshot({ path: path.join(out, `${tag}-${name}.png`), fullPage: false });

async function expectText(locator, text, what) {
  const deadline = Date.now() + TIMEOUT;
  let seen = "";
  while (Date.now() < deadline) {
    seen = (await locator.innerText().catch(() => "")) ?? "";
    if (seen.includes(text)) return;
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`${what}: "${text}" never appeared; last text:\n${seen.slice(0, 2000)}`);
}

async function appear(page, selector, what) {
  const loc = page.locator(selector).first();
  try {
    await loc.waitFor({ state: "visible", timeout: TIMEOUT });
  } catch {
    throw new Error(`${what}: ${selector} never appeared`);
  }
  return loc;
}

function ok(cond, what) {
  if (!cond) throw new Error(`assertion failed: ${what}`);
}

/** Positive-area overlap. Shared edges do not count. */
function overlaps(a, b) {
  if (!a || !b) return false;
  return a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top;
}

/** The thread scroller (CAD-600) or the page (the before layout) at its
 *  tail — re-sent while the content is still growing, so a reply landing
 *  mid-scroll cannot leave the shot above the bottom. */
const toTail = (page) =>
  page.evaluate(async () => {
    const scroller = document.querySelector("[data-chat-scroll]");
    if (!scroller) {
      window.scrollTo(0, document.documentElement.scrollHeight);
      return;
    }
    const at = () => scroller.scrollTop >= scroller.scrollHeight - scroller.clientHeight - 2;
    for (let i = 0; i < 20 && !at(); i++) {
      scroller.scrollTop = scroller.scrollHeight;
      await new Promise((r) => requestAnimationFrame(() => setTimeout(r, 50)));
    }
  });

/** Wait (bounded) for the working row to clear — the fake master's
 *  queued turns land replies while the run goes on. */
async function settle(page, ms = 20_000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if ((await page.locator('.workrow[data-turn="working"]').count()) === 0) return;
    await page.waitForTimeout(500);
  }
}

const toTop = (page) =>
  page.evaluate(() => {
    const scroller = document.querySelector("[data-chat-scroll]");
    if (scroller) scroller.scrollTop = 0;
    window.scrollTo(0, 0);
  });

/** Queue one operator message through the board's own route — the
 *  session key is this tab's second credential (CAD-313), and a
 *  session-bearing write must declare its own Origin. */
async function send(page, text, n) {
  const key = await page.evaluate(() => sessionStorage.getItem("cadence.sessionKey"));
  const resp = await page.request.post(`${base}/api/threads/master/messages`, {
    headers: {
      ...seam,
      Origin: base,
      ...(key ? { "X-Cadence-Session": key } : {}),
    },
    data: { text, message: `e2e-cad600-${tag}-${n}` },
  });
  if (!resp.ok()) throw new Error(`thread_send refused (${resp.status()}): ${await resp.text()}`);
}

/** Log in this tab, then land on Home. */
async function openHome(context) {
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error("page error:", String(e)));
  await page.goto(login);
  await expectText(
    page.locator('section[aria-live="polite"]'),
    "Signed in as the operator.",
    "the login link signs this tab in",
  );
  await page.goto(base);
  return page;
}

const LONG_THREAD = [
  "Summarise where CAD-600 stands and what is left.",
  "Which lanes are running right now?",
  "Draft the acceptance for the dock's keyboard behaviour.",
  "What changed on the tracker since yesterday?",
  "List the open questions I owe an answer to.",
  "Show me the plan for the next release.",
  "What did the last review flag?",
  "How long has the master been running today?",
  "Any blocked tickets I should look at?",
  "Write a short note for the handover.",
  "What is the oldest doing ticket?",
  "Summarise the last three merges.",
  "Which tests cover the board's home screen?",
  "Give me the state of the UI build.",
  "Which lane owns the icons pass?",
  "List the tickets waiting on a review.",
  "What is in the outbox right now?",
  "How many agents are idle?",
  "Which plans are waiting for approval?",
  "Anything I should look at before I sign off?",
];

/** The full-height layout checks (after only). */
async function checkLayout(page) {
  const geom = await page.evaluate(() => {
    const panel = document.querySelector("[data-chat-panel]");
    const dock = document.querySelector("[data-chat-dock]");
    const scroller = document.querySelector("[data-chat-scroll]");
    const last = document.querySelector('ol[aria-label="messages"] > li:last-child');
    const need = document.querySelector(".needbtn");
    const needShown = !!(need && getComputedStyle(need).display !== "none" && need.getClientRects().length);
    const shell = document.querySelector("[data-app-shell]");
    const unit = (name) => {
      const el = document.createElement("div");
      el.style.cssText = `position:fixed;left:0;top:0;height:${name};width:0;pointer-events:none;visibility:hidden`;
      document.body.appendChild(el);
      const h = el.getBoundingClientRect().height;
      el.remove();
      return h;
    };
    const r = (el) => (el ? el.getBoundingClientRect() : null);
    return {
      docScroll: document.documentElement.scrollHeight,
      winH: window.innerHeight,
      scrollY: window.scrollY,
      panel: r(panel),
      dock: r(dock),
      scroller: scroller
        ? { scrollTop: scroller.scrollTop, scrollHeight: scroller.scrollHeight, clientHeight: scroller.clientHeight }
        : null,
      last: r(last),
      need: needShown ? r(need) : null,
      padding: scroller ? getComputedStyle(scroller).paddingBottom : null,
      shell: shell ? { height: r(shell).height, overflowY: getComputedStyle(shell).overflowY } : null,
      dvh: unit("100dvh"),
      vh: unit("100vh"),
    };
  });
  ok(geom.scroller !== null, "the thread has its own scroller ([data-chat-scroll])");
  ok(geom.dock !== null, "the composer is a dock ([data-chat-dock])");
  ok(geom.docScroll <= geom.winH + 2, `the page itself does not scroll (doc ${geom.docScroll} > window ${geom.winH})`);
  ok(geom.scrollY === 0, "the window is never scrolled");
  ok(
    geom.scroller.scrollHeight > geom.scroller.clientHeight,
    "a long thread scrolls inside the panel",
  );
  ok(
    Math.abs(geom.dock.bottom - geom.panel.bottom) <= 2,
    `the dock sits at the panel's bottom (dock ${geom.dock.bottom} vs panel ${geom.panel.bottom})`,
  );
  ok(geom.dock.bottom <= geom.winH + 1, "the dock is inside the viewport");
  ok(
    geom.panel.bottom <= geom.winH + 1 && geom.panel.bottom >= geom.winH - 16,
    `the panel reaches the viewport's bottom (panel ${geom.panel.bottom}, window ${geom.winH})`,
  );
  ok(geom.last && geom.last.bottom <= geom.dock.top + 1, "the last message is never hidden behind the dock");
  if (geom.need) {
    ok(
      !overlaps(geom.last, geom.need),
      `the last message stays clear of Needs-you (message ${JSON.stringify(geom.last)} button ${JSON.stringify(geom.need)})`,
    );
  }
  // Headless Chrome gives 100vh and 100dvh the same used height, and a
  // CDP device-metrics override scales them together instead of inserting
  // mobile browser chrome, so a shorter dynamic viewport cannot be
  // emulated here. The shell is still checked against a 100dvh probe with
  // overflow hidden — that is what stops the page scrolling when a phone's
  // 100vh is taller. If a runner ever does split the units, the shell must
  // follow dvh.
  ok(geom.shell, "the home shell is marked");
  ok(geom.shell.overflowY === "hidden", `home locks the shell's overflow (${geom.shell.overflowY})`);
  ok(
    Math.abs(geom.shell.height - geom.dvh) <= 2,
    `the home shell is 100dvh (shell ${geom.shell.height}, dvh ${geom.dvh}, vh ${geom.vh})`,
  );
  if (geom.vh - geom.dvh > 2) {
    ok(geom.shell.height <= geom.dvh + 2, "when dvh is shorter than vh, the shell follows dvh");
    ok(geom.docScroll <= geom.dvh + 2, "the page does not grow to 100vh");
  }
  ok(
    geom.padding && parseFloat(geom.padding) > 0,
    "the thread reserves the dock's height as bottom padding",
  );
}

/** The jump pill clears the dock, and on a phone it does not share a
 *  hit target with the Needs-you button. */
async function assertPillClear(page) {
  const geom = await page.evaluate(() => {
    const pill = document.querySelector(".newpill")?.getBoundingClientRect() ?? null;
    const dock = document.querySelector("[data-chat-dock]")?.getBoundingClientRect() ?? null;
    const need = document.querySelector(".needbtn");
    const shown = !!(need && getComputedStyle(need).display !== "none" && need.getClientRects().length);
    return { pill, dock, need: shown ? need.getBoundingClientRect() : null };
  });
  ok(geom.pill && geom.dock && geom.pill.bottom <= geom.dock.top + 1, "the pill sits above the dock");
  if (geom.need) {
    ok(
      !overlaps(geom.pill, geom.need),
      `the jump pill stays clear of Needs-you (pill ${JSON.stringify(geom.pill)} button ${JSON.stringify(geom.need)})`,
    );
  }
}

async function main() {
  fs.mkdirSync(out, { recursive: true });
  const browser = await chromium.launch({
    headless: true,
    channel: "chrome",
    args: ["--host-resolver-rules=MAP *.localhost 127.0.0.1"],
  });
  const context = await browser.newContext({
    viewport,
    colorScheme: "dark",
    extraHTTPHeaders: seam,
  });
  try {
    const page = await openHome(context);
    const thread = page.locator('ol[aria-label="messages"]');

    if (phase === "not-started") {
      // The panel with no master: the "start it" state centred, the
      // dock present and disabled with its reason. Dark and light.
      await appear(page, '[data-empty="master"]', "the not-started card");
      await expectText(page.locator("[data-composer-block]"), "not started", "the dock's disabled reason");
      await page.waitForTimeout(300);
      await shot(page, `${size}-dark-not-started`);
      await page.evaluate(() => localStorage.setItem("cadence-theme", "light"));
      await page.emulateMedia({ colorScheme: "light" });
      await page.reload();
      await appear(page, '[data-empty="master"]', "the not-started card in light");
      await page.waitForTimeout(300);
      await shot(page, `${size}-light-not-started`);
      console.log(`cad600: ${tag} ${size} not-started evidence captured`);
      return;
    }

    // ---- Live: a long thread, the tail, the pill, both themes ----
    await appear(page, "section[aria-label='master thread'] h1", "the Master header");
    await appear(page, "ol[aria-label='messages']", "the thread");

    // Idempotent seeding: the scratch thread may already hold a page of
    // messages from the other tag's run. Top-level items only — nested
    // markdown lists (a briefing note) are not thread items.
    const items = page.locator('ol[aria-label="messages"] > li');
    const have = await items.count();
    for (let i = have; i < LONG_THREAD.length; i++) await send(page, LONG_THREAD[i], i + 1);
    // A fixed-id marker rides the tail: it is what the light-theme
    // reload below proves survived (the seeding above is count-based,
    // so no specific ask is guaranteed to be in the thread).
    const marker = `cad600 ${tag} evidence marker`;
    await send(page, marker, 999);
    await page.waitForFunction(
      (n) => document.querySelectorAll('ol[aria-label="messages"] > li').length >= n,
      LONG_THREAD.length,
      { timeout: 60_000 },
    );
    await page.waitForTimeout(1500); // a reply or two in
    await settle(page);
    await toTail(page);
    await page.waitForTimeout(400);
    if (asserts) await checkLayout(page);
    await shot(page, `${size}-dark-tail`);

    // The pill: scrolled up in the history, a new message arrives. (The
    // before layout only shows it while the page itself scrolls — the
    // shot is optional there; after, it is required.)
    await toTop(page);
    await page.waitForTimeout(200);
    // A fresh id every run: a repeated one is the same message to the
    // daemon (idempotent), so it would never be an arrival.
    await send(page, "one more while I read history", Date.now());
    const pill = page.locator(".newpill").first();
    try {
      await pill.waitFor({ state: "visible", timeout: asserts ? 15_000 : 6_000 });
    } catch {
      if (asserts) throw new Error("the jump-to-latest pill never appeared");
      console.log("note: the pill did not appear in the before layout — skipped");
    }
    if (await pill.isVisible()) {
      await expectText(pill, "new", "the pill counts the arrival");
      if (asserts) await assertPillClear(page);
      await page.waitForTimeout(300);
      await shot(page, `${size}-dark-pill`);
      await pill.click();
      await page.waitForTimeout(500);
      await shot(page, `${size}-dark-jumped`);
    }

    // Keyboard: Enter sends, Shift+Enter makes a newline, the focus
    // stays in the dock after a send (after only).
    if (asserts) {
      const box = page.getByLabel("message to the master");
      await box.click();
      await box.fill("first line");
      await box.press("Shift+Enter");
      await box.type("second line");
      const lines = await box.inputValue();
      ok(lines.includes("\n"), "Shift+Enter inserts a newline");
      const before = await box.evaluate((el) => el.getBoundingClientRect().height);
      await box.fill(Array.from({ length: 14 }, (_, i) => `line ${i + 1}`).join("\n"));
      const grown = await box.evaluate((el) => ({
        h: el.getBoundingClientRect().height,
        max: parseFloat(getComputedStyle(el).maxHeight),
        scrolls: el.scrollHeight > el.clientHeight,
      }));
      ok(grown.h > before, `the textarea grows with the draft (${before} → ${grown.h})`);
      ok(
        Math.abs(grown.h - grown.max) <= 2 && grown.scrolls,
        `past ~8 lines it scrolls (h ${grown.h}, max ${grown.max})`,
      );
      await box.fill("focus check");
      await page.locator(".sendbtn").click();
      await page.waitForTimeout(200);
      const focused = await page.evaluate(() => document.activeElement?.getAttribute("aria-label"));
      ok(focused === "message to the master", `focus stays in the dock after sending (${focused})`);
      await box.fill("");
      await toTail(page);
    }

    // Light theme.
    await page.evaluate(() => localStorage.setItem("cadence-theme", "light"));
    await page.emulateMedia({ colorScheme: "light" });
    await page.reload();
    await expectText(thread, marker, "the thread survives the reload");
    await settle(page);
    await toTail(page);
    await page.waitForTimeout(400);
    await shot(page, `${size}-light-tail`);

    // Phone, light: the same pill, now that the theme has flipped. The
    // dark shot above is not enough — the review's overlap was visible
    // in both themes, and the light tail is where the button covered
    // the last user bubble.
    if (size === "phone") {
      await toTop(page);
      await page.waitForTimeout(200);
      await send(page, "one more in the light theme", Date.now());
      try {
        await pill.waitFor({ state: "visible", timeout: asserts ? 15_000 : 6_000 });
      } catch {
        if (asserts) throw new Error("the jump-to-latest pill never appeared in the light theme");
        console.log("note: the light pill did not appear — skipped");
      }
      if (await pill.isVisible()) {
        await expectText(pill, "new", "the light pill counts the arrival");
        if (asserts) await assertPillClear(page);
        await page.waitForTimeout(300);
        await shot(page, `${size}-light-pill`);
      }
    }

    console.log(`cad600: ${tag} ${size} evidence captured${asserts ? " (layout checks passed)" : ""}`);
  } finally {
    await context.close();
    await browser.close();
  }
}

main().catch((e) => {
  console.error(e.message ?? e);
  process.exit(1);
});
