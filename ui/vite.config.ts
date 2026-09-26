import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import process from "node:process";
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

// Dev proxies the API to `cadence ui run` on its default loopback port.
// Build emits fixed asset names — index.js/index.css plus the latin
// woff2 files — so the Rust binary can embed them behind `--features ui`.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: {
    __CADENCE_BUILD__: JSON.stringify(uiBuild()),
  },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:3010",
    },
  },
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
});
