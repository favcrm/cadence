import {
  applyPrivateApp,
  authorizeHostedPrivateReview,
  checkPrivateApp,
  decideHostedPrivateReview,
  getHostedPrivateReview,
  grantPrivateAppUpdatePolicy,
  listHostedPrivateReviews,
  readPrivateAppUpdatePolicy,
  revokePrivateAppUpdatePolicy,
} from "../src/features/workspace-apps/privateAppUpload";
import { ApiError, errorRequestId } from "../src/lib/apiError";
import type * as HappyDom from "happy-dom";
import type * as React from "react";
import type * as ReactDom from "react-dom/client";
import type * as Locale from "../src/lib/locale";
import type * as Dialog from "../src/features/workspace-apps/PrivateAppDialog";
import type * as ReviewCardsModule from "../src/features/home/PrivateAppReviewCards";
import type * as UpdaterSettingsModule from "../src/features/workspace-apps/PrivateAppUpdaterSettings";

declare function require(name: string): unknown;
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
let responses: { status: number; body: unknown; headers?: Record<string, string> }[] = [];
globalThis.fetch = async (input, init) => {
  calls.push({ path: String(input), init: init ?? {} });
  const next = responses.shift();
  if (!next) throw new Error("unexpected request");
  return new Response(JSON.stringify(next.body), {
    status: next.status,
    headers: { "Content-Type": "application/json", ...next.headers },
  });
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
  responses = [{ status: 200, body: { ...receipt, data: { ...receipt.data, catalog_stale: true } } }];
  const stale = await getHostedPrivateReview(reviewId);
  equal(stale.catalogStale, true, "the owner read exposes catalog drift without changing the review state");
  equal(stale.state, "pending");
  const beforeKnownStale = calls.length;
  await rejected(decideHostedPrivateReview(stale, "install"), (error) => error instanceof ApiError,
    "the client does not send an install POST for a known stale review");
  equal(calls.length, beforeKnownStale);
  responses = [{ status: 200, body: { ...receipt, data: { ...receipt.data, digest: `sha256:${"b".repeat(64)}` } } }];
  await rejected(getHostedPrivateReview(reviewId), (error) => error instanceof ApiError && error.status === 502,
    "a mismatched review fingerprint cannot be presented for approval");

  // Mount the real owner dialog against the same transport fixtures.
  const { Window } = require("happy-dom") as typeof HappyDom;
  const win = new Window({ url: "http://localhost/apps" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement",
    "navigator", "Event", "MouseEvent", "location", "history"] as const) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const { act, createElement } = require("react") as typeof React;
  const { createRoot } = require("react-dom/client") as typeof ReactDom;
  const { LocaleProvider } = require("../src/lib/locale") as typeof Locale;
  // The icon package's CommonJS entry is mislabeled; icons are unrelated to the dialog behavior.
  const loader = require("node:module") as {
    prototype: { require(this: unknown, name: string): unknown };
  };
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (name) {
    if (name === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (name === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.call(this, name);
  };
  let dialog: typeof Dialog;
  let reviewCardsMod: typeof ReviewCardsModule;
  let updaterSettingsMod: typeof UpdaterSettingsModule;
  try {
    dialog = require("../src/features/workspace-apps/PrivateAppDialog") as typeof Dialog;
    reviewCardsMod = require("../src/features/home/PrivateAppReviewCards") as typeof ReviewCardsModule;
    updaterSettingsMod = require("../src/features/workspace-apps/PrivateAppUpdaterSettings") as typeof UpdaterSettingsModule;
  } finally {
    loader.prototype.require = originalRequire;
  }
  const { PrivateAppDialog } = dialog;
  const PrivateAppReviewCards = reviewCardsMod.default;
  const { PrivateAppUpdaterSettings } = updaterSettingsMod;
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const mount = (key: string) => act(async () => root.render(createElement(LocaleProvider, null,
    createElement(PrivateAppDialog, { key, reviewId, onClose() {}, onInstalled() {} }))));
  const approve = () => [...host.querySelectorAll("button")].find((button) =>
    button.textContent?.trim() === "Approve and install");
  const staleReceipt = { ...receipt, data: { ...receipt.data, catalog_stale: true } };
  const recovery = "Workspace apps changed after this review was created. Ask the requester for a new review.";
  try {
    calls = [];
    responses = [{ status: 200, body: staleReceipt }];
    await mount("already-stale");
    equal(host.querySelector('[role="alert"]')?.textContent?.trim(), recovery);
    equal(approve(), undefined, "an already-stale review has no approval action");
    equal(calls.length, 1, "opening a stale dialog does not submit a decision");

    calls = [];
    responses = [{ status: 200, body: { ...receipt, data: { ...receipt.data, catalog_stale: false } } }];
    await mount("then-stale");
    equal(approve()?.disabled, true, "a current review first requires consent");
    const consent = host.querySelector<HTMLInputElement>('input[type="checkbox"]');
    if (!consent) throw new Error("the current review must offer consent");
    await act(async () => consent.click());
    const button = approve();
    if (!button || button.disabled) throw new Error("a current review can be approved after consent");
    responses = [staleResponse, { status: 200, body: staleReceipt }];
    await act(async () => button.click());
    equal(host.querySelector('[role="alert"]')?.textContent?.trim(), recovery);
    equal(approve(), undefined, "a late refused approval cannot be submitted again from the stale dialog");
    equal(calls.map(({ init }) => init.method ?? "GET").join(" "), "GET POST GET",
      "the stale refusal is reconciled without replaying approval");

    // AOS-690 transport helpers & pre-AOS-690 404/501 degradation.
    equal(errorRequestId({ error: { request_id: "req_body_1" } }), "req_body_1");
    equal(
      errorRequestId({}, new Response(null, { headers: { "x-request-id": "req_hdr_2" } })),
      "req_hdr_2",
    );
    equal(errorRequestId({ error: { request_id: "bad space" } }), undefined);

    for (const status of [404, 501]) {
      responses = [{ status, body: { error: { code: "not_found" } } }];
      const missing = await listHostedPrivateReviews();
      equal(missing.supported, false);
      equal(missing.reviews.length, 0);

      responses = [{ status, body: { error: { code: "not_found" } } }];
      const polMissing = await readPrivateAppUpdatePolicy("install_app1");
      equal(polMissing.supported, false);
      equal(polMissing.policy, null);
    }

    const agentReceipt = {
      ok: true,
      data: {
        ...receipt.data,
        install_id: "install_app1",
        expected_digest: digest,
        requesting_agent: "agents/ap_upd",
        will_revoke_grants: true,
        active_bindings_count: 2,
        review: {
          ...review,
          install_id: "install_app1",
          expected_digest: digest,
          expected_generation: review.catalog_generation,
          structural_diff: { added: ["app.md"], changed: [], removed: [] },
          previous_screen_access: review.screen_access,
        },
      },
    };
    responses = [{ status: 200, body: { ok: true, data: { reviews: [agentReceipt.data] } } }];
    const listed = await listHostedPrivateReviews();
    equal(listed.supported, true);
    equal(listed.reviews.length, 1);
    equal(listed.reviews[0]?.requestingAgent, "agents/ap_upd");
    equal(listed.reviews[0]?.activeBindingsCount, 2);

    responses = [{ status: 200, body: { ok: true, data: { ...agentReceipt.data, authorized_by: "owner_1" } } }];
    const authorized = await authorizeHostedPrivateReview(listed.reviews[0]!);
    equal(authorized.authorizedBy, "owner_1");

    const grantedPolicy = {
      id: "pol_1",
      install_id: "install_app1",
      agent: "agents/ap_upd",
      source_kind: "staged_zip",
      repository_id: 0,
      owner: "owner_1",
      expires_at: Date.now() + 86_400_000,
      installed_digest: digest,
    };
    calls = [];
    responses = [{ status: 200, body: { ok: true, data: { policy: grantedPolicy } } }];
    const savedPolicy = await grantPrivateAppUpdatePolicy({
      installId: "install_app1",
      expectedDigest: digest,
      agent: "agents/ap_upd",
      source: "staged_zip",
      expiresAt: grantedPolicy.expires_at,
    });
    equal(savedPolicy.source_kind, "staged_zip");
    const grantPayload = JSON.parse(String(calls[0]?.init.body)) as Record<string, unknown>;
    equal("repository_id" in grantPayload, false, "staged_zip policy must omit repository_id");
    equal(grantPayload.expires_at, grantedPolicy.expires_at);

    responses = [{ status: 200, body: { ok: true, data: { revoked: true, install_id: "install_app1", policy_id: "pol_1" } } }];
    await revokePrivateAppUpdatePolicy("install_app1", "pol_1");

    // Mounted PrivateAppDialog: Authorize button, request_id ref, and opt-in 30-day policy after install.
    calls = [];
    responses = [{ status: 200, body: agentReceipt }];
    await mount("agent-review");
    equal(host.textContent?.includes("agents/ap_upd"), true);
    equal(host.textContent?.includes("2 connection or tool grants"), true);
    const agentConsent = host.querySelector<HTMLInputElement>('input[type="checkbox"]');
    if (!agentConsent) throw new Error("expected consent checkbox");
    await act(async () => agentConsent.click());
    const authBtn = [...host.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Authorize agent to apply");
    if (!authBtn) throw new Error("expected Authorize agent to apply button");
    responses = [{ status: 200, body: { ok: true, data: { ...agentReceipt.data, authorized_by: "owner_1" } } }];
    await act(async () => authBtn.click());
    equal(host.textContent?.includes("Authorized for agent apply"), true);

    const approveBtn = approve();
    if (!approveBtn) throw new Error("expected Approve and install button");
    responses = [{ status: 200, body: { ok: true, data: { ...agentReceipt.data, state: "installed", applied_install_id: "install_app1" } } }];
    await act(async () => approveBtn.click());
    const optInBtn = [...host.querySelectorAll("button")].find((b) =>
      b.textContent?.trim() === "Allow routine patch updates (30 days)");
    if (!optInBtn) throw new Error("expected explicit post-install 30-day opt-in button");
    equal(calls.filter((c) => c.path.includes("update-policy")).length, 0, "policy is never granted automatically");
    responses = [{ status: 200, body: { ok: true, data: { policy: grantedPolicy } } }];
    await act(async () => optInBtn.click());
    equal(host.textContent?.includes("Routine patch permission saved for 30 days."), true);

    // Local check refusal surfaces Ref: <requestId>.
    responses = [{ status: 200, body: { schema: 1, enabled: false } }];
    await act(async () => root.render(createElement(LocaleProvider, null,
      createElement(PrivateAppDialog, { key: "diag-upload", onClose() {}, onInstalled() {} }))));
    const fileInput = host.querySelector<HTMLInputElement>('input[type="file"]');
    if (!fileInput) throw new Error("expected file input");
    const dtFile = new File(["zip-bytes"], "bad.zip", { type: "application/zip" });
    Object.defineProperty(fileInput, "files", { value: [dtFile], configurable: true });
    await act(async () => { fileInput.dispatchEvent(new Event("change", { bubbles: true })); });
    const checkBtn = [...host.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Check package");
    if (!checkBtn) throw new Error("expected Check package button");
    responses = [{
      status: 409,
      headers: { "x-request-id": "req_diag_99" },
      body: { ok: false, error: { code: "invalid_archive", message: "The zip manifest was unreadable.", request_id: "req_diag_99" } },
    }];
    await act(async () => checkBtn.click());
    equal(host.textContent?.includes("Ref: req_diag_99"), true);

    // Mounted Home PrivateAppReviewCards: hides silently on 404; installs inline on 200.
    responses = [{ status: 404, body: { error: "not_found" } }];
    await act(async () => root.render(createElement(LocaleProvider, null,
      createElement(PrivateAppReviewCards, { enabled: true, revision: 1 }))));
    equal(host.innerHTML, "", "404 pre-AOS-690 review list hides silently");

    responses = [{ status: 200, body: { ok: true, data: { reviews: [agentReceipt.data] } } }];
    await act(async () => root.render(createElement(LocaleProvider, null,
      createElement(PrivateAppReviewCards, { enabled: true, revision: 2 }))));
    const link = host.querySelector<HTMLAnchorElement>(`a[href="/apps?review=${reviewId}"]`);
    if (!link) throw new Error("expected fallback /apps?review= link in Home card");
    const cardApprove = [...host.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Approve and install");
    if (!cardApprove) throw new Error("expected inline Approve and install button in Home card");
    responses = [{ status: 200, body: { ok: true, data: { ...agentReceipt.data, state: "installed", applied_install_id: "install_app1" } } }];
    await act(async () => cardApprove.click());
    equal(host.textContent?.includes("Installed."), true);

    // Mounted Manage PrivateAppUpdaterSettings: hides on 404, grants & revokes on 200.
    responses = [{ status: 404, body: { error: "not_found" } }];
    await act(async () => root.render(createElement(LocaleProvider, null,
      createElement(PrivateAppUpdaterSettings, { installId: "install_app1", installedDigest: digest, busy: false }))));
    equal(host.innerHTML, "", "404 pre-AOS-690 update-policy hides silently");

    responses = [
      { status: 200, body: { ok: true, data: { policy: null } } },
      { status: 200, body: { profiles: [{ id: "ap_upd", name: "App Updater", status: "active" }] } },
    ];
    await act(async () => root.render(createElement(LocaleProvider, null,
      createElement(PrivateAppUpdaterSettings, { key: "mgr-200", installId: "install_app1", installedDigest: digest, busy: false }))));
    equal(host.textContent?.includes("Private app update automation"), true);
    const savePolicyBtn = [...host.querySelectorAll("button")].find((b) =>
      b.textContent?.trim() === "Allow routine patch updates (30 days)");
    if (!savePolicyBtn) throw new Error("expected policy grant button");
    responses = [{ status: 200, body: { ok: true, data: { policy: grantedPolicy } } }];
    await act(async () => savePolicyBtn.click());
    equal(host.textContent?.includes("Revoke permission"), true);
    const revokeBtn = [...host.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Revoke permission");
    if (!revokeBtn) throw new Error("expected Revoke permission button");
    responses = [{ status: 200, body: { ok: true, data: { revoked: true, install_id: "install_app1", policy_id: "pol_1" } } }];
    await act(async () => revokeBtn.click());
    equal(host.textContent?.includes("Routine patch permission revoked."), true);
  } finally {
    await act(async () => root.unmount());
    await win.happyDOM.close();
  }
  console.log("private app upload receipt checks passed");
}
void main();
