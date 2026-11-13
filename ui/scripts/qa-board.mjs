#!/usr/bin/env node
// CAD-2291: the QA board — one command that serves a checkout's UI against a
// `cadence dev` sandbox, with the hosted routes answered from fixtures as a
// synthetic hosted member. Usage and the control API: docs/BOARD.md, "QA board".
//
//   node ui/scripts/qa-board.mjs up --checkout <path> [--name <n>] [--cadence <bin>]
//   node ui/scripts/qa-board.mjs status [--name <n>]
//   node ui/scripts/qa-board.mjs down [--name <n>]
import { execFileSync, spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import https from "node:https";
import { createRequire } from "node:module";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT = fileURLToPath(import.meta.url);
const FIXTURES = path.join(path.dirname(SCRIPT), "qa-board", "fixtures");
const VALIDATOR = path.join(path.dirname(SCRIPT), "qa-board", "validate.ts");
const NAME = /^[a-z0-9][a-z0-9-]{0,15}$/;
const ROOT = /^\/tmp\/qa-board-[a-z0-9][a-z0-9-]{0,15}$/;
const SANDBOX = "qa";
const EVIDENCE = "Hosted surfaces (member chat, the Browser panel, member app screens) are answered from qa-board fixtures: this is FIXTURE EVIDENCE, not a real hosted board. A check on a real hosted board stays an operator canary.";

function args(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i].startsWith("--")) out[argv[i].slice(2)] = argv[++i];
    else out._.push(argv[i]);
  }
  return out;
}
function die(message) {
  console.error(`qa-board: ${message}`);
  process.exit(1);
}
function rootOf(name) {
  if (!NAME.test(name)) die(`--name must match ${NAME}`);
  const root = `/tmp/qa-board-${name}`;
  if (!ROOT.test(root)) die(`refusing state dir ${root}`);
  return root;
}
const readJson = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
const writeJson = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const git = (dir, ...a) => execFileSync("git", ["-C", dir, ...a], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
function revision(dir) {
  try {
    const dirty = git(dir, "status", "--porcelain").split("\n").filter(Boolean).length;
    return { sha: git(dir, "rev-parse", "HEAD"), clean: dirty === 0, dirty_files: dirty };
  } catch {
    return { sha: "unverified", clean: false };
  }
}
/** The sandbox verbs, scoped to this board's own sandbox base. */
function cadence(bin, root, ...a) {
  const env = { ...process.env, CADENCE_SANDBOX_ROOT: root };
  for (const key of ["CADENCE_STATE_DIR", "CADENCE_PM_DIR", "CADENCE_PROFILE", "CADENCE_ORG"]) delete env[key];
  return spawnSync(bin, ["dev", ...a], { env, encoding: "utf8", timeout: 120_000 });
}

// ---- ports and processes -------------------------------------------------

function portFree(port) {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.once("error", () => resolve(false));
    server.listen({ port, host: "0.0.0.0", exclusive: true }, () => server.close(() => resolve(true)));
  });
}
async function freePorts(count) {
  const ports = [];
  for (let port = 3110; port <= 3199 && ports.length < count; port++) if (await portFree(port)) ports.push(port);
  if (ports.length < count) die("no free ports left in 3110-3199");
  return ports;
}
function listening(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ port, host: "127.0.0.1" });
    socket.once("connect", () => { socket.destroy(); resolve(true); });
    socket.once("error", () => resolve(false));
  });
}
function cmdline(pid) {
  try { return fs.readFileSync(`/proc/${pid}/cmdline`, "utf8").split("\0"); } catch { return null; }
}
/** Our process, identified by a marker in its command line (never a reused pid). */
const ours = (proc) => Number.isInteger(proc?.pid) && (cmdline(proc.pid) ?? []).join(" ").includes(proc.marker);
function signal(proc, sig) {
  try { process.kill(proc.group ? -proc.pid : proc.pid, sig); } catch { /* already gone */ }
}
async function stop(procs) {
  const live = procs.filter(ours);
  for (const proc of live) signal(proc, "SIGTERM");
  for (let i = 0; i < 50 && live.some(ours); i++) await sleep(200);
  for (const proc of live.filter(ours)) signal(proc, "SIGKILL");
  for (let i = 0; i < 25 && live.some(ours); i++) await sleep(200);
  return live.filter(ours).map((proc) => proc.pid);
}
function sandboxPids(stateDir) {
  const found = [];
  for (const pid of fs.readdirSync("/proc").filter((p) => /^\d+$/.test(p)).map(Number)) {
    const argv = cmdline(pid);
    const at = argv?.indexOf("--state-dir") ?? -1;
    if (at >= 0 && argv[at + 1] === stateDir && argv[at + 3] === "run") found.push({ pid, marker: `--state-dir ${stateDir} ${argv[at + 2]} run` });
  }
  return found;
}
function fetchJson(url, { insecure = false } = {}) {
  return new Promise((resolve, reject) => {
    const get = url.startsWith("https:") ? https.get : http.get;
    const req = get(url, { rejectUnauthorized: !insecure, timeout: 5000 }, (res) => {
      let body = "";
      res.on("data", (c) => (body += c));
      res.on("end", () => (res.statusCode === 200 ? resolve(body.trim().startsWith("{") ? JSON.parse(body) : body) : reject(new Error(`${url}: ${res.statusCode}`))));
    });
    req.on("timeout", () => req.destroy(new Error(`${url}: timeout`)));
    req.on("error", reject);
  });
}
async function ready(url, opts) {
  for (let i = 0; i < 120; i++) {
    try { return await fetchJson(url, opts); } catch { await sleep(500); }
  }
  throw new Error(`${url} did not answer within 60 s`);
}

