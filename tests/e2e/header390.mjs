// CAD-429: on a read-only board the header must fit 390 px — every
// route, both themes, daemon reachable and not. Writable boards are
// covered too, signed in and out — the SignIn chip is the widest case.
//
//   cd ui && pnpm build          # the script serves ui/dist as-is
//   cd tests/e2e && pnpm install # playwright-core (board.mjs's stack)
//   node header390.mjs           # E2E_CHROME=<binary> overrides Chrome
//
// A stub server answers the board's API — meta mode and the `daemon`
// field flip between passes — so no daemon or pm dir is needed. For
// each (theme, meta mode, daemon, route) the page opens at 390x844 and
// the document must not scroll sideways; the collapsed header chips must
// still expose their words (sr-only label + title) and the opened menu
// must show them in full. Prints one JSON line per pass and a summary;
// exits 1 when anything overflows.

import { chromium } from "playwright-core";
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const DIST = path.join(ROOT, "ui", "dist");
const WIDTH = 390;
const HEIGHT = 844;
const ROUTES = [
  "/",
  "/overview",
  "/projects",
  "/projects/demo",
  "/projects/demo/epics",
  "/projects/demo/milestones",
  "/projects/demo/context",
  "/agents",
  "/agents/swe-1",
  "/setup",
  "/settings",
  "/settings/memory",
  "/login",
  "/no-such-page",
  "/projects/demo?issue=DEMO-1",
];
const THEMES = ["dark", "light"];
const DAEMONS = ["reachable", "unreachable"];

/** /api/meta variants — a read-only board, a writable one signed out
 *  (the sign-in chip) and one signed in (sign-out + writes chips). */
const MODES = {
  readonly: {
    meta: {
      read_only: true,
      actor: "viewer (read-only)",
      tailnet_url: null,
      version: "0.0.0-e2e",
      build_commit: "e2e",
      build_time: "e2e",
      daemon: null,
    },
    // Header chips these `title`s prove meta + health landed.
    wait: ["refuses every write", "daemon socket"],
    // The menu must show these words at 390 px.
    menu: (daemon) => ["read-only", `daemon ${daemon}`, "refresh"],
  },
  unsigned: {
    meta: {
      read_only: false,
      signed_in: false,
      login_hint: "cadence ui login",
      actor: "operator (ui)",
      tailnet_url: null,
      version: "0.0.0-e2e",
      build_commit: "e2e",
      build_time: "e2e",
      daemon: null,
    },
    wait: ["daemon socket", "operator's session"],
    menu: (daemon) => [`daemon ${daemon}`, "refresh", "sign in"],
  },
  signed: {
    meta: {
      read_only: false,
      signed_in: true,
      operator: true,
      session: {
        id: "s-e2e",
        origin: "loopback",
        created: 0,
        last_used: 0,
        idle_expires_at: 0,
        expires_at: 0,
        user_agent: "e2e",
      },
      actor: "operator (ui)",
      tailnet_url: null,
      version: "0.0.0-e2e",
      build_commit: "e2e",
      build_time: "e2e",
      daemon: null,
    },
    wait: ["daemon socket", "Signed in as", "writes commit"],
    menu: (daemon) => [`daemon ${daemon}`, "refresh", "sign out", "writes:"],
  },
};

// ---- the stub board --------------------------------------------------

const state = { daemon: "reachable", mode: "readonly" };

const issue = {
  id: "DEMO-1",
  project: "demo",
  title: "Seeded card — a reasonably long title to give the row real width",
  status: "doing",
  status_source: "file",
  priority: "P2",
  owner: "swe-1",
  component: "ui",
  tags: ["ui", "header"],
  blocked_by: [],
  relates: [],
  refs: [],
  container: false,
  ready: true,
  blocked: false,
  created: "2026-09-24T00:00:00Z",
  rev: "e2e",
  counts: { comments: 0, artifacts: 0, refs: 0 },
  checks: { done: 0, total: 0 },
};

const agent = {
  alias: "swe-1",
  provider: "devin",
  endpoint_kind: "pty",
  state: "idle",
  group: "lane-pm",
  running: 0,
  queued: 0,
  unknown: 0,
  parked: 0,
  fenced: false,
  on: [],
};

