import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "..");
const appRoot = path.join(root, "app");
const out = path.join(appRoot, "screens/main");

const candidates = [
  process.env.CADENCE_APP_KIT,
  path.resolve(root, "../app-kit"),
  path.resolve(root, "../../ui/app-kit"),
].filter(Boolean);
const appKit = candidates.find((candidate) => existsSync(path.join(candidate, "package.json")));
if (!appKit) {
  console.error("build: set CADENCE_APP_KIT to the app-kit folder, or place it next to the template (../app-kit)");
  process.exit(1);
}

const manifest = readFileSync(path.join(appRoot, "app.md"), "utf8");
const app = manifest.match(/^app:\s*(\S+)\s*$/m)?.[1];
if (!app) throw new Error("build: app.md has no app id");

function digest(files) {
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(path.relative(root, file));
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
    path.join(appRoot, "workflows/hello.md"),
  ]),
  sdk_digest: digest([
    path.join(appKit, "src/index.mjs"),
    path.join(appKit, "src/receiver.mjs"),
    path.join(appKit, "src/refusal.mjs"),
  ]),
  toolchain_digest: digest([path.join(root, "package.json"), path.join(root, "scripts/build.mjs")]),
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
    { contract: "app-screens/v2", app, entry: "client.js", assets, provenance, host_contract: "screen-actions.v1", may: [] },
    null,
    2,
  )}\n`,
);

console.log(`built ${app}: ${assets.map((asset) => `${asset.name} ${asset.size}B`).join(", ")}`);
