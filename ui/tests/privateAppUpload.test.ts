import { checkPrivateApp, applyPrivateApp, decideHostedPrivateReview, getHostedPrivateReview } from "../src/features/workspace-apps/privateAppUpload";
import { ApiError } from "../src/lib/apiError";
function equal(actual: unknown, expected: unknown, message = "values differ"): void {
  if (actual !== expected) throw new Error(message);
}
async function rejected(action: Promise<unknown>, check: (error: unknown) => boolean, message: string): Promise<void> {
  try { await action; } catch (error) { if (check(error)) return; throw new Error(message); }
  throw new Error(`${message}: unexpectedly succeeded`);
}

const digest = `sha256:${"a".repeat(64)}`;
const file = new File(["synthetic zip"], "private.zip", { type: "application/zip" });
const review = {
  schema: 1, name: "private-checklist", title: "Private checklist", version: "1.0.0", digest,
  files: ["app.md"], committed: false, visibility: "workspace", catalog_generation: "c1:synthetic",
  access_diff: { connections: { added: [], removed: [] }, capabilities: {} },
  declared_access: { connections: [], capabilities: {} },
  screen_access: [{ tag: "main", host_contract: "screen-records.v1", operations: ["records.list"], tools: null, remote_image_origins: [] }],
  compatibility: { schema: 1, ok: true, compatible: true, declared: true, checks: [] },
  notes: [], secret_warnings: [],
};
let calls: { path: string; init: RequestInit }[] = [];
let responses: { status: number; body: unknown }[] = [];
globalThis.fetch = async (input, init) => {
  calls.push({ path: String(input), init: init ?? {} });
  const next = responses.shift();
  if (!next) throw new Error("unexpected request");
  return new Response(JSON.stringify(next.body), { status: next.status, headers: { "Content-Type": "application/json" } });
};

