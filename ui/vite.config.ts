import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Dev proxies the API to `cadence ui run` on its default loopback port.
// Build emits fixed asset names — index.js/index.css plus the latin
// woff2 files — so the Rust binary can embed them behind `--features ui`.
export default defineConfig({
  plugins: [react(), tailwindcss()],
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
