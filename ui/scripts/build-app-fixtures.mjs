import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, writeFile, mkdir } from "node:fs/promises";
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "vite";

const ui = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repo = resolve(ui, "..");
const artifact = resolve(repo, "src/app_preview_export/producer.mjs");
const baselinePath = resolve(repo, "src/app_preview_export/baseline.json");
const contract = "app-screen-projection-fixtures/5";
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const fail = message => { console.error(`app fixture drift: ${message}`); process.exitCode = 1; };

function git(args, options = {}) {
  return execFileSync("git", ["-C", repo, ...args], {
    encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], ...options,
  }).trim();
}
function eventBase() {
  if (process.env.GITHUB_EVENT_PATH) {
    const event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"));
    const name = process.env.GITHUB_EVENT_NAME;
    if (name === "pull_request" || name === "pull_request_target") return event.pull_request?.base?.sha ?? null;
    if (name === "merge_group") return event.merge_group?.base_sha ?? null;
    if (name === "push" && event.before !== "0".repeat(40)) return event.before ?? null;
    if (name === "workflow_dispatch" || name === "push") {
      const commit = git(["cat-file", "-p", "HEAD"]);
      const parentHeader = commit.split("\n").find(line => line.startsWith("parent "));
      if (!parentHeader) throw new Error(`${name} HEAD commit has no parent (root commit)`);
      const parent = /^parent ([0-9a-f]{40})$/.exec(parentHeader)?.[1];
      if (!parent) throw new Error(`${name} HEAD commit has a malformed first parent`);
      return parent;
    }
    throw new Error(`unsupported CI event for trusted fixture base: ${name ?? "unknown"}`);
  }
  if (process.env.CI === "true") throw new Error("CI fixture check requires its GitHub event payload");
  return git(["merge-base", "HEAD", "origin/main"]);
}

function fetchMissingCiBase(base) {
  const repository = process.env.GITHUB_REPOSITORY;
  const server = process.env.GITHUB_SERVER_URL;
  if (process.env.CI !== "true" || !repository || !server)
    throw new Error(`trusted base commit ${base} is not available locally (offline checks do not fetch)`);
  const expected = new URL(server);
  const origin = new URL(git(["remote", "get-url", "origin"]));
  const expectedPath = `/${repository.replace(/^\/+|\/+$/g, "")}`;
  if (origin.protocol !== "https:" || origin.hostname !== expected.hostname ||
      ![expectedPath, `${expectedPath}.git`].includes(origin.pathname.replace(/\/+$/, "")))
    throw new Error("configured CI origin does not match the event repository");
  execFileSync("git", ["-C", repo, "fetch", "--no-tags", "--no-write-fetch-head", "--depth=1", "origin", base],
    { stdio: ["ignore", "ignore", "pipe"] });
}

async function buildArtifact(outDir) {
  await mkdir(outDir, { recursive: true });
  await build({ configFile: resolve(ui, "vite.app-fixtures.config.ts"), mode: "production",
    build: { outDir, emptyOutDir: true, sourcemap: true } });
  const js = await readFile(resolve(outDir, "producer.mjs"));
  const map = JSON.parse(await readFile(resolve(outDir, "producer.mjs.map"), "utf8"));
  if (!Array.isArray(map.sources) || !Array.isArray(map.sourcesContent) ||
      map.sources.length === 0 || map.sources.length !== map.sourcesContent.length ||
      map.sources.some((source, i) => typeof source !== "string" || typeof map.sourcesContent[i] !== "string"))
    throw new Error("Vite did not emit a complete producer dependency graph with source content");
  const mapPath = resolve(outDir, "producer.mjs.map");
  const sourceRoot = map.sourceRoot ?? "";
  const dependencyEntries = map.sources.map((source, index) => {
    const absolute = resolve(dirname(mapPath), sourceRoot, source);
    const key = relative(repo, absolute).split(sep).join("/");
    if (!key || key === ".." || key.startsWith("../") || resolve(repo, key) !== absolute)
      throw new Error(`producer dependency is outside the repository: ${source}`);
    return [key, sha(map.sourcesContent[index])];
  }).sort(([a], [b]) => a.localeCompare(b));
  if (new Set(dependencyEntries.map(([key]) => key)).size !== dependencyEntries.length)
    throw new Error("Vite source map contains duplicate canonical producer dependencies");
  const dependencies = dependencyEntries.map(([path, hash]) => ({ path, sha256: hash }));
  return { js, dependencies, jsPath: resolve(outDir, "producer.mjs") };
}