function api(pathname, res) {
  const json = (body, status = 200) => {
    res.writeHead(status, { "content-type": "application/json" });
    res.end(JSON.stringify(body));
  };
  const hold = () => {
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write(": e2e\n\n");
    // Held open: the app's EventSource stays live, nothing more to say.
  };
  if (pathname === "/api/stream" || pathname.endsWith("/stream")) return hold();
  if (pathname === "/api/health") {
    return json({
      ok: true,
      pm_dir: "/tmp/e2e-pm",
      pm_present: true,
      projects: 1,
      issues: 1,
      daemon: state.daemon,
      embedded: true,
    });
  }
  if (pathname === "/api/meta") return json(MODES[state.mode].meta);
  if (pathname === "/api/projects") {
    return json({
      projects: [
        { key: "demo", prefix: "DEMO", components: ["ui"], tags: [], repos: [], issues: 1 },
      ],
    });
  }
  if (pathname.endsWith("/context")) {
    return json({
      project: "demo",
      state: "ok",
      manifest: { path: "/tmp/e2e-pm/demo/manifest.md", state: "ok", errors: [] },
      snapshot: { revision_state: "clean", dirty: false, dirty_truncated: false },
      documents: [],
      memories: {
        included: [],
        lessons: "",
        withheld: [],
        withheld_total: 0,
        withheld_omitted: 0,
        load_errors: [],
        load_errors_total: 0,
        load_errors_omitted: 0,
        matched_total: 0,
      },
      limits: {
        max_manifest_bytes: 0,
        max_manifest_entries: 0,
        max_path_bytes: 0,
        max_title_bytes: 0,
        max_document_bytes: 0,
        max_excerpt_bytes: 0,
        max_git_output_bytes: 0,
        max_memory_entries: 0,
        max_memory_lesson_bytes: 0,
        max_response_bytes: 0,
        response_truncated: false,
        response_excerpt_reductions: 0,
      },
    });
  }
  if (pathname === "/api/issues") return json({ issues: [issue] });
  if (pathname === `/api/issues/${issue.id}`) {
    return json({
      ...issue,
      frontmatter: {},
      body: "A seeded issue body.\n",
      path: "/tmp/e2e-pm/demo/issues/DEMO-1/issue.md",
      links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
      comments: [],
      notes_chain: [],
      artifacts: [],
      activity: [],
    });
  }
  if (pathname.endsWith("/history")) return json({ id: issue.id, history: [] });
  if (pathname === "/api/agents") {
    return json({
      daemon: state.daemon,
      agents: [agent],
      totals: { running: 0, queued: 0, fenced: 0, parked: 0, inboxes: 0 },
      by_issue: {},
    });
  }
  if (pathname === `/api/agents/${agent.alias}`) {
    return json({
      agent: { ...agent, enabled: true },
      queued: 0,
      unknown: 0,
      running: [],
      events: [],
      fenced: false,
    });
  }
  if (pathname === "/api/overview") {
    return json({
      needs_me: [],
      drift: { matched: false },
      projects: [{ key: "demo", open_by_status: { doing: 1 } }],
      github: { state: "unavailable" },
      daemon: { reachable: state.daemon === "reachable" },
      generated_at: Math.floor(Date.now() / 1000),
    });
  }
  if (pathname === "/api/milestones") return json({ milestones: [] });
  if (pathname === "/api/memories") return json({ memories: [] });
  if (pathname === "/api/settings/model-defaults") {
    return json({
      revision: 1,
      config: { schema: 1, providers: {} },
      providers: [],
      roles: [],
      read_only: true,
    });
  }
  if (pathname === "/api/setup") {
    return json({
      checks: [],
      checked_at: Date.now(),
      detect_only: true,
      ran_now: true,
      age_ms: 0,
      recheck_in_ms: 0,
    });
  }
  if (pathname.startsWith("/api/threads/")) {
    return json({
      alias: "master",
      thread: { id: "t1", alias: "master", created: "2026-09-24T00:00:00Z", updated: "2026-09-24T00:00:00Z" },
      entries: [],
      cursor: 0,
    });
  }
  if (pathname.startsWith("/api/master/")) return json({});
  return json({ error: "no such api" }, 404);
}

const MIME = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
  ".json": "application/json",
};

const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://127.0.0.1");
  if (url.pathname.startsWith("/api/")) return api(url.pathname, res);
  let file = path.join(DIST, url.pathname === "/" ? "index.html" : url.pathname);
  if (!file.startsWith(DIST) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
    file = path.join(DIST, "index.html"); // SPA fallback
  }
  res.writeHead(200, { "content-type": MIME[path.extname(file)] ?? "application/octet-stream" });
  fs.createReadStream(file).pipe(res);
});

// ---- the check -------------------------------------------------------

/** The menu's worded chips must carry what the header's icons mean —
 *  some rendered match, not just a hidden span holding the text. */
async function expectMenuWords(page, words) {
  const nav = page.locator("nav");
  for (const word of words) {
    const shown = await nav
      .getByText(word, { exact: false })
      .evaluateAll((els) =>
        els.some((e) => e.getBoundingClientRect().width > 2),
      )
      .catch(() => false);
    if (!shown) throw new Error(`the menu lost the words "${word}"`);
  }
}