// ---- up ------------------------------------------------------------------

/** Compile validate.ts against the served checkout's ui/src, with the ui
 *  tests' own compiler options, and run it over the payloads to be served. */
function validate(root, ui, served, viewUrl) {
  const check = path.join(root, "check");
  fs.mkdirSync(path.join(check, "scripts", "qa-board"), { recursive: true });
  fs.cpSync(path.join(ui, "src"), path.join(check, "src"), { recursive: true });
  fs.symlinkSync(path.join(ui, "node_modules"), path.join(check, "node_modules"));
  fs.copyFileSync(VALIDATOR, path.join(check, "scripts", "qa-board", "validate.ts"));
  const { compilerOptions } = readJson(path.join(ui, "tsconfig.tests.json"));
  writeJson(path.join(check, "tsconfig.json"), { compilerOptions: { ...compilerOptions, rootDir: ".", outDir: "out", noEmitOnError: true }, files: ["scripts/qa-board/validate.ts"] });
  const tsc = spawnSync(path.join(ui, "node_modules", ".bin", "tsrs"), ["-p", path.join(check, "tsconfig.json")], { encoding: "utf8" });
  if (tsc.status !== 0) throw new Error(`the fixture validator does not compile against ${ui}/src:\n${tsc.stdout}${tsc.stderr}`);
  const compiled = path.join(check, "out", "scripts", "qa-board", "validate.js");
  const run = spawnSync(process.execPath, [compiled, served, viewUrl], { encoding: "utf8", env: { ...process.env, NODE_PATH: path.join(ui, "node_modules") } });
  let result;
  try { result = JSON.parse(run.stdout.trim().split("\n").pop()); } catch { result = { ok: false, failures: [`validator output unreadable: ${run.stdout}${run.stderr}`] }; }
  if (run.status !== 0 || !result.ok) throw new Error(`fixture payloads refused by the UI's own parsers:\n  ${result.failures.join("\n  ")}`);
  return compiled;
}