function readBaseBaseline() {
  const base = eventBase();
  if (typeof base !== "string" || !/^[0-9a-f]{40}$/.test(base)) throw new Error("trusted event base is missing or malformed");
  try {
    git(["cat-file", "-e", `${base}^{commit}`]);
  } catch {
    if (process.env.CI !== "true") throw new Error(`trusted base commit ${base} is not available locally (offline checks do not fetch)`);
    fetchMissingCiBase(base);
    git(["cat-file", "-e", `${base}^{commit}`]);
  }
  const file = "src/app_preview_export/baseline.json";
  const names = git(["ls-tree", "-r", "--name-only", base, "--", file]).split("\n").filter(Boolean);
  if (!names.includes(file)) return null; // Verified tree: first introduction only.
  const text = git(["show", `${base}:${file}`]);
  const baseline = JSON.parse(text);
  if (!baseline || typeof baseline.contract !== "string" || typeof baseline.artifact_sha256 !== "string" ||
      typeof baseline.golden_sha256 !== "string" || !baseline.dependencies || typeof baseline.dependencies !== "object")
    throw new Error("trusted base fixture baseline is malformed");
  return baseline;
}

function goldenInput() {
  return JSON.stringify({
    contract,
    bundle_digest: `sha256:${"e".repeat(64)}`,
    manifest: { app: "fixture-app", title: "Fixture App", version: "1.0.0", summary: null, capabilities: {} },
    inventory: ["app.md", "screens/main/client.js", "screens/main/screens.json"],
    screen: { tag: "main", contract: "app-screens/v2", may: [], tools: {} },
    kit: "cadence-kit/1",
  });
}
function goldenDigest(jsPath) {
  const output = execFileSync(process.execPath, [jsPath], { input: goldenInput(), encoding: "utf8", maxBuffer: 8 * 1024 * 1024 });
  const parsed = JSON.parse(output);
  if (parsed.fixture !== true || parsed.contract !== contract || !Array.isArray(parsed.scenarios) ||
      parsed.scenarios.map(row => row.label).join(",") !== "empty,loading,unavailable,ready,draft,review")
    throw new Error("canonical producer did not emit the complete closed scenario set");
  return sha(output);
}
function contractNumber(value) {
  const match = /^app-screen-projection-fixtures\/(\d+)$/.exec(value);
  if (!match) throw new Error(`unknown numeric fixture contract '${value}'`);
  return Number(match[1]);
}

const tmp = await mkdtemp(resolve(tmpdir(), "cad-app-fixtures-"));
try {
  execFileSync(resolve(ui, "node_modules/.bin/tsrs"), ["--ignoreConfig", "--noEmit", "--target", "ES2022", "--module", "ESNext",
    "--moduleResolution", "bundler", "--lib", "ES2023,DOM,DOM.Iterable", "--strict", "--skipLibCheck",
    "--verbatimModuleSyntax", "scripts/app-fixtures/producer.ts"], { cwd: ui, stdio: "inherit" });
  process.chdir(ui);
  const generated = await buildArtifact(tmp);
  const golden_sha256 = goldenDigest(generated.jsPath);
  const nextBaseline = { contract, artifact_sha256: sha(generated.js), golden_sha256,
    scenarios: ["empty", "loading", "unavailable", "ready", "draft", "review"], dependencies: generated.dependencies };
  const writeMode = process.argv.includes("--write");
  if (writeMode) {
    await writeFile(artifact, generated.js);
    await writeFile(baselinePath, `${JSON.stringify(nextBaseline, null, 2)}\n`);
    console.log(`wrote trusted fixture artifact (${nextBaseline.artifact_sha256}) and canonical golden (${golden_sha256})`);
  } else {
    const [checkedArtifact, checkedBaseline, base] = await Promise.all([
      readFile(artifact), readFile(baselinePath, "utf8").then(JSON.parse), Promise.resolve(readBaseBaseline()),
    ]);
    if (!checkedBaseline || checkedBaseline.contract !== contract) throw new Error("checked-in baseline has an unknown fixture contract");
    if (sha(generated.js) !== sha(checkedArtifact)) fail("checked-in artifact differs from the Vite producer graph");
    if (JSON.stringify(nextBaseline) !== JSON.stringify(checkedBaseline)) fail("baseline does not describe regenerated artifact, golden and complete module graph");
    if (base) {
      const oldVersion = contractNumber(base.contract);
      const newVersion = contractNumber(checkedBaseline.contract);
      if (base.golden_sha256 !== nextBaseline.golden_sha256 && newVersion <= oldVersion)
        fail(`canonical golden changed without a numeric contract bump (trusted base ${base.contract})`);
      if (newVersion < oldVersion) fail("fixture contract version was downgraded");
    }
    if (!process.exitCode) console.log(`fixture producer, canonical golden and drift baseline current (${nextBaseline.artifact_sha256}; ${golden_sha256})`);
  }
} catch (error) {
  fail(error?.stack ?? String(error));
} finally {
  await rm(tmp, { recursive: true, force: true });
}
