#!/usr/bin/env bun
/**
 * Build the vendorable artifact from source.
 *
 * Output (all committed, so a consumer pins one source revision and one hash
 * per file):
 *
 *   dist/runtime.mjs             one ESM bundle; platform modules stay external
 *   dist/*.d.ts                  the public declarations, self-contained
 *   dist/package.json            the vendored package's own metadata
 *   dist/LICENSE, dist/NOTICE    provenance of the derived code
 *   dist/THIRD_PARTY_NOTICES.txt bundled dependencies and their licenses
 *   dist/sha256.json             sha256 of every other file in dist/
 *
 * The engine SDK is bundled. No SDK package or type reaches the consumer.
 *
 * Run after `tsc -p tsconfig.build.json` (which writes the declarations).
 */
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distDir = join(packageDir, "dist");
const nodeModules = join(packageDir, "node_modules");
const manifest = JSON.parse(
  readFileSync(join(packageDir, "package.json"), "utf8"),
);
const metafiles = [
  join(distDir, ".metafile-runtime.json"),
  join(distDir, ".metafile-testing.json"),
];

// 1. ESM bundles. A Worker provides cloudflare:* modules; node:* stays an
// import the consumer's compatibility flag satisfies. Everything else is in.
// `runtime.mjs` carries the engine; `testing.mjs` is the neutral test seam.
for (const { source, outfile, metafile } of [
  { source: "src/index.ts", outfile: "runtime.mjs", metafile: metafiles[0] },
  { source: "src/testing.ts", outfile: "testing.mjs", metafile: metafiles[1] },
]) {
  const args = [
    "build",
    source,
    `--outfile=${relative(packageDir, join(distDir, outfile))}`,
    "--format=esm",
    "--target=browser",
    "--external",
    "cloudflare:*",
    "--external",
    "node:*",
  ];
  if (metafile) args.push(`--metafile=${relative(packageDir, metafile)}`);
  const bundle = spawnSync(process.execPath, args, {
    cwd: packageDir,
    encoding: "utf8",
  });
  if (bundle.status !== 0) {
    throw new Error(
      `bun build ${source} failed:\n${bundle.stderr || bundle.stdout}`,
    );
  }
}

