#!/usr/bin/env node
// Local-only build. Run under supported build-slot admission; never publishes.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdirSync, mkdtempSync, rmSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve, join, relative } from "node:path";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { gzipSync } from "node:zlib";

const args = process.argv.slice(2);
const working = args.includes("--working-tree");
function option(name) {
  const at = args.indexOf(name);
  if (at < 0 || !args[at + 1] || args[at + 1].startsWith("--")) throw new Error(`Required: ${name}`);
  return args[at + 1];
}
const source = resolve(option("--source-tree"));
const sourceSha = option("--source-sha");
if (!/^[a-f0-9]{40}$/.test(sourceSha)) throw new Error("--source-sha must be a full commit SHA");
const ui = join(source, "ui");
const toolchain = resolve(option("--toolchain-root"));
const requireTool = createRequire(join(toolchain, "package.json"));
const hostPackage = JSON.parse(readFileSync(join(ui, "package.json"), "utf8"));
const kitPackage = JSON.parse(readFileSync(join(ui, "screen-ui/package.json"), "utf8"));
const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const git = (...argv) => execFileSync("git", ["-C", source, ...argv]);
git("cat-file", "-e", `${sourceSha}^{commit}`);
if (git("rev-parse", "HEAD").toString().trim() !== sourceSha) throw new Error("Checkout HEAD differs from --source-sha");

const names = ["react", "react-dom", "@types/react", "@types/react-dom", "@hugeicons/react", "@hugeicons/core-free-icons", "@typescript/typescript6", "vite", "@vitejs/plugin-react", "tailwindcss", "@tailwindcss/vite"];
const versions = {};
for (const name of names) {
  const expected = hostPackage.dependencies?.[name] ?? hostPackage.devDependencies?.[name];
  const installed = JSON.parse(readFileSync(join(toolchain, "node_modules", name, "package.json"), "utf8")).version;
  if (installed !== expected) throw new Error(`${name}: installed ${installed}, source pins ${expected}`);
  versions[name] = installed;
}
for (const [name, version] of Object.entries({ ...kitPackage.dependencies, ...kitPackage.peerDependencies })) {
  if (versions[name] !== version) throw new Error(`Kit dependency differs from host pin: ${name}`);
}
const bun = execFileSync("bun", ["--version"], { cwd: ui }).toString().trim();
if (hostPackage.packageManager !== `bun@${bun}`) throw new Error("bun differs from source pin");
// TypeScript 7 ships no JS compiler API; @typescript/typescript6 is its official API companion.
const ts = requireTool("@typescript/typescript6");
const { build } = await import(pathToFileURL(requireTool.resolve("vite")));
const { default: react } = await import(pathToFileURL(requireTool.resolve("@vitejs/plugin-react")));
const { default: tailwind } = await import(pathToFileURL(requireTool.resolve("@tailwindcss/vite")));

