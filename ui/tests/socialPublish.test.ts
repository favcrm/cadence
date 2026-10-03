export {};
/** CAD-787 product client checks. Every fetch the client can make stays on
 *  same-origin `/api/` paths — no external provider is reachable, so no
 *  live post is possible in any test. */
declare function require(name: string): any;
const store = require("../src/features/workspace-apps/socialPublish") as typeof import("../src/features/workspace-apps/socialPublish");
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const calls: { url: string; method: string; body: any }[] = [];
const envelope = (overrides: any = {}) => ({
  intent_id: "intent-a", request: "key-a", state: "queued",
  frozen: {
    install_id: "install-a", context_id: null, run_id: "run-a",
    effect_id: "effect-a", artifact_id: "artifact-a", bundle_digest: "bundle-a",
    slot: "publication", connection_id: "conn-a",
    destination_id: "17841400008460056", toolkit: "instagram",
    caption_digest: "c-digest", image_digest: "i-digest", media_key: null,
    grant_id: "grant-a", approval_id: "op-a",
    due_epoch: 1790601000, timezone: "Asia/Hong_Kong",
  },
  frozen_digest: "binding-digest", receipt: null, upstream: null,
  writer: "writer-a", reviewer: "reviewer-a", ...overrides,
});
let intents: any[] = [envelope()];
(globalThis as any).fetch = async (input: unknown, init?: { method?: string; body?: string }) => {
  const url = String(input);
  calls.push({ url, method: init?.method ?? "GET", body: init?.body ? JSON.parse(init.body) : undefined });
  if (url === "/api/social-publishes?install_id=install-a") return new Response(JSON.stringify({ intents }), { status: 200 });
  if (url === "/api/social-publishes/intent-a") return new Response(JSON.stringify({ intent: intents[0] }), { status: 200 });
  if (url === "/api/social-publishes" && init?.method === "POST") {
    const body = JSON.parse(init.body ?? "{}");
    intents.unshift(envelope({ intent_id: "intent-b", state: "queued", frozen: { ...intents[0].frozen, due_epoch: body.due_epoch } }));
    return new Response(JSON.stringify({ intent: intents[0] }), { status: 200 });
  }
  if (url === "/api/social-publishes/intent-a/cancel" && init?.method === "POST") {
    intents[0] = { ...intents[0], state: "cancelled" };
    return new Response(JSON.stringify({ intent: intents[0] }), { status: 200 });
  }
  if (url === "/api/social-media-imports" && init?.method === "POST")
    return new Response(JSON.stringify({ ok: true, media_key: `dp1.ws.conn.${"ab".repeat(16)}`, image_digest: "ab".repeat(32) }), { status: 200 });
  throw new Error(`Unexpected fetch ${init?.method ?? "GET"} ${url}`);
};
async function main() {
  const listed = await store.socialPublish.list("install-a", null);
  assert(listed.intents.length === 1 && listed.intents[0].destination_id === "17841400008460056" && !Object.hasOwn(listed.intents[0], "destination_handle"), "List echoes the destination id only — the handle is the UI display constant");
  const shown = await store.socialPublish.show("intent-a");
  assert(shown.intent.destination_id === "17841400008460056", "Show pins the pilot account id, never a source handle");
  const scheduled = await store.socialPublish.schedule({
    request_id: "install-a.run-a.1790601000", install_id: "install-a", run_id: "run-a",
    effect_id: "effect-a", artifact_id: "artifact-a", bundle_digest: "bundle-digest",
    slot: "publication", destination_id: "17841400008460056", toolkit: "instagram",
    grant_id: "grant-a", approval_id: "op-a", due_epoch: 1790601000, timezone: "Asia/Hong_Kong",
  });
  assert(scheduled.intent.state === "queued", "Schedule freezes a queued intent");
  const write = calls.find(call => call.url === "/api/social-publishes");
  assert(write?.body.slot === "publication" && write?.body.timezone === "Asia/Hong_Kong"
    && write?.body.destination_id === "17841400008460056"
    && !Object.hasOwn(write?.body ?? {}, "caption_digest"), "Artifact-freeze body carries no caller-invented digest");
  const cancelled = await store.socialPublish.cancel("intent-a", "install-a", "ctx-a");
  const cancelCall = calls.find(call => call.url === "/api/social-publishes/intent-a/cancel");
  assert(cancelCall?.body.install_id === "install-a" && cancelCall?.body.context_id === "ctx-a" && Object.keys(cancelCall.body).length === 2, "Cancel names the intent's install and context scope only");
  assert(cancelled.intent.state === "cancelled", "Cancel closes the intent");
  assert(calls.length > 0 && calls.every(call => call.url.startsWith("/api/") && !call.url.includes("http")), "Every client call stays same-origin — no external provider reachable");
  const states = ["queued", "processing", "posted", "refused", "cancelled", "held"] as const;
  assert(states.every(state => store.publishStateText(state).length > 0), "All six store states have operator text");
  assert(store.publishStateTone("posted") === "ok" && store.publishStateTone("refused") === "fail"
    && store.publishStateTone("held") === "warn"
    && store.publishStateTone("queued") === "muted" && store.publishStateTone("processing") === "muted", "State tones match the approved language");
  assert(store.publishStateText("held").includes("human") && !store.publishStateText("held").includes("reconnect"), "Held names the human, not a reconnect state");
  assert(store.reconcileReading("held")?.includes("human")
    && store.reconcileReading("posted") === null && store.reconcileReading("processing") === null && store.reconcileReading("queued") === null, "Only held carries standing reconcile guidance");
  const mapped = store.toPublishIntent(intents[0]);
  assert(mapped.channel === "instagram" && mapped.idempotency_key === "key-a" && mapped.frozen_digest === "binding-digest"
    && mapped.writer === "writer-a" && mapped.reviewer === "reviewer-a" && mapped.upstream === null, "Envelope maps to the flat view without invention");
  assert(store.toPublishIntent(envelope({ state: "posted", receipt: { permalink: "https://p/", destination_id: "d", caption_digest: "c", provider_ids: [], provider_payload: "{}" } })).permalink === "https://p/", "Permalink comes only from a posted receipt");
  assert(store.toPublishIntent(envelope({ state: "held", receipt: { reason: "dispatch authority differs from frozen approval" } })).refusal?.message === "dispatch authority differs from frozen approval", "Held reason maps raw with no code");
  for (const bad of [
    envelope({ state: "uncertain" }),
    envelope({ frozen: { ...intents[0].frozen, toolkit: "sms" } }),
    envelope({ frozen: { ...intents[0].frozen, due_epoch: "tomorrow" } }),
    envelope({ frozen: { ...intents[0].frozen, caption_digest: null } }),
  ]) {
    let threw = false;
    try { store.toPublishIntent(bad); } catch { threw = true; }
    assert(threw, "Off-shape envelope fails closed instead of rendering");
  }
  const base = mapped;
  assert(store.showsUncertainReading({ ...base, state: "processing", upstream: { state: "processing" } }), "Processing plus upstream evidence shows the uncertain display");
  assert(!store.showsUncertainReading({ ...base, state: "processing", upstream: null }), "Processing without evidence shows no uncertain display");
  assert(!store.showsUncertainReading({ ...base, state: "held", upstream: { state: "processing" } }), "Held never shows the uncertain display");
  assert(!store.showsUncertainReading({ ...base, state: "posted", upstream: { state: "processing" } }), "Terminal states never show the uncertain display");
  const vocabulary = ["bad_key", "bad_connection", "bad_destination", "bad_caption_digest", "bad_image_digest", "bad_run", "bad_effect", "bad_grant", "bad_intent", "bad_revision", "bad_timezone", "cancel_closed", "cross_workspace", "grant_mismatch", "grant_binding_mismatch", "grant_revoked", "grant_exhausted", "grant_window", "grant_approval", "grant_bounds", "image_required", "key_conflict", "not_publishable", "wrong_connection", "wrong_destination", "wrong_toolkit", "unknown_key", "send_disabled"];
  assert(vocabulary.length === 28, "Refusal vocabulary is exactly the 28-code list");
  for (const code of vocabulary) {
    const copy = store.refusalCopy({ code, message: "" }).toLowerCase();
    assert(copy.includes("nothing was published") || copy.includes("nothing was stored") || copy.includes("no provider was called") || copy.includes("fails instead") || copy.includes("nothing to reconcile") || copy.includes("already left queued") || copy.includes("refused"), `Refusal ${code} stays explicit`);
  }
  assert(store.refusalCopy({ code: "send_disabled", message: "" }).includes("not a state"), "send_disabled is a refusal code, not a state");
  assert(store.refusalCopy({ code: "future_code", message: "raw" }).includes("future_code") && store.refusalCopy({ code: "future_code", message: "raw" }).includes("raw"), "Unknown codes stay visible with raw message");
  assert(store.refusalFromError("approval_replay: this approval already authorized another social publish intent").includes("already authorized another post") && store.refusalFromError("bad_approval: approval id must be apv-").includes("not one this panel minted"), "Cadence approval refusals get operator copy");
  assert(store.refusalFromError("plain failure") === "plain failure", "Uncoded errors stay raw");
  assert(store.refusalCopy({ code: "send_disabled", message: "" }).includes("changed nothing"), "Disabled gate names validate-all/mutate-nothing");
  assert(store.canCancel("queued") && !store.canCancel("processing") && !store.canCancel("posted") && !store.canCancel("cancelled"), "Only queued intents cancel (cancel_closed past queued)");
  const label = store.dueLabel(1790601000, "Asia/Hong_Kong");
  assert(label.includes("Asia/Hong_Kong") && label.includes("1790601000"), "Due label always shows timezone and epoch");
  assert(store.parseDueEpoch("2026-09-30T18:30") !== null && store.parseDueEpoch("") === null && store.parseDueEpoch("tomorrow") === null, "Incomplete due values keep Schedule gated");
  assert(store.PILOT_DESTINATION.handle === "@sakeboyhk" && store.PILOT_DESTINATION.account_id === "17841400008460056", "Pilot identity is the operator-named account");
  const importBody = { request_id: "imp-a", install_id: "install-a", run_id: "run-a", artifact_id: "artifact-a", bundle_digest: "bundle-a", slot: "publication" as const, toolkit: "instagram" as const, destination_id: "17841400008460056" };
  const imported = await store.socialPublish.importMedia(importBody);
  assert(imported.image_digest === "ab".repeat(32) && calls.some(call => call.url === "/api/social-media-imports" && call.method === "POST"), "Import posts provenance to the same-origin media door");
  let forgedThrew = false;
  try { await store.socialPublish.importMedia({ ...importBody, bytes_url: "https://evil" } as any); } catch { forgedThrew = true; }
  assert(forgedThrew, "Import never serializes a caller path, URL or byte field");
  const reviewed = `sha256:${"ab".repeat(32)}`;
  assert(store.importMatchesReviewed(imported, reviewed), "A key binding the reviewed digest is usable");
  assert(!store.importMatchesReviewed({ ...imported, image_digest: "cd".repeat(32) }, reviewed), "A different imported digest is refused");
  assert(!store.importMatchesReviewed({ ...imported, media_key: `dp1.ws.conn.${"cd".repeat(16)}` }, reviewed), "A key bound to another digest is refused");
  assert(!store.importMatchesReviewed(imported, "ab".repeat(32)), "A reviewed digest without its sha256: prefix is not trusted");
  assert(store.isGrantIdUsable("dpq_synthetic_grant_01") && !store.isGrantIdUsable("dpq_short") && !store.isGrantIdUsable("grant-a") && !store.isGrantIdUsable(`dpq_${"a".repeat(65)}`), "Grant ids mirror valid_grant_id");
  console.log("social publish client checks passed");
}
void main();
