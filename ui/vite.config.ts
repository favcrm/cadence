import { execSync } from "node:child_process";
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import process from "node:process";
import { brotliCompressSync, constants as zlibConstants, gzipSync } from "node:zlib";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

/**
 * The build id the bundle compares against the server's `hello` frame
 * (CAD-573): `<crate version>+<git sha>`, the same shape the binary's
 * `--version` prints. `CADENCE_BUILD_COMMIT` pins the sha when git is
 * absent; "unknown" reads as cannot-tell — it never prompts a reload.
 */
function uiBuild(): string {
  let version = "0.1.0";
  try {
    const toml = readFileSync(new URL("../Cargo.toml", import.meta.url), "utf8");
    version = toml.match(/^version\s*=\s*"([^"]+)"/m)?.[1] ?? version;
  } catch {
    // No Cargo.toml next to ui/ — the package.json fallback stands.
  }
  let sha = process.env.CADENCE_BUILD_COMMIT || "";
  if (!sha) {
    try {
      sha = execSync("git rev-parse HEAD", {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      }).trim();
    } catch {
      // No repo — build.rs would also have written "unknown".
    }
  }
  return `${version}+${sha || "unknown"}`;
}

// The UI package's own version — the shell side of an app screen's
// `min_shell`. Injected so the bundle never reads package.json at runtime.
function shellVersion(): string {
  try {
    const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")) as { version?: unknown };
    return typeof pkg.version === "string" ? pkg.version : "0.1.0";
  } catch {
    return "0.1.0";
  }
}

// Dev proxies only to the explicitly selected sandbox board.
function devBoardPort(): number {
  const raw = process.env.CADENCE_DEV_BOARD_PORT;
  if (!raw || !/^\d+$/.test(raw)) {
    throw new Error("CADENCE_DEV_BOARD_PORT must be an integer from 3110 to 3199");
  }
  const port = Number(raw);
  if (port < 3110 || port > 3199) {
    throw new Error("CADENCE_DEV_BOARD_PORT must be an integer from 3110 to 3199");
  }
  return port;
}

/**
 * Precompress emitted files for the board's Content-Encoding serving:
 * `index.js` re-downloads on every full load under `Cache-Control:
 * no-store`, so it is the largest single transfer a load pays (1.8MB
 * raw vs ~416KB brotli on the current bundle). Node's built-in zlib
 * emits `<file>.br`/`.gz` siblings in dist; the server answers them
 * when `Accept-Encoding` allows and falls back to the raw file
 * otherwise. Not part of rollup output — `build.rollupOptions` would
 * add them to the manifest and fingerprint them. Compresses in
 * `writeBundle` (after the output is on disk): `closeBundle` runs before
 * rolldown writes in Vite 8, so a first build walked a missing `dist`
 * and failed.
 */
function precompress() {
  let outDir = "dist";
  return {
    name: "cadence-precompress",
    apply: "build" as const,
    configResolved(config: { build: { outDir?: string }; root: string }) {
      outDir = resolve(config.root, config.build.outDir ?? "dist");
    },
    writeBundle() {
      const SKIP = /\.(br|gz|woff2?|png|jpe?g|webp|ico|gif)$/;
      const walk = (dir: string): void => {
        for (const entry of readdirSync(dir)) {
          const file = join(dir, entry);
          const stat = statSync(file);
          if (stat.isDirectory()) {
            walk(file);
            continue;
          }
          // Embedded HTML/JS/CSS require siblings even when an asset is tiny.
          if (SKIP.test(entry)) continue;
          const raw = readFileSync(file);
          writeFileSync(
            `${file}.br`,
            brotliCompressSync(raw, {
              params: { [zlibConstants.BROTLI_PARAM_QUALITY]: 11 },
            }),
          );
          writeFileSync(`${file}.gz`, gzipSync(raw, { level: 9 }));
        }
      };
      walk(outDir);
    },
  };
}

// Build emits fixed asset names — index.js/index.css plus the latin
// woff2 files — so the Rust binary can embed them behind `--features ui`.
export default defineConfig(({ command, isPreview }) => ({
  plugins: [react(), tailwindcss(), precompress()],
  define: {
    __CADENCE_BUILD__: JSON.stringify(uiBuild()),
    __SHELL_VERSION__: JSON.stringify(shellVersion()),
  },
  server:
    command === "serve" && !isPreview
      ? { proxy: { "/api": `http://127.0.0.1:${devBoardPort()}` } }
      : undefined,
  build: {
    assetsInlineLimit: 0,
    rollupOptions: {
      output: {
        entryFileNames: "assets/index.js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: (asset) =>
          asset.names?.some((n) => n.endsWith(".css"))
            ? "assets/index.css"
            : "assets/[name].[ext]",
      },
    },
  },
}));
