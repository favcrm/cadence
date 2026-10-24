import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const screens = JSON.parse(readFileSync(path.join(root, "app/screens/main/screens.json"), "utf8"));
const manifest = readFileSync(path.join(root, "app/app.md"), "utf8");
const app = manifest.match(/^app:\s*(\S+)\s*$/m)?.[1];

test("the screen package matches the app manifest", () => {
  assert.equal(screens.contract, "app-screens/v2");
  assert.equal(screens.app, app);
  assert.equal(screens.entry, "client.js");
  assert.equal(screens.host_contract, "screen-actions.v1");
  for (const key of ["source_digest", "sdk_digest", "toolchain_digest"]) {
    assert.match(screens.provenance[key], /^sha256:[0-9a-f]{64}$/, `provenance.${key}`);
  }
});

test("the declared screen assets match the built bytes", () => {
  for (const asset of screens.assets) {
    const file = path.join(root, "app/screens/main", asset.name);
    const bytes = readFileSync(file);
    assert.equal(asset.size, statSync(file).size, `${asset.name} size`);
    assert.equal(asset.sha256, `sha256:${createHash("sha256").update(bytes).digest("hex")}`, `${asset.name} digest`);
  }
});