// Closed source allowlist, including type-only edges. New imports require audit here.
const allowed = new Set([
  "screen-ui/src/index.ts", "screen-ui/src/navigation.tsx",
  ...["DataTable", "Field", "Detail", "DrawerShell", "States", "ConfirmDialog"].map((name) => `src/features/app-shell/shared/${name}.tsx`),
  "src/features/app-shell/shared/useDrawerClose.ts", "src/features/app-shell/shared/hostErrors.ts",
  ...["Button", "Link", "Select", "icons"].map((name) => `src/ui/${name}.tsx`),
  "src/ui/buttonActivate.ts", "src/ui/selectLogic.ts",
  "src/lib/apiError.ts", "src/lib/types.ts", "src/lib/cacheCore.ts", "src/lib/useResource.ts",
  "src/lib/localeText.tsx",
]);
const external = (id) => /^(react(?:\/.*)?|react-dom(?:\/.*)?|@hugeicons\/react|@hugeicons\/core-free-icons)$/.test(id);
const options = {
  target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler, jsx: ts.JsxEmit.ReactJSX,
  lib: ["lib.es2023.d.ts", "lib.dom.d.ts", "lib.dom.iterable.d.ts"],
  strict: true, skipLibCheck: true, noUnusedLocals: true, noUnusedParameters: true,
  isolatedModules: true, verbatimModuleSyntax: true, useDefineForClassFields: true,
  typeRoots: [join(toolchain, "node_modules/@types")],
};
function compilerHost(compilerOptions) {
  const host = ts.createCompilerHost(compilerOptions);
  host.resolveModuleNames = (imports, file) => imports.map((name) =>
    ts.resolveModuleName(name, file, compilerOptions, ts.sys).resolvedModule ??
    (!name.startsWith(".") ? ts.resolveModuleName(name, join(toolchain, "src/__screen_ui__.tsx"), compilerOptions, ts.sys).resolvedModule : undefined));
  return host;
}
const entry = join(ui, "screen-ui/src/index.ts");
const seen = new Set();
function visit(file) {
  const path = relative(ui, file).replaceAll("\\", "/");
  if (!allowed.has(path)) throw new Error(`Authority-free entry escaped allowlist: ${path}`);
  if (seen.has(path)) return;
  seen.add(path);
  const parsed = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
  function walk(node) {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier) {
      const name = node.moduleSpecifier.text;
      if (name.startsWith(".")) {
        const found = ts.resolveModuleName(name, file, options, ts.sys).resolvedModule;
        if (!found) throw new Error(`Unresolved closure import: ${name} in ${path}`);
        visit(found.resolvedFileName);
      } else if (!external(name)) throw new Error(`Unadmitted external: ${name}`);
    }
    if (ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword || node.expression.getText(parsed) === "require")) {
      throw new Error(`Dynamic closure import is not admitted: ${path}`);
    }
    ts.forEachChild(node, walk);
  }
  walk(parsed);
}
visit(entry);
const cssNames = ["styles", "theme", "controls", "base", "foundation", "primitives", "drawer-base", "drawer"].map((name) => `screen-ui/src/${name}.css`);
const sourceNames = [...seen, ...cssNames, "screen-ui/package.json", "screen-ui/DESIGN.md", "scripts/package-screen-ui.mjs", "package.json", "bun.lock"].sort();
const sourceFiles = sourceNames.map((name) => ({ path: `ui/${name}`, sha256: sha256(readFileSync(join(ui, name))) }));
if (!working) {
  for (const file of sourceFiles) {
    if (sha256(git("show", `${sourceSha}:${file.path}`)) !== file.sha256) throw new Error(`Uncommitted source: ${file.path}; --working-tree is development-only`);
  }
}
const closureHash = sha256(JSON.stringify(sourceFiles));
const provenance = {
  package: `${kitPackage.name}@${kitPackage.version}`,
  closureSha256: closureHash, files: sourceFiles,
  toolchain: { node: process.version, bun, packages: versions },
};
const temp = mkdtempSync(join(tmpdir(), "screen-ui-"));
try {
  const pack = join(temp, "package");
  mkdirSync(pack);
  const declarationOptions = { ...options, declaration: true, emitDeclarationOnly: true, rootDir: ui, outDir: join(pack, "types") };
  const program = ts.createProgram([entry], declarationOptions, compilerHost(declarationOptions));
  const diagnostics = ts.getPreEmitDiagnostics(program);
  if (diagnostics.length) {
    throw new Error(ts.formatDiagnosticsWithColorAndContext(diagnostics, { getCurrentDirectory: () => ui, getCanonicalFileName: (f) => f, getNewLine: () => "\n" }));
  }
  const emitted = program.emit();
  if (emitted.emitSkipped || emitted.diagnostics.length) throw new Error("Declaration emission failed");
  console.log(`Kit typecheck: passed; audited ${seen.size} source modules`);
  // Check the current host against the same pinned dependency directory, without installing.
  const config = ts.readConfigFile(join(ui, "tsconfig.json"), ts.sys.readFile);
  if (config.error) throw new Error("Host tsconfig unreadable");
  const hostConfig = ts.parseJsonConfigFileContent(config.config, ts.sys, ui, { typeRoots: options.typeRoots, noEmit: true, types: [join(toolchain, "node_modules/vite/client.d.ts")] });
  const hostProgram = ts.createProgram(hostConfig.fileNames, hostConfig.options, compilerHost(hostConfig.options));
  const hostDiagnostics = ts.getPreEmitDiagnostics(hostProgram);
  if (hostDiagnostics.length) throw new Error(ts.formatDiagnosticsWithColorAndContext(hostDiagnostics, { getCurrentDirectory: () => ui, getCanonicalFileName: (f) => f, getNewLine: () => "\n" }));
  console.log("Host typecheck: passed");
  await build({
    configFile: false, root: ui, publicDir: false,
    plugins: [react(), tailwind(), {
      name: "screen-ui-entry-and-boundary",
      transform(code, id) {
        if (id === entry) return { code: `${code}\nimport ${JSON.stringify(join(ui, "screen-ui/src/styles.css"))};`, map: null };
      },
      buildEnd(error) {
        if (error) return;
        for (const id of this.getModuleIds()) {
          if (id.startsWith(`${ui}/src/`) && !allowed.has(relative(ui, id))) throw new Error(`Bundled privileged source: ${id}`);
        }
      },
    }],
    resolve: { alias: { tailwindcss: join(toolchain, "node_modules/tailwindcss/index.css") } },
    build: {
      outDir: pack, emptyOutDir: false, sourcemap: false, minify: false,
      lib: { entry, formats: ["es"], fileName: () => "index.js", cssFileName: "styles" },
      rollupOptions: { external },
    },
  });
  writeFileSync(join(pack, "package.json"), `${JSON.stringify(kitPackage, null, 2)}\n`);
  writeFileSync(join(pack, "DESIGN.md"), readFileSync(join(ui, "screen-ui/DESIGN.md")));
  // Archive identity is content/toolchain based, independent of a later squash SHA.
  writeFileSync(join(pack, "build-manifest.json"), `${JSON.stringify(provenance, null, 2)}\n`);
  const tar = execFileSync("tar", ["--sort=name", "--format=ustar", "--mtime=@0", "--owner=0", "--group=0", "--numeric-owner", "--mode=u=rwX,go=rX", "-cf", "-", "-C", temp, "package"], { maxBuffer: 32 * 1024 * 1024 });
  const archive = gzipSync(tar, { level: 9 });
  const output = join(ui, "dist/screen-ui");
  mkdirSync(output, { recursive: true });
  const filename = `cadence-screen-ui-${kitPackage.version}.tgz`;
  writeFileSync(join(output, filename), archive);
  const files = [];
  function inventory(dir) {
    for (const item of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name, "en"))) {
      const path = join(dir, item.name);
      if (item.isDirectory()) inventory(path);
      else files.push({ path: relative(pack, path), sha256: sha256(readFileSync(path)) });
    }
  }
  inventory(pack);
  const receipt = {
    ...provenance,
    source: working ? { kind: "working-tree", sourceSha: null, referenceSha: sourceSha } : { kind: "commit", sourceSha },
    archive: { filename, sha256: sha256(archive), bytes: archive.length }, artifacts: files,
  };
  writeFileSync(join(output, "archive-lock.json"), `${JSON.stringify(receipt, null, 2)}\n`);
  console.log(JSON.stringify({ source: receipt.source, closureSha256: closureHash, archive: receipt.archive, output }, null, 2));
} finally {
  rmSync(temp, { recursive: true, force: true });
}
