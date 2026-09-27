import assert from "node:assert/strict";
import { test } from "node:test";
import { moduleNameCollisions } from "./check-module-names.mjs";

test("rejects all four beta.1 helper/component collisions", () => {
  const pairs = [
    ["features/apps/Apps.tsx", "features/apps/apps.ts"],
    ["features/settings/PlatformAccount.tsx", "features/settings/platformAccount.ts"],
    ["features/projects/Workflows.tsx", "features/projects/workflows.ts"],
    ["features/home/AgentUpdates.tsx", "features/home/agentUpdates.ts"],
  ];
  assert.deepEqual(moduleNameCollisions(pairs.flat()), pairs);
});

test("allows descriptive helpers and separate directories, excludes assets", () => {
  assert.deepEqual(moduleNameCollisions([
    "apps/Apps.tsx", "apps/appViewModel.ts", "other/Apps.tsx", "apps/Apps.css",
  ]), []);
});

test("rejects identical stems and case-insensitive directory aliases", () => {
  const pairs = [["home/View.ts", "home/View.tsx"], ["Apps/data.js", "apps/Data.jsx"]];
  assert.deepEqual(moduleNameCollisions(pairs.flat()), pairs);
});
