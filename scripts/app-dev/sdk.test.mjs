import assert from "node:assert/strict";
import { test } from "node:test";
import { createStudioFixture } from "../../app-previews/social-content/sdk.mjs";
import {
  studioFixture,
  fixtureWeek,
} from "../../app-previews/social-content/fixtures.mjs";

test("immutable source selection creates attributed batch items and multiple independent drafts", () => {
  const sdk = createStudioFixture(studioFixture);
  const before = sdk.read().sources;
  const originalCount = sdk
    .read()
    .posts.filter((post) => post.sourceId === "source-3").length;
  const run = sdk.batch(["source-3", "source-4"]);
  assert.equal(run.items.length, 2);
  for (const item of run.items) {
    const post = sdk.read().posts.find((p) => p.id === item.postId);
    const source = before.find((s) => s.id === item.sourceId);
    assert.equal(post.sourceId, source.id);
    assert.equal(post.caption, source.text);
    assert.equal(post.runId, run.id);
    assert.equal(post.status, "draft");
  }
  sdk.batch(["source-3"]);
  assert.equal(
    sdk.read().posts.filter((p) => p.sourceId === "source-3").length,
    originalCount + 2,
  );
  assert.deepEqual(sdk.read().sources, before);
  const copy = sdk.read();
  copy.sources[0].text = "tampered";
  assert.deepEqual(sdk.read().sources, before);
  assert.throws(() => sdk.batch(["forged-source"]), /valid source/);
});

test("material revisions pin local review/approval and edits refuse stale writes and staging", () => {
  const sdk = createStudioFixture(studioFixture);
  const id = sdk.batch(["source-3"]).items[0].postId;
  const get = () => sdk.read().posts.find((p) => p.id === id);
  assert.throws(() => sdk.stage(id, get().revision), /Review and approve/);
  sdk.material(
    id,
    {
      scheduleAt: `${fixtureWeek}T10:00`,
      destinations: ["instagram", "facebook"],
    },
    get().revision,
  );
  sdk.review(id, get().revision);
  sdk.approvePlan(id, get().revision);
  assert.equal(get().status, "scheduled");
  const receipt = sdk.stage(id, get().revision);
  assert.deepEqual(receipt, {
    kind: "simulation",
    revision: get().revision,
    externalReceipt: null,
  });
  assert.throws(() => sdk.stage(id, get().revision), /already staged/);
  const old = get().revision;
  sdk.edit(id, "Human caption", old);
  assert.equal(get().reviewedRevision, null);
  assert.equal(get().approvalRevision, null);
  assert.equal(get().outbox, null);
  assert.match(get().needsYou, /voided/);
  assert.throws(() => sdk.edit(id, "Stale caption", old), /changed while/);
  assert.throws(() => sdk.stage(id, get().revision), /Review and approve/);
  for (const patch of [
    { media: ["new image"] },
    { scheduleAt: `${fixtureWeek}T12:00` },
    { destinations: ["web"] },
  ]) {
    sdk.review(id, get().revision);
    sdk.approvePlan(id, get().revision);
    sdk.stage(id, get().revision);
    sdk.material(id, patch, get().revision);
    assert.equal(get().reviewedRevision, null);
    assert.equal(get().approvalRevision, null);
    assert.equal(get().outbox, null);
  }
  assert.throws(
    () => sdk.material(id, { approvalRevision: 99 }, get().revision),
    /Unsupported/,
  );
});

test("hold and repeated review clear local approval/outbox; lease and undo never restore authority", () => {
  const sdk = createStudioFixture(studioFixture);
  const id = "fixture-1";
  const get = () => sdk.read().posts.find((p) => p.id === id);
  assert.throws(() => sdk.edit(id, "Changed", 1), /Take over/);
  assert.throws(() => sdk.ask(id, "shorter", 1), /Take over/);
  sdk.takeOver(id);
  sdk.review(id, 1);
  sdk.approvePlan(id, 1);
  sdk.stage(id, 1);
  sdk.hold(id);
  assert.equal(get().approvalRevision, null);
  assert.equal(get().outbox, null);
  sdk.approvePlan(id, 1);
  sdk.stage(id, 1);
  sdk.review(id, 1);
  assert.equal(get().approvalRevision, null);
  assert.equal(get().outbox, null);
  sdk.ask(id, "shorter", 1);
  assert.match(get().caption, /Fixture instruction/);
  sdk.undo(id, get().revision);
  assert.equal(get().caption, studioFixture.posts[0].caption);
  assert.equal(get().approvalRevision, null);
  assert.equal(get().outbox, null);
});

test("day agenda preserves dates, chronological card order and crowded-day records", async () => {
  const { postsForDay } = await import(
    "../../app-previews/social-content/calendar.mjs"
  );
  const posts = [
    { id: "late", scheduleAt: "2026-12-31T18:45" },
    { id: "early", scheduleAt: "2026-12-31T09:30" },
    { id: "same", scheduleAt: "2026-12-31T18:45" },
    { id: "next", scheduleAt: "2027-01-01T09:30" },
  ];
  assert.deepEqual(
    postsForDay(posts, "2026-12-31").map((post) => post.id),
    ["early", "late", "same"],
  );
  assert.equal(postsForDay(posts, "2027-01-01")[0].id, "next");
  assert.deepEqual(postsForDay(posts, "2027-01-02"), []);
  assert.equal(posts.length, 4);
});
