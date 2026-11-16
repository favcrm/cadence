#!/usr/bin/env node
/** Generate license notices for the locked Wasm dependency graph. */
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const metadata = JSON.parse(
  execFileSync(
    "cargo",
    [
      "metadata",
      "--manifest-path",
      join(root, "Cargo.toml"),
      "--locked",
      "--format-version",
      "1",
      "--filter-platform",
      "wasm32-unknown-unknown",
    ],
    { maxBuffer: 100 * 1024 * 1024 },
  ),
);
const image = metadata.packages.find((item) => item.name === "cadence-image" && !item.source);
if (!image || !metadata.resolve) throw new Error("the locked image dependency graph is unavailable");
const packages = new Map(metadata.packages.map((item) => [item.id, item]));
const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
const seen = new Set();
const pending = [image.id];
while (pending.length) {
  const id = pending.pop();
  if (seen.has(id)) continue;
  seen.add(id);
  const node = nodes.get(id);
  if (!node) throw new Error(`missing dependency metadata: ${id}`);
  for (const dependency of node.deps) {
    if (dependency.dep_kinds.some((kind) => kind.kind !== "dev")) pending.push(dependency.pkg);
  }
}
const external = [...seen]
  .map((id) => packages.get(id))
  .filter((item) => item?.source)
  .sort((a, b) => a.name.localeCompare(b.name, "en") || a.version.localeCompare(b.version, "en"));
const sections = [
  "Third-party software in cadence-image.wasm",
  "========================================",
  "",
  "Generated from the locked wasm32 dependency graph. Each package entry",
  "includes its declared license and the license/notice files it ships.",
  "",
];
for (const item of external) {
  const dir = dirname(item.manifest_path);
  const names = readdirSync(dir)
    .filter((name) => /^(?:license|copying|notice)(?:[._-].*)?$/i.test(name))
    .filter((name) => statSync(join(dir, name)).isFile())
    .sort();
  if (item.license_file && !names.includes(item.license_file)) names.push(item.license_file);
  if (!item.license || names.length === 0)
    throw new Error(`missing license declaration or text for ${item.name}@${item.version}`);
  sections.push("--------------------------------------------------------------------------");
  sections.push(`${item.name}@${item.version} (${item.license})`);
  for (const name of names) {
    sections.push(`\n${name}\n`);
    sections.push(readFileSync(join(dir, name), "utf8").trimEnd());
  }
  sections.push("");
}
process.stdout.write(`${sections.join("\n")}\n`);
