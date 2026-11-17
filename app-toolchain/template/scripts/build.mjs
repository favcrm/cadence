import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = process.argv[2] ? path.resolve(process.argv[2]) : path.resolve(here, "..");
const appRoot = path.join(root, "app");
const out = path.join(appRoot, "screens/main");

const candidates = [
  process.env.CADENCE_APP_KIT,
  path.resolve(root, "app-kit"),
  path.resolve(root, "../app-kit"),
  path.resolve(root, "../../ui/app-kit"),
].filter(Boolean);
const appKit = candidates.find((candidate) => existsSync(path.join(candidate, "package.json")));
if (!appKit) {
  console.error(
    "build: vendor the app-kit at app-kit/ (or keep the archive sibling ../app-kit), or set CADENCE_APP_KIT for a direct bun build; cadence app dev scrubs that variable",
  );
  process.exit(1);
}

const manifest = readFileSync(path.join(appRoot, "app.md"), "utf8");
const app = manifest.match(/^app:\s*(\S+)\s*$/m)?.[1];
if (!app) throw new Error("build: app.md has no app id");
const packageConfig = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
const screen = packageConfig.cadence?.screen ?? { host_contract: "screen-actions.v1", may: [] };
if (!screen || typeof screen !== "object" || Array.isArray(screen) ||
    Object.keys(screen).some((key) => !["host_contract", "may", "operations", "tools"].includes(key)))
  throw new Error("build: invalid cadence.screen declaration");

function digest(files, base = root) {
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(path.relative(base, file).split(path.sep).join("/"));
    hash.update("\0");
    hash.update(readFileSync(file));
  }
  return `sha256:${hash.digest("hex")}`;
}

const provenance = {
  source_digest: digest([
    path.join(root, "src/main.js"),
    path.join(root, "src/styles.css"),
    path.join(appRoot, "app.md"),
    ...(existsSync(path.join(appRoot, "workflows")) ? readdirSync(path.join(appRoot, "workflows"))
      .filter((name) => name.endsWith(".md")).sort().map((name) => path.join(appRoot, "workflows", name)) : []),
  ]),
  sdk_digest: digest([
    path.join(appKit, "src/index.mjs"),
    path.join(appKit, "src/receiver.mjs"),
    path.join(appKit, "src/refusal.mjs"),
    path.join(appKit, "src/forms.mjs"),
    path.join(appKit, "src/react.mjs"),
  ], appKit),
  toolchain_digest: digest([path.join(root, "package.json"), fileURLToPath(import.meta.url)]),
};

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

const result = await Bun.build({
  entrypoints: [path.join(root, "src/main.js")],
  outdir: out,
  naming: "client.js",
  format: "iife",
  target: "browser",
  minify: true,
  plugins: [
    {
      name: "app-kit-alias",
      setup(build) {
        build.onResolve({ filter: /^@cadence\/app-kit$/ }, () => ({
          path: path.join(appKit, "src/index.mjs"),
        }));
        build.onResolve({ filter: /^@cadence\/app-kit\/react$/ }, () => ({
          path: path.join(appKit, "src/react.mjs"),
        }));
      },
    },
  ],
});
if (!result.success) {
  for (const log of result.logs) console.error(log);
  process.exit(1);
}

copyFileSync(path.join(root, "src/styles.css"), path.join(out, "styles.css"));

const assets = ["client.js", "styles.css"].map((name) => {
  const file = path.join(out, name);
  const bytes = readFileSync(file);
  return {
    name,
    media_type: name.endsWith(".css") ? "text/css" : "text/javascript",
    sha256: `sha256:${createHash("sha256").update(bytes).digest("hex")}`,
    size: statSync(file).size,
  };
});

writeFileSync(
  path.join(out, "screens.json"),
  `${JSON.stringify(
    { contract: "app-screens/v2", app, entry: "client.js", assets, provenance, ...screen },
    null,
    2,
  )}\n`,
);

console.log(`built ${app}: ${assets.map((asset) => `${asset.name} ${asset.size}B`).join(", ")}`);
