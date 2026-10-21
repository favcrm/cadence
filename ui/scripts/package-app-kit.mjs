#!/usr/bin/env node
// Local-only package for @cadence/app-kit. Never publishes; the caller pins
// the source SHA and the resulting tarball digest.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const args = process.argv.slice(2);
const option = (name) => {
  const at = args.indexOf(name);
  if (at < 0 || !args[at + 1] || args[at + 1].startsWith("--")) throw new Error(`Required: ${name}`);
  return args[at + 1];
};
const source = resolve(option("--source-tree"));
const sourceSha = option("--source-sha");
const out = resolve(option("--out"));
if (!/^[a-f0-9]{40}$/.test(sourceSha)) throw new Error("--source-sha must be a full commit SHA");
const head = execFileSync("git", ["-C", source, "rev-parse", "HEAD"]).toString().trim();
if (head !== sourceSha) throw new Error("checkout HEAD differs from --source-sha");

const pkg = JSON.parse(readFileSync(join(source, "ui/app-kit/package.json"), "utf8"));
const scratch = mkdtempSync(join(tmpdir(), "cadence-app-kit-"));
try {
  cpSync(join(source, "ui/app-kit"), scratch, { recursive: true });
  const file = join(out, `cadence-app-kit-${pkg.version}.tgz`);
  execFileSync("tar", ["-czf", file, "-C", scratch, "."]);
  const digest = createHash("sha256").update(readFileSync(file)).digest("hex");
  process.stdout.write(`${JSON.stringify({ name: pkg.name, version: pkg.version, source_sha: sourceSha, file, sha256: digest })}\n`);
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