async function up(opts) {
  const name = opts.name ?? "qa";
  const root = rootOf(name);
  if (!opts.checkout) die("up needs --checkout <path>");
  const checkout = fs.realpathSync(opts.checkout);
  const ui = path.join(checkout, "ui");
  for (const tool of ["vite", "tsrs"]) {
    if (!fs.existsSync(path.join(ui, "node_modules", ".bin", tool))) {
      die(`${ui}/node_modules has no ${tool}: hard-link the shared ui/node_modules into this checkout (never bun install here)`);
    }
  }
  if (fs.existsSync(root)) die(`${root} exists: run \`down --name ${name}\` first`);
  fs.mkdirSync(root, { mode: 0o700 });
  fs.chmodSync(root, 0o700);
  const bin = opts.cadence ?? "cadence";
  const state = { name, root, checkout, cadence: bin, procs: [], ports: {} };
  const save = () => writeJson(path.join(root, "state.json"), state);
  save();
  try {
    const [sandboxPort, proxyPort, viewerPort, uiPort] = await freePorts(4);
    state.ports = { ui: uiPort, fixture_proxy: proxyPort, viewer: viewerPort, sandbox: sandboxPort };
    save();

    // (a) the sandbox board, for native routes
    const devUp = cadence(bin, root, "up", SANDBOX, "--port", String(sandboxPort));
    fs.writeFileSync(path.join(root, "sandbox-up.log"), `${devUp.stdout}${devUp.stderr}`);
    const stateDir = path.join(root, SANDBOX, "state");
    state.procs.push(...sandboxPids(stateDir).map((p) => ({ ...p, role: "sandbox" })));
    save();
    if (devUp.status !== 0) throw new Error(`cadence dev up failed (exit ${devUp.status}): ${devUp.stderr.trim()}`);
    const version = await ready(`http://127.0.0.1:${sandboxPort}/api/version`);
    const sandboxMeta = await fetchJson(`http://127.0.0.1:${sandboxPort}/api/meta`);

    // The payloads exactly as served: the fixtures, with the sandbox's build
    // identity in /api/meta (it is the backend serving every native route).
    const served = path.join(root, "served");
    fs.mkdirSync(served);
    for (const file of fs.readdirSync(FIXTURES).filter((f) => f.endsWith(".json"))) {
      const payload = readJson(path.join(FIXTURES, file));
      if (file === "meta.json") for (const key of ["build_commit", "build_time", "version", "daemon"]) payload[key] = sandboxMeta[key] ?? null;
      writeJson(path.join(served, file), payload);
    }
    const viewUrl = `https://127.0.0.1:${viewerPort}/live`;
    state.validator = validate(root, ui, served, viewUrl);

    // (e) the stand-in viewer's self-signed certificate; the key stays in a file
    const certs = path.join(root, "viewer");
    fs.mkdirSync(certs, { mode: 0o700 });
    execFileSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", "/CN=127.0.0.1",
      "-addext", "subjectAltName=IP:127.0.0.1", "-keyout", path.join(certs, "key.pem"), "-out", path.join(certs, "cert.pem")], { stdio: "ignore" });
    save();

    // (c)+(d)+(e) the fixture proxy, control endpoint and viewer: one process
    const log = (file) => fs.openSync(path.join(root, file), "a", 0o600);
    const serve = spawn(process.execPath, [SCRIPT, "serve", "--name", name], { detached: true, stdio: ["ignore", log("serve.log"), log("serve.log")], cwd: root });
    state.procs.push({ pid: serve.pid, group: true, marker: `serve --name ${name}`, role: "fixture-proxy" });
    save();
    serve.unref();
    // (b) Vite from the checkout's ui/, proxying /api to the fixture proxy
    const env = { ...process.env, CADENCE_DEV_BOARD_PORT: String(proxyPort) };
    delete env.CADENCE_BUILD_COMMIT;
    const vite = spawn(process.execPath, [path.join(ui, "node_modules", "vite", "bin", "vite.js"), "--host", "127.0.0.1", "--port", String(uiPort), "--strictPort"],
      { detached: true, stdio: ["ignore", log("vite.log"), log("vite.log")], cwd: ui, env });
    state.procs.push({ pid: vite.pid, group: true, marker: `--port ${uiPort} --strictPort`, role: "vite" });
    save();
    vite.unref();
    await ready(`http://127.0.0.1:${proxyPort}/__qa/counts`);
    await ready(`https://127.0.0.1:${viewerPort}/ping`, { insecure: true });
    await ready(`http://127.0.0.1:${uiPort}/`);

    const provenance = {
      qa_board: name,
      fixture_evidence: EVIDENCE,
      ui: { checkout, ...revision(checkout), url: `http://127.0.0.1:${uiPort}/`, served_by: "vite dev server from the checkout's ui/" },
      sandbox: { build: version.build ?? "unverified", hosted: sandboxMeta.hosted, url: `http://127.0.0.1:${sandboxPort}/`, root: path.join(root, SANDBOX) },
      fixtures: { dir: FIXTURES, ...revision(path.dirname(SCRIPT)), validated_against: `${ui}/src` },
      ports: state.ports,
      control: `http://127.0.0.1:${proxyPort}/__qa/state`,
      counts: `http://127.0.0.1:${proxyPort}/__qa/counts`,
      viewer: `${viewUrl} (self-signed: run the browser with --ignore-https-errors)`,
      state_dir: root,
      down: `node ${SCRIPT} down --name ${name}`,
    };
    state.provenance = provenance;
    save();
    console.log(JSON.stringify(provenance, null, 2));
  } catch (error) {
    console.error(`qa-board: up failed: ${error.message}`);
    await down({ name }, true);
    process.exit(1);
  }
}

