import { resolve } from "node:path";
import { defineConfig } from "vite";

const root = resolve(import.meta.dirname);
export default defineConfig({
  configFile: false,
  root,
  publicDir: false,
  build: {
    lib: {
      entry: resolve(root, "scripts/app-fixtures/producer.ts"),
      formats: ["es"],
      fileName: () => "producer.mjs",
    },
    target: "node22",
    outDir: resolve(root, "../src/app_preview_export"),
    emptyOutDir: false,
    minify: false,
    rolldownOptions: { output: { codeSplitting: false } },
  },
});