async function main() {
  responses = [{ status: 200, body: { ...review, compatibility: { ...review.compatibility, checks: [{ kind: "contract" }] } } }];
  await rejected(checkPrivateApp(file, null), (error: unknown) => error instanceof ApiError && error.status === 502,
    "an incomplete compatibility check cannot become install consent");
  const failedCheck = { kind: "core", required: "2", host: "1", ok: false };
  responses = [{ status: 200, body: { ...review, compatibility: { ...review.compatibility, checks: [failedCheck] } } }];
  await rejected(checkPrivateApp(file, null), (error: unknown) => error instanceof ApiError && error.status === 502,
    "a failed compatibility check cannot be presented as compatible");
  responses = [{ status: 200, body: { ...review, compatibility: { ...review.compatibility, ok: false, compatible: false, checks: [{ ...failedCheck, ok: true }] } } }];
  await rejected(checkPrivateApp(file, null), (error: unknown) => error instanceof ApiError && error.status === 502,
    "a contradictory incompatible verdict cannot become a review");
  responses = [{ status: 200, body: { ...review, compatibility: { ...review.compatibility, ok: false, compatible: false, checks: [failedCheck] } } }];
  equal((await checkPrivateApp(file, null)).compatible, false, "a consistent incompatible review remains visible");
  responses = [{ status: 409, body: { ok: false, error: { code: "host_incompatible", message: "Unsupported tool." } } }];
  await rejected(checkPrivateApp(file, null), (error: unknown) => error instanceof ApiError && error.code === "host_incompatible",
    "a nested host refusal retains its explanation");
  responses = [{ status: 200, body: { ...review, declared_access: { connections: [], capabilities: {
    publishing: { schema: 1, version: 1, capability: "text.publish", action: "publish", resource_kind: "connection_account", effect: "read" },
  } } } }];
  await rejected(checkPrivateApp(file, null), (error: unknown) => error instanceof ApiError && error.status === 502,
    "an invalid capability tuple cannot be shown as an approved grant");

  calls = [];
  responses = [
    { status: 200, body: review },
    { status: 200, body: { committed: true, source_kind: "private", digest, name: review.name, version: review.version, install_id: "i1" } },
    { status: 404, body: { error: "not found" } },
  ];
  const checked = await checkPrivateApp(file, null);
  await rejected(applyPrivateApp(file, checked), (error: unknown) => error instanceof ApiError && error.status === 502,
    "a failed readback after successful POST is uncertain, not a definite refusal");
  equal(calls[0]?.init.body, file, "check sends the selected file");
  equal(calls[1]?.init.body, file, "apply sends the same file");
  equal(new Headers(calls[1]?.init.headers).get("x-cadence-app-digest"), digest);
  equal(new Headers(calls[1]?.init.headers).get("x-cadence-app-generation"), review.catalog_generation);
  equal(calls[2]?.path, "/api/app-installations/i1", "success requires an installation readback");
  const reviewId = `par_${"1".repeat(32)}`;
  const receipt = { ok: true, data: { id: reviewId, state: "pending", expires_at: 2000000000000,
    source: "https://github.com/a/b/releases/download/v1/app.zip", zip_sha256: digest,
    digest, name: review.name, generation: review.catalog_generation, install_id: null, expected_digest: null, review } };
  calls = [];
  await rejected(getHostedPrivateReview("par_123"), (error: unknown) => error instanceof ApiError,
    "a malformed review link is refused before any fetch");
  equal(calls.length, 0);
  responses = [{ status: 200, body: receipt }];
  const hosted = await getHostedPrivateReview(reviewId);
  equal(calls.length, 1, "opening a review performs only a read");
  equal(calls[0]?.init.method, undefined);
  responses = [{ status: 200, body: { ...receipt, data: { ...receipt.data, state: "installed" } } }];
  equal((await decideHostedPrivateReview(hosted, "install")).state, "installed");
  equal(calls[1]?.path, `/api/app-installations/private/reviews/${reviewId}/install`);
  equal(calls[1]?.init.body, JSON.stringify({ digest, zip_sha256: digest }), "approval binds both fingerprints");
  responses = [{ status: 200, body: { ...receipt, data: { ...receipt.data, state: "rejected" } } }];
  equal((await decideHostedPrivateReview(hosted, "reject")).state, "rejected");
  equal(calls[2]?.path, `/api/app-installations/private/reviews/${reviewId}/reject`);
  responses = [{ status: 200, body: { ...receipt, data: { ...receipt.data, state: "installed" } } }];
  equal((await decideHostedPrivateReview(hosted, "reject")).state, "installed",
    "a concurrent owner approval stays installed");
  responses = [
    { status: 409, body: { error: { code: "review_closed", message: "Review closed." } } },
    { status: 200, body: { ...receipt, data: { ...receipt.data, state: "expired" } } },
  ];
  const beforeClosed = calls.length;
  equal((await decideHostedPrivateReview(hosted, "install")).state, "expired");
  equal(calls.length, beforeClosed + 2);
  equal(calls[beforeClosed]?.init.method, "POST");
  equal(calls[beforeClosed + 1]?.init.method, undefined, "a closed review is reconciled without replaying approval");
  const staleResponse = { status: 409, body: { error: { code: "installation_stale", message: "Catalog generation changed." } } };
  responses = [staleResponse, { status: 200, body: receipt }];
  const beforeStale = calls.length;
  await rejected(decideHostedPrivateReview(hosted, "install"),
    (error: unknown) => error instanceof ApiError && error.code === "installation_stale",
    "a still-pending stale package requires a fresh review");
  equal(calls.length, beforeStale + 2);
  equal(calls[beforeStale + 1]?.init.method, undefined);
  responses = [staleResponse, { status: 200, body: { ...receipt, data: { ...receipt.data, state: "installed" } } }];
  equal((await decideHostedPrivateReview(hosted, "install")).state, "installed",
    "an owner who won the activation race leaves a verified installed receipt");
  responses = [{ status: 200, body: { ...receipt, data: { ...receipt.data, digest: `sha256:${"b".repeat(64)}` } } }];
  await rejected(getHostedPrivateReview(reviewId), (error) => error instanceof ApiError && error.status === 502,
    "a mismatched review fingerprint cannot be presented for approval");
  console.log("private app upload receipt checks passed");
}
void main();
