import test from "node:test";
import assert from "node:assert/strict";
import { parseFrontmatter, validateBundle } from "../src/index.mjs";

const manifest = `---
app: demo
title: Demo
version: '0.1.0'
summary: One demo app.
needs:
  connections: []
listing:
  icon: assets/icon.svg
  screenshots:
    - assets/a.png
---

# Demo
`;

function valid() {
  return new Map([
    ["app/app.md", manifest],
    ["app/assets/icon.svg", "<svg/>"],
    ["app/assets/a.png", ""],
    [
      "app/views/views.json",
      JSON.stringify({
        contract: "app-views/v1",
        app: "demo",
        views: [{ id: "main", title: "Main", kind: "table" }],
      }),
    ],
    ["app/workflows/do.md", "---\ntitle: Do\ngoal: one thing\n---\n\nDo it.\n"],
    ["app/rubrics/review.md", "# Review\n"],
  ]);
}

const codes = (files) => validateBundle(files).errors.map((error) => error.code);

test("a valid bundle passes", () => {
  assert.deepEqual(validateBundle(valid()), { ok: true, errors: [] });
});

test("frontmatter reads top-level values and the listing block", () => {
  const front = parseFrontmatter(manifest);
  assert.equal(front.values.app, "demo");
  assert.equal(front.values.version, "0.1.0");
  assert.equal(front.listing.icon, "assets/icon.svg");
  assert.deepEqual(front.listing.screenshots, ["assets/a.png"]);
});

test("a missing manifest is its own code", () => {
  const files = valid();
  files.delete("app/app.md");
  assert.deepEqual(codes(files), ["bundle/app-md-missing"]);
});

test("a workflow is required", () => {
  const files = valid();
  files.delete("app/workflows/do.md");
  assert.ok(codes(files).includes("bundle/workflow-missing"));
});

test("a foreign views contract is refused", () => {
  const files = valid();
  files.set("app/views/views.json", JSON.stringify({ contract: "app-views/v2", app: "demo", views: [] }));
  assert.deepEqual(codes(files), ["views/contract", "views/empty"]);
});

test("duplicate view ids are refused", () => {
  const files = valid();
  files.set(
    "app/views/views.json",
    JSON.stringify({
      contract: "app-views/v1",
      app: "demo",
      views: [
        { id: "main", title: "Main", kind: "table" },
        { id: "main", title: "Other", kind: "table" },
      ],
    }),
  );
  assert.deepEqual(codes(files), ["views/duplicate-id"]);
});

test("a listing asset that is missing is refused", () => {
  const files = valid();
  files.delete("app/assets/icon.svg");
  assert.deepEqual(codes(files), ["bundle/asset-missing"]);
});

test("a workflow without a goal is refused", () => {
  const files = valid();
  files.set("app/workflows/do.md", "---\ntitle: Do\n---\n\nDo it.\n");
  assert.deepEqual(codes(files), ["workflow/field-missing"]);
});

test("a bad version and app slug are refused", () => {
  const files = valid();
  files.set("app/app.md", manifest.replace("version: '0.1.0'", "version: v1").replace("app: demo", "app: Demo"));
  assert.deepEqual(codes(files).sort(), ["bundle/app-invalid", "bundle/version-invalid", "views/app-mismatch"]);
});

test("views must name the manifest app", () => {
  const files = valid();
  files.set(
    "app/views/views.json",
    JSON.stringify({
      contract: "app-views/v1",
      app: "other",
      views: [{ id: "main", title: "Main", kind: "table" }],
    }),
  );
  assert.deepEqual(codes(files), ["views/app-mismatch"]);
});