// ---- down and status -----------------------------------------------------

async function down(opts, quiet = false) {
  const name = opts.name ?? "qa";
  const root = rootOf(name);
  const stat = fs.lstatSync(root, { throwIfNoEntry: false });
  if (!stat) {
    if (!quiet) console.log(JSON.stringify({ qa_board: name, state: "absent" }));
    return;
  }
  if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid()) die(`${root} is not this user's QA board directory`);
  let state = { procs: [], ports: {}, cadence: "cadence" };
  try { state = readJson(path.join(root, "state.json")); } catch { /* a partial up: nothing recorded */ }
  const left = await stop(state.procs);
  const reset = fs.existsSync(path.join(root, SANDBOX)) ? cadence(state.cadence, root, "reset", SANDBOX) : null;
  if (left.length === 0) fs.rmSync(root, { recursive: true, force: true });
  const open = [];
  for (const port of Object.values(state.ports)) if (await listening(port)) open.push(port);
  const report = {
    qa_board: name,
    state: left.length === 0 && open.length === 0 && !fs.existsSync(root) ? "removed" : "incomplete",
    stopped: state.procs.map((p) => ({ role: p.role, pid: p.pid })),
    still_running: left,
    open_ports: open,
    sandbox_reset: reset === null ? "absent" : reset.status === 0 ? "ok" : `failed: ${reset.stderr.trim()}`,
    state_dir_removed: !fs.existsSync(root),
  };
  if (!quiet) console.log(JSON.stringify(report, null, 2));
  if (report.state !== "removed") process.exitCode = 1;
}

async function status(opts) {
  const names = opts.name ? [opts.name] : fs.readdirSync("/tmp").filter((d) => d.startsWith("qa-board-")).map((d) => d.slice(9)).filter((n) => NAME.test(n));
  const boards = [];
  for (const name of names) {
    const root = rootOf(name);
    let state;
    try { state = readJson(path.join(root, "state.json")); } catch { boards.push({ qa_board: name, state: fs.existsSync(root) ? "unreadable" : "absent" }); continue; }
    const ports = {};
    for (const [role, port] of Object.entries(state.ports)) ports[role] = { port, listening: await listening(port) };
    const procs = state.procs.map((p) => ({ role: p.role, pid: p.pid, running: ours(p) }));
    const up = procs.length > 0 && procs.every((p) => p.running) && Object.values(ports).every((p) => p.listening);
    boards.push({ qa_board: name, state: up ? "running" : "degraded", procs, ports, provenance: state.provenance ?? null });
  }
  console.log(JSON.stringify(boards, null, 2));
}

// ---- serve: the fixture proxy, control endpoint and stand-in viewer -------