// 2. Declarations: point relative specifiers at the emitted files. TypeScript
// keeps the source's `.ts` spellings; a `.js` specifier resolves to the
// matching `.d.ts` in every consumer resolution mode.
for (const name of readdirSync(distDir).filter((entry) =>
  entry.endsWith(".d.ts"),
)) {
  const path = join(distDir, name);
  const rewritten = readFileSync(path, "utf8")
    .replace(/(from\s+["'])(\.[^"']*)\.ts(["'])/g, "$1$2.js$3")
    .replace(/(import\(\s*["'])(\.[^"']*)\.ts(["'])/g, "$1$2.js$3");
  writeFileSync(path, rewritten);
}

// 3. The public declarations must resolve without the engine SDK: an
// application vendors this package and keeps its own dependency graph.
const publicDeclarations = [
  "index.d.ts",
  "hosted-session.d.ts",
  "contract.d.ts",
  "master.d.ts",
  "testing.d.ts",
  "tool-policy.d.ts",
];
for (const name of publicDeclarations) {
  const text = readFileSync(join(distDir, name), "utf8");
  const leaked = text.match(
    /from\s+["'](?:agents|@earendil-works)\/[^"']*["']/,
  );
  if (leaked)
    throw new Error(`${name} exposes an engine SDK type: ${leaked[0]}`);
}
// Private implementation declarations are not part of either public entry.
for (const name of readdirSync(distDir).filter((entry) =>
  entry.endsWith(".d.ts"),
)) {
  if (!publicDeclarations.includes(name)) rmSync(join(distDir, name));
}

// 4. The vendored package's own metadata and provenance.
copyFileSync(join(packageDir, "LICENSE"), join(distDir, "LICENSE"));
copyFileSync(join(packageDir, "NOTICE"), join(distDir, "NOTICE"));
writeFileSync(
  join(distDir, "package.json"),
  `${JSON.stringify(
    {
      name: manifest.name,
      version: manifest.version,
      description: manifest.description,
      license: manifest.license,
      type: "module",
      main: "./runtime.mjs",
      types: "./index.d.ts",
      exports: {
        ".": { types: "./index.d.ts", import: "./runtime.mjs" },
        "./testing": { types: "./testing.d.ts", import: "./testing.mjs" },
      },
    },
    null,
    2,
  )}\n`,
);

// 5. Third-party notices, from the packages the bundles actually contain.
const inputs = new Set();
for (const metafile of metafiles) {
  const meta = JSON.parse(readFileSync(metafile, "utf8"));
  for (const input of Object.keys(meta.inputs ?? {})) inputs.add(input);
}
const owners = new Map();
for (const input of inputs) {
  let dir = dirname(resolve(packageDir, input));
  while (dir.startsWith(nodeModules) && dir !== nodeModules) {
    const candidate = join(dir, "package.json");
    if (existsSync(candidate)) {
      const owner = JSON.parse(readFileSync(candidate, "utf8"));
      if (owner.name)
        owners.set(`${owner.name}@${owner.version ?? "0"}`, { dir, owner });
      break;
    }
    dir = dirname(dir);
  }
}
const licenseFiles = (dir) =>
  readdirSync(dir)
    .sort()
    .filter((name) => /^(licen[cs]e|copying|copyright|notice)/i.test(name))
    .filter((name) => statSync(join(dir, name)).isFile())
    .map((name) => `${name}\n${readFileSync(join(dir, name), "utf8").trim()}`)
    .join("\n\n");
const declaredLicense = (owner) =>
  typeof owner.license === "string"
    ? owner.license
    : (owner.license?.type ?? "unspecified");
// These published packages omit license files. Preserve their pinned upstream
// texts offline; NOTICE records the exact sources and declaration discrepancy.
const omittedLicenses = new Map(
  ["chord", "pi-ai", "pi-durable", "pi-telemetry"].map((name) => [
    `@earendil-works/${name}@1.4.0`,
    "pi-1.4.0.LICENSE",
  ]),
);
omittedLicenses.set("standardwebhooks@1.1.1", "standardwebhooks-1.1.1.LICENSE");
const sections = [...owners.entries()]
  .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
  .map(([id, { dir, owner }]) => {
    const saved = omittedLicenses.get(id);
    const text =
      licenseFiles(dir) ||
      (saved
        ? readFileSync(join(packageDir, "licenses", saved), "utf8").trim()
        : "");
    if (!text)
      throw new Error(
        `${id} has no preserved license text; review it before bundling`,
      );
    return [
      "--------------------------------------------------------------------------",
      `${id} (${declaredLicense(owner)})`,
      "--------------------------------------------------------------------------",
      ...(id === "standardwebhooks@1.1.1"
        ? [
            "npm declares MIT; its published source repository supplies Apache-2.0 text below.",
          ]
        : []),
      text,
    ].join("\n");
  });
writeFileSync(
  join(distDir, "THIRD_PARTY_NOTICES.txt"),
  [
    "Third-party software bundled in runtime.mjs and testing.mjs",
    "===========================================================",
    "",
    "The Cadence hosted runtime bundles include the packages below. Each entry",
    "names the package, its declared license and the license text it ships.",
    "",
    ...sections,
    "",
  ].join("\n"),
);
for (const metafile of metafiles) rmSync(metafile, { force: true });

// 6. One hash per artifact file, so a consumer can pin what it vendors.
const files = [];
const walk = (dir) => {
  for (const entry of readdirSync(dir).sort()) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) walk(path);
    else if (entry !== "sha256.json" && !entry.startsWith(".metafile"))
      files.push(path);
  }
};
walk(distDir);
const digests = Object.fromEntries(
  files
    .map((path) => [
      relative(distDir, path),
      createHash("sha256").update(readFileSync(path)).digest("hex"),
    ])
    .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0)),
);
writeFileSync(
  join(distDir, "sha256.json"),
  `${JSON.stringify(digests, null, 2)}\n`,
);