/** The chips this change collapses — identified by their titles. */
const COLLAPSED = /refuses every write|daemon socket|re-read the folders/;

async function pass(page, base, route, mode, daemon) {
  await page.goto(`${base}${route}`);
  // The sticky app bar, not the `<header>` of a card or a drawer.
  const appbar = page.locator("header.sticky");
  await appbar.waitFor();
  // Meta + health landed once the mode's chips are on the bar.
  await page.waitForFunction(
    (want) => {
      const titles = [...document.querySelectorAll("header.sticky .chip")].map(
        (c) => c.getAttribute("title") ?? "",
      );
      return want.every((w) => titles.some((t) => t.includes(w)));
    },
    MODES[mode].wait,
    { timeout: 10_000 },
  );
  // A `?issue=` or `/agents/<alias>` route opens a drawer whose scrim
  // covers the menu button — close it (a no-op everywhere else).
  await page.keyboard.press("Escape");
  await page.locator(".drawer").waitFor({ state: "detached" }).catch(() => {});
  const m = await page.evaluate(() => ({
    inner: window.innerWidth,
    doc: document.documentElement.scrollWidth,
    body: document.body?.scrollWidth ?? 0,
  }));
  const overflow = Math.max(m.doc, m.body);
  if (overflow > m.inner) {
    throw new Error(`scrollWidth ${overflow} > innerWidth ${m.inner}`);
  }
  // Collapsed chips keep their meaning: an icon, a title, words still in
  // the DOM. Chips that manage their own small-screen shape (SignIn's
  // "sign in", or a display:none "writes:" waiting for lg) only need the
  // title + words.
  const chips = await page.evaluate(() =>
    [...document.querySelectorAll("header.sticky .chip")]
      .filter((c) => c.offsetParent !== null)
      .map((c) => ({
        title: c.getAttribute("title") ?? "",
        icon: c.querySelector("svg")?.getBoundingClientRect().width ?? 0,
        words: (c.textContent ?? "").trim(),
        labelVisible: [...c.querySelectorAll("span")].some(
          (s) =>
            s.getBoundingClientRect().width > 2 && (s.textContent ?? "").trim() !== "",
        ),
      })),
  );
  for (const c of chips) {
    if (!c.title || !c.words) {
      throw new Error(`a header chip lost its meaning: ${JSON.stringify(c)}`);
    }
    if (COLLAPSED.test(c.title) && (c.icon <= 0 || c.labelVisible)) {
      throw new Error(`a collapsed chip did not collapse: ${JSON.stringify(c)}`);
    }
  }
  // The menu lists the same chips with their words.
  await page.getByLabel("menu").click();
  await expectMenuWords(page, MODES[mode].menu(daemon));
  await page.getByLabel("menu").click();
  return m;
}

async function main() {
  if (!fs.existsSync(path.join(DIST, "index.html"))) {
    throw new Error(`no ${path.join(DIST, "index.html")} — run 'pnpm build' in ui/ first`);
  }
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const launch = { headless: true };
  if (process.env.E2E_CHROME) launch.executablePath = process.env.E2E_CHROME;
  else launch.channel = "chrome";
  const browser = await chromium.launch(launch);
  const failures = [];
  let checks = 0;
  try {
    for (const theme of THEMES) {
      const context = await browser.newContext({ viewport: { width: WIDTH, height: HEIGHT } });
      await context.addInitScript(
        (t) => localStorage.setItem("cadence-theme", t),
        theme,
      );
      const page = await context.newPage();
      for (const mode of Object.keys(MODES)) {
        state.mode = mode;
        for (const daemon of DAEMONS) {
          state.daemon = daemon;
          for (const route of ROUTES) {
            try {
              const m = await pass(page, base, route, mode, daemon);
              checks++;
              console.log(
                JSON.stringify({ theme, mode, daemon, route, scrollWidth: m.doc, innerWidth: m.inner, ok: true }),
              );
            } catch (e) {
              checks++;
              failures.push({ theme, mode, daemon, route, error: String(e.message ?? e) });
              console.log(JSON.stringify({ theme, mode, daemon, route, ok: false, error: String(e.message ?? e) }));
            }
          }
        }
      }
      await context.close();
    }
  } finally {
    await browser.close();
    server.close();
  }
  if (failures.length) {
    console.error(`header390: ${failures.length}/${checks} checks failed`);
    process.exit(1);
  }
  console.log(`header390: all ${checks} checks passed at ${WIDTH}px`);
}

main().catch((e) => {
  console.error(`header390: ${e?.stack ?? e}`);
  process.exit(1);
});