function serve(opts) {
  const root = rootOf(opts.name);
  const state = readJson(path.join(root, "state.json"));
  const fx = Object.fromEntries(fs.readdirSync(path.join(root, "served")).map((f) => [f.slice(0, -5), readJson(path.join(root, "served", f))]));
  // The UI's own parser, compiled at `up`: a state set mid-run is checked too.
  const { checkBrowser, BROWSER_PRESETS } = createRequire(SCRIPT)(state.validator);
  const { sandbox, viewer } = state.ports;
  let browser = { ...fx["browser-handoff"] };
  const locale = { ...fx.locale };
  const history = fx["member-assistant-history"];
  const entries = [...history.entries];
  let views = 0;
  const counts = { routes: {}, proxied: 0, masked_401: {}, views: 0, viewer_loads: 0, viewer_done: 0 };
  const count = (key) => (counts.routes[key] = (counts.routes[key] ?? 0) + 1);

  const send = (res, status, body, type = "application/json") => {
    res.writeHead(status, { "Content-Type": type, "Cache-Control": "no-store" });
    res.end(type === "application/json" ? JSON.stringify(body) : body);
  };
  const body = (req) => new Promise((resolve) => {
    let raw = "";
    req.on("data", (c) => (raw += c));
    req.on("end", () => { try { resolve(raw ? JSON.parse(raw) : {}); } catch { resolve(null); } });
  });
  // Opaque positions in the member history: `csc1_<entries seen>`.
  const at = (token) => (typeof token === "string" && /^csc1_\d+$/.test(token) ? Number(token.slice(5)) : null);
  function page(req) {
    const limit = Math.max(1, Math.min(Number(req.limit) || 200, 200));
    const n = entries.length;
    let from;
    let to;
    if (req.before !== undefined) { to = Math.min(at(req.before) ?? n, n); from = Math.max(0, to - limit); }
    else if (req.tail) { to = n; from = Math.max(0, n - limit); }
    else { from = Math.min(at(req.after) ?? 0, n); to = Math.min(n, from + limit); }
    const rows = entries.slice(from, to);
    const empty = rows.length === 0;
    return {
      conversation: history.conversation,
      entries: rows,
      cursor: empty && !req.tail && req.before === undefined ? (req.after ?? null) : empty && n === 0 ? null : `csc1_${to}`,
      before_cursor: empty && n === 0 ? null : `csc1_${from}`,
      more_before: from > 0,
      more_after: to < n,
    };
  }

  const fixtures = {
    "GET /api/meta": () => fx.meta,
    "GET /api/locale": () => locale,
    "POST /api/locale": (b) => {
      if (b?.scope === "user") locale.user_locale = b.locale ?? null;
      else if (b?.scope === "organization") locale.organization_locale = b.locale ?? null;
      return locale;
    },
    "POST /api/member-assistant/state": () => fx["member-assistant-state"],
    "POST /api/member-assistant/profile": () => fx["member-assistant-profile"],
    "POST /api/member-assistant/history": (b) => page(b ?? {}),
    "POST /api/member-assistant/send": (b) => {
      entries.push({ id: `csrid_qa_${entries.length + 1}`, role: "member", kind: "message", text: String(b?.text ?? ""), message: b?.message ?? null, created: Date.now() / 1000 });
      return { ...fx["member-assistant-send"], message: b?.message };
    },
    "GET /api/browser": () => browser,
    "POST /api/browser/view": (_b, res) => {
      if (browser.state === "none" || browser.state === "ended") return send(res, 409, { error: "not_in_handoff" });
      counts.views = ++views;
      return { url: `https://127.0.0.1:${viewer}/live?view=${views}` };
    },
    "POST /api/browser/stop": () => {
      browser = { ...fx["browser-ended"] };
      return fx["browser-stop"];
    },
  };

  function proxy(req, res, pathname) {
    counts.proxied++;
    const upstream = http.request({ host: "127.0.0.1", port: sandbox, method: req.method, path: req.url, headers: { ...req.headers, host: `127.0.0.1:${sandbox}` } }, (up) => {
      // The sandbox is signed out: a 401 would sign the fixture member out of
      // the whole shell. Shell routes the fixtures do not answer read as absent.
      if (up.statusCode === 401) {
        up.resume();
        counts.masked_401[pathname] = (counts.masked_401[pathname] ?? 0) + 1;
        return send(res, 404, { code: "qa_board_masked_401", error: "the signed-out sandbox refused this route; the QA board answers it as absent" });
      }
      res.writeHead(up.statusCode, up.headers);
      up.pipe(res);
    });
    upstream.on("error", (e) => (res.headersSent ? res.destroy() : send(res, 502, { error: `sandbox unreachable: ${e.message}` })));
    req.pipe(upstream);
  }

  async function control(req, res, pathname) {
    if (pathname === "/__qa/counts" && req.method === "GET") return send(res, 200, counts);
    if (pathname !== "/__qa/state") return send(res, 404, { error: "unknown control route" });
    if (req.method === "POST") {
      const b = await body(req);
      if (b === null || typeof b !== "object") return send(res, 400, { error: "body must be JSON" });
      if (b.browser !== undefined && !BROWSER_PRESETS.includes(b.browser)) return send(res, 400, { error: `browser preset must be one of ${BROWSER_PRESETS.join(", ")}` });
      const next = b.browser !== undefined ? { ...fx[`browser-${b.browser}`] } : { ...browser };
      for (const key of ["state", "signedIn", "url", "instructions"]) if (key in b) next[key] = b[key];
      const why = checkBrowser(next);
      if (why) return send(res, 400, { error: `browser state refused: ${why}` });
      if ("locale" in b && b.locale !== null && b.locale !== "en" && b.locale !== "zh-TW") return send(res, 400, { error: "locale must be en, zh-TW or null" });
      browser = next;
      if ("locale" in b) locale.user_locale = b.locale;
    } else if (req.method !== "GET") return send(res, 405, { error: "GET or POST" });
    return send(res, 200, { browser, locale, views });
  }

  http.createServer(async (req, res) => {
    const { pathname } = new URL(req.url, "http://qa-board.invalid");
    if (pathname.startsWith("/__qa/")) return control(req, res, pathname);
    const handler = fixtures[`${req.method} ${pathname}`];
    if (!handler) return proxy(req, res, pathname);
    count(`${req.method} ${pathname}`);
    const answer = handler(req.method === "POST" ? await body(req) : null, res);
    if (answer !== undefined) send(res, 200, answer);
  }).listen(state.ports.fixture_proxy, "127.0.0.1");

  const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);
  https.createServer({ key: fs.readFileSync(path.join(root, "viewer", "key.pem")), cert: fs.readFileSync(path.join(root, "viewer", "cert.pem")) }, async (req, res) => {
    const { pathname } = new URL(req.url, "https://qa-board.invalid");
    if (pathname === "/done" && req.method === "POST") {
      // The provider's own Done: the takeover ends and the assistant resumes.
      counts.viewer_done++;
      if (browser.state === "handoff") browser = { ...browser, state: "active", instructions: null };
      return send(res, 200, { browser });
    }
    if (pathname === "/ping") return send(res, 200, "ok", "text/plain");
    if (pathname !== "/live") return send(res, 404, "not found", "text/plain");
    counts.viewer_loads++;
    send(res, 200, `<!doctype html><meta charset="utf-8"><title>QA board live view</title>
<body style="font:14px system-ui;margin:0;padding:16px;background:#f4f4f5;color:#18181b">
<p style="margin:0 0 8px;font-weight:600">Stand-in live view (QA board fixture, not a provider)</p>
<p style="margin:0 0 4px">Page: <code>${esc(browser.url)}</code></p>
<p style="margin:0 0 12px">Signed in: ${browser.signedIn ? "yes" : "no"} · state: ${esc(browser.state)}</p>
<button onclick="fetch('/done',{method:'POST'}).then(()=>{this.textContent='Done sent'})">Done</button>
</body>`, "text/html; charset=utf-8");
  }).listen(viewer, "127.0.0.1");

  process.on("SIGTERM", () => process.exit(0));
}

const opts = args(process.argv.slice(2));
const verb = opts._[0];
if (verb === "up") await up(opts);
else if (verb === "down") await down(opts);
else if (verb === "status") await status(opts);
else if (verb === "serve") serve(opts);
else die("usage: qa-board.mjs up --checkout <path> [--name <n>] [--cadence <bin>] | status [--name <n>] | down [--name <n>]");
