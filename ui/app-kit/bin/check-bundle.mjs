#!/usr/bin/env node
// cadence-app-check <dir>
// Structural bundle check for one app repository. Exit 1 lists every finding.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { validateBundle } from "../src/index.mjs";

const dir = process.argv[2] ?? ".";
const text = new Set([".md", ".json", ".svg", ".txt", ".html", ".css", ".js", ".mjs", ".ts", ".tsx"]);
const files = new Map();

function walk(path) {
  for (const name of readdirSync(path)) {
    if (name === "node_modules" || name === ".git") continue;
    const full = join(path, name);
    const stat = statSync(full);
    if (stat.isDirectory()) {
      walk(full);
      continue;
    }
    if (!stat.isFile()) continue;
    const key = relative(dir, full).split(sep).join("/");
    const ext = name.slice(name.lastIndexOf("."));
    files.set(key, text.has(ext) ? readFileSync(full, "utf8") : "");
  }
}

walk(dir);
const result = validateBundle(files);
process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
process.exit(result.ok ? 0 : 1);
