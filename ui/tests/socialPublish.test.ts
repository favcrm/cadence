export {};
/** CAD-787 product client checks. Every fetch the client can make stays on
 *  same-origin `/api/` paths — no external provider is reachable, so no
 *  live post is possible in any test. */
declare function require(name: string): any;
const store = require("../src/features/workspace-apps/socialPublish") as typeof import("../src/features/workspace-apps/socialPublish");
function assert(value: unknown, why: string): asserts value { if (!value) throw new Error(why); }
const calls: { url: string; method: string; body: any }[] = [];
const intents: any[] = [{
  intent_id: "intent-a", install_id: "install-a", context_id: null, run_id: "run-a",
  effect_id: "effect-a", state: "queued", channel: "instagram",
  destination_id: "17841400008460056", destination_handle: "@sakeboyhk",
  caption_digest: "c-digest", image_digest: "i-digest", destination_digest: "d-digest",
  idempotency_key: "key-a", due_epoch: 1790601000, timezone: "Asia/Hong_Kong",
  grant_id: "grant-a", approval_id: "op-a", writer: "writer-a", reviewer: "reviewer-a",
  permalink: null, receipt: null, refusal: null,
}];
(globalThis as any).fetch = async (input: unknown, init?: { method?: string; body?: string }) => {
  const url = String(input);
  calls.push({ url, method: init?.method ?? "GET", body: init?.body ? JSON.parse(init.body) : undefined });
  if (url === "/api/social-publishes?install_id=install-a") return new Response(JSON.stringify({ intents }), { status: 200 });
  if (url === "/api/social-publishes/intent-a") return new Response(JSON.stringify({ intent: intents[0] }), { status: 200 });
  if (url === "/api/social-publishes" && init?.method === "POST") {
    const body = JSON.parse(init.body ?? "{}");
    intents.unshift({ ...intents[0], intent_id: "intent-b", state: "queued", due_epoch: body.due_epoch });
    return new Response(JSON.stringify({ intent: intents[0] }), { status: 200 });
  }
  if (url === "/api/social-publishes/intent-a/cancel" && init?.method === "POST") {
    intents[0] = { ...intents[0], state: "cancelled" };
    return new Response(JSON.stringify({ intent: intents[0] }), { status: 200 });
  }
  throw new Error(`Unexpected fetch ${init?.method ?? "GET"} ${url}`);
};
async function main() {
  const listed = await store.socialPublish.list("install-a", null);
  assert(listed.intents.length === 1 && listed.intents[0].destination_handle === "@sakeboyhk", "List returns exact-destination intents");
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
  const cancelled = await store.socialPublish.cancel("intent-a");
  assert(cancelled.intent.state === "cancelled", "Cancel closes the intent");
  assert(calls.length > 0 && calls.every(call => call.url.startsWith("/api/") && !call.url.includes("http")), "Every client call stays same-origin — no external provider reachable");
  const states = ["queued", "processing", "posted", "refused", "cancelled", "held"] as const;
  assert(states.every(state => store.publishStateText(state).length > 0), "All six store states have operator text");
  assert(store.publishStateTone("posted") === "ok" && store.publishStateTone("refused") === "fail"
    && store.publishStateTone("held") === "warn"
    && store.publishStateTone("queued") === "muted" && store.publishStateTone("processing") === "muted", "State tones match the approved language");
  assert(store.publishStateText("held").includes("human") && !store.publishStateText("held").includes("reconnect"), "Held names the human, not a reconnect state");
  assert(store.reconcileReading("processing")?.includes("uncertain") && store.reconcileReading("held")?.includes("human")
    && store.reconcileReading("posted") === null && store.reconcileReading("queued") === null, "Uncertain reads as processing-or-held only");
  const vocabulary = ["bad_key", "bad_connection", "bad_destination", "bad_caption_digest", "bad_image_digest", "bad_run", "bad_effect", "bad_grant", "bad_intent", "bad_revision", "bad_timezone", "cancel_closed", "cross_workspace", "grant_mismatch", "binding_mismatch", "grant_revoked", "grant_exhausted", "grant_window", "grant_approval", "grant_bounds", "image_required", "key_conflict", "not_publishable", "wrong_connection", "wrong_destination", "wrong_toolkit", "unknown_key", "send_disabled"];
  assert(vocabulary.length === 28, "Refusal vocabulary is exactly the 28-code list");
  for (const code of vocabulary) {
    const copy = store.refusalCopy({ code, message: "" }).toLowerCase();
    assert(copy.includes("nothing was published") || copy.includes("nothing was stored") || copy.includes("no provider was called") || copy.includes("fails instead") || copy.includes("nothing to reconcile") || copy.includes("already left queued") || copy.includes("refused"), `Refusal ${code} stays explicit`);
  }
  assert(store.refusalCopy({ code: "send_disabled", message: "" }).includes("not a state"), "send_disabled is a refusal code, not a state");
  assert(store.refusalCopy({ code: "future_code", message: "raw" }).includes("future_code") && store.refusalCopy({ code: "future_code", message: "raw" }).includes("raw"), "Unknown codes stay visible with raw message");
  assert(store.isApprovalIdUsable("op-a") && !store.isApprovalIdUsable("") && !store.isApprovalIdUsable("x".repeat(121)) && store.isApprovalIdUsable("x".repeat(120)), "Approval bound is 1..=120 characters");
  assert(store.refusalCopy({ code: "send_disabled", message: "" }).includes("changed nothing"), "Disabled gate names validate-all/mutate-nothing");
  assert(store.canCancel("queued") && !store.canCancel("processing") && !store.canCancel("posted") && !store.canCancel("cancelled"), "Only queued intents cancel (cancel_closed past queued)");
  const label = store.dueLabel(1790601000, "Asia/Hong_Kong");
  assert(label.includes("Asia/Hong_Kong") && label.includes("1790601000"), "Due label always shows timezone and epoch");
  assert(store.parseDueEpoch("2026-09-30T18:30") !== null && store.parseDueEpoch("") === null && store.parseDueEpoch("tomorrow") === null, "Incomplete due values keep Schedule gated");
  assert(store.PILOT_DESTINATION.handle === "@sakeboyhk" && store.PILOT_DESTINATION.account_id === "17841400008460056", "Pilot identity is the operator-named account");
  console.log("social publish client checks passed");
}
void main();
