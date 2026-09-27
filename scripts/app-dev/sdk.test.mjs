import assert from "node:assert/strict";
import { test } from "node:test";
import { createFixtureSdk } from "../../app-previews/social-content/sdk.mjs";
test("create, edit, review and local outbox retain revision and never invent a receipt", () => {
  const sdk = createFixtureSdk();
  const p = sdk.create("Source");
  assert.throws(() => sdk.stage(p.id));
  sdk.review(p.id);
  sdk.edit(p.id, "Edited");
  assert.throws(() => sdk.stage(p.id));
  sdk.review(p.id);
  assert.deepEqual(sdk.stage(p.id), {
    kind: "simulation",
    revision: 2,
    externalReceipt: null,
  });
  assert.equal(sdk.read()[0].status, "local-outbox");
  assert.throws(() => sdk.stage(p.id));
});
test("fixture state is isolated, empty input refused and callers cannot mutate snapshots", () => {
  const a = createFixtureSdk(),
    b = createFixtureSdk();
  assert.throws(() => a.create(" "));
  const p = a.create("Source");
  a.read()[0].caption = "Forged";
  assert.equal(a.read()[0].caption, "Source");
  assert.equal(b.read().length, 0);
  assert.throws(() => a.edit(p.id, ""));
});
test("non-contiguous fixture IDs cannot collide", () => {
  const sdk = createFixtureSdk([{ id: "fixture-2", caption: "Existing" }]);
  const created = sdk.create("New");
  assert.notEqual(created.id, "fixture-2");
  assert.equal(new Set(sdk.read().map((p) => p.id)).size, 2);
});
