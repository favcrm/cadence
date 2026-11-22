import { ApiError, errorMessage, errorRequestId } from "../../lib/apiError";
import { privateFetch, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import { workspaceApps, type AccessDiff, type Installation } from "./workspaceApps";

const DIGEST = /^sha256:[0-9a-f]{64}$/;
const ZIP_LIMIT = 8 * 1024 * 1024;
const GITHUB_SOURCE = "/api/app-installations/private/github";
const HOSTED_REVIEW_ID = /^par_[0-9a-f]{32}$/;
const AGENT_ACTOR = /^agents\/[A-Za-z0-9_-]{1,120}$/;
const INSTALL_ID = /^install_[A-Za-z0-9_-]{1,120}$/;

/** Discovery is deliberately optional: older hosts keep the upload-only flow. */
export async function githubPrivateAppAvailable(signal?: AbortSignal): Promise<boolean> {
  try {
    return await withPrivateOperation(async (operation) => {
      const response = await privateFetch(GITHUB_SOURCE, {
        credentials: "same-origin", cache: "no-store",
        headers: { "X-Cadence-Board": "1", ...sessionHeaders() },
      }, operation);
      if (!response.ok) return false;
      const body: unknown = await response.json().catch(() => null);
      operation.assertCurrent();
      return object(body) && body.schema === 1 && body.enabled === true;
    }, signal);
  } catch {
    return false;
  }
}

/** Read at most the supported ZIP size before passing the same File to check and apply. */
export async function fetchGithubPrivateApp(url: string): Promise<{ file: File; digest: string; sessionGeneration: number }> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(GITHUB_SOURCE, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify({ url }),
    }, operation);
    if (!response.ok) {
      const body: unknown = await response.json().catch(() => null);
      operation.assertCurrent();
      throw new ApiError(errorMessage(body, "The GitHub package could not be fetched."), response.status, {
        code: refusalCode(body),
        requestId: errorRequestId(body, response),
      });
    }
    const digest = response.headers.get("X-Cadence-App-Zip-Sha256");
    if (response.status !== 200 || !/^application\/zip(?:\s*;|$)/i.test(response.headers.get("Content-Type") ?? "") ||
        !digest || !DIGEST.test(digest) || !response.body) throw invalid();
    const reader = response.body.getReader();
    const chunks: Uint8Array<ArrayBuffer>[] = [];
    let size = 0;
    try {
      while (true) {
        const { done, value } = await reader.read();
        operation.assertCurrent();
        if (done) break;
        size += value.byteLength;
        if (size > ZIP_LIMIT) throw invalid();
        chunks.push(value as Uint8Array<ArrayBuffer>);
      }
    } finally {
      if (size > ZIP_LIMIT) void reader.cancel().catch(() => {});
      reader.releaseLock();
    }
    if (!size) throw invalid();
    return { file: new File(chunks, "github-release.zip", { type: "application/zip" }), digest, sessionGeneration: operation.generation };
  });
}

export type PrivateTarget = { installId: string; name: string; digest: string; generation: string };
type ScreenAccess = {
  tag: string;
  host_contract: string | null;
  operations: string[] | null;
  tools: string[] | null;
  remote_image_origins: string[];
};
export type PrivateReview = {
  name: string;
  title: string;
  summary: string | null;
  version: string;
  digest: string;
  files: string[];
  generation: string;
  access: AccessDiff;
  declared: { connections: string[]; capabilities: Record<string, NonNullable<AccessDiff["capabilities"][string]["after"]>> };
  screens: ScreenAccess[];
  previousScreens: ScreenAccess[] | null;
  structural: { added: string[]; changed: string[]; removed: string[] } | null;
  compatible: boolean;
  warnings: boolean;
  sessionGeneration: number;
  target: PrivateTarget | null;
  willRevokeGrants?: boolean;
  activeBindingsCount?: number;
};

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function strings(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === "string");
}
function slot(value: unknown): boolean {
  if (!object(value) || value.schema !== 1 || value.version !== 1 ||
      typeof value.capability !== "string" || typeof value.action !== "string" ||
      typeof value.resource_kind !== "string" || typeof value.effect !== "string") return false;
  if (value.resource_kind === "installation")
    return value.capability === "file.upload" && value.action === "attach" && value.effect === "draft";
  if (value.resource_kind !== "connection_account") return false;
  if (value.capability === "text.publish") return value.action === "publish" && value.effect === "send";
  const parts = value.capability.split(".");
  return value.capability.length <= 64 && parts.length === 2 &&
    parts.every((part) => /^[a-z0-9][a-z0-9-]{0,31}$/.test(part)) &&
    /^[a-z0-9_]{1,32}$/.test(value.action) && (value.effect === "read" || value.effect === "draft");
}
function screens(value: unknown): value is ScreenAccess[] {
  return Array.isArray(value) && value.every((screen) => object(screen) && typeof screen.tag === "string" &&
    (screen.host_contract === null || typeof screen.host_contract === "string") &&
    (screen.operations === null || strings(screen.operations)) &&
    (screen.tools === null || strings(screen.tools)) && strings(screen.remote_image_origins));
}
function access(value: unknown): value is AccessDiff {
  return object(value) && object(value.connections) && strings(value.connections.added) &&
    strings(value.connections.removed) && object(value.capabilities) &&
    Object.values(value.capabilities).every((change) => object(change) &&
      (change.before === null || slot(change.before)) && (change.after === null || slot(change.after)));
}
function diff(value: unknown): value is { added: string[]; changed: string[]; removed: string[] } {
  return object(value) && strings(value.added) && strings(value.changed) && strings(value.removed);
}
function declaredAccess(value: unknown): value is PrivateReview["declared"] {
  return object(value) && strings(value.connections) && object(value.capabilities) &&
    Object.values(value.capabilities).every(slot);
}
function compatibility(value: unknown): value is { compatible: boolean } {
  if (!object(value) || value.schema !== 1 || typeof value.declared !== "boolean" ||
      typeof value.ok !== "boolean" || typeof value.compatible !== "boolean" ||
      value.ok !== value.compatible || !Array.isArray(value.checks)) return false;
  let allPassed = true;
  for (const check of value.checks) {
    if (!object(check) || typeof check.ok !== "boolean" ||
        !(check.kind === "core"
          ? typeof check.required === "string" && typeof check.host === "string"
          : check.kind === "contract" && typeof check.contract === "string" &&
            Array.isArray(check.required) && check.required.every(Number.isInteger) &&
            Array.isArray(check.supported) && check.supported.every(Number.isInteger))) return false;
    if (!check.ok) allPassed = false;
  }
  return value.compatible === allPassed;
}
function refusalCode(value: unknown): string | undefined {
  const reason = object(value) && object(value.error) ? value.error : value;
  return object(reason) && typeof reason.code === "string" && /^[a-z0-9_]{1,64}$/.test(reason.code) ? reason.code : undefined;
}
function invalid(): ApiError {
  return new ApiError("The package review could not be verified.", 502);
}

function parseReview(value: unknown, target: PrivateTarget | null, sessionGeneration: number): PrivateReview {
  if (!object(value) || value.schema !== 1 || value.committed !== false || value.visibility !== "workspace" ||
      typeof value.name !== "string" || !value.name || typeof value.title !== "string" || !value.title ||
      (value.summary !== undefined && (typeof value.summary !== "string" || value.summary.length > 600)) ||
      typeof value.version !== "string" || !value.version || typeof value.digest !== "string" || !DIGEST.test(value.digest) ||
      typeof value.catalog_generation !== "string" || !value.catalog_generation || !strings(value.files) ||
      !access(value.access_diff) || !declaredAccess(value.declared_access) || !screens(value.screen_access) || value.screen_access.length !== 1 ||
      !strings(value.notes) || !Array.isArray(value.secret_warnings) || !value.secret_warnings.every(object) ||
      !compatibility(value.compatibility)) throw invalid();
  if (target && (value.install_id !== target.installId || value.name !== target.name ||
      value.expected_digest !== target.digest || value.expected_generation !== target.generation ||
      value.catalog_generation !== target.generation || !diff(value.structural_diff) || !screens(value.previous_screen_access))) throw invalid();
  const willRevokeGrants = typeof value.will_revoke_grants === "boolean" ? value.will_revoke_grants : undefined;
  const activeBindingsCount = typeof value.active_bindings_count === "number" &&
    Number.isInteger(value.active_bindings_count) && value.active_bindings_count >= 0
    ? value.active_bindings_count
    : undefined;
  return {
    name: value.name,
    title: value.title,
    summary: typeof value.summary === "string" ? value.summary.trim() || null : null,
    version: value.version,
    digest: value.digest,
    files: value.files,
    generation: value.catalog_generation,
    access: value.access_diff,
    declared: value.declared_access,
    screens: value.screen_access,
    previousScreens: target ? value.previous_screen_access as ScreenAccess[] : null,
    structural: target ? value.structural_diff as PrivateReview["structural"] : null,
    compatible: value.compatibility.compatible,
    warnings: value.secret_warnings.length > 0,
    sessionGeneration,
    target,
    ...(willRevokeGrants !== undefined ? { willRevokeGrants } : {}),
    ...(activeBindingsCount !== undefined ? { activeBindingsCount } : {}),
  };
}

export type HostedPrivateReview = {
  id: string;
  state: "pending" | "installed" | "rejected" | "expired";
  catalogStale: boolean;
  expiresAt: number;
  source: string;
  zipSha256: string;
  review: PrivateReview;
  requestingAgent?: string;
  authorizedBy?: string;
  willRevokeGrants?: boolean;
  activeBindingsCount?: number;
  installedInstallId?: string;
};

export type PrivateAppRoutinePolicy = {
  id: string;
  install_id: string;
  agent: string;
  source_kind: "github" | "staged_zip";
  expires_at: number;
  installed_digest: string;
};

function boundedText(value: unknown, max = 256): string | undefined {
  return typeof value === "string" && value.length > 0 && value.length <= max ? value : undefined;
}

function parseHostedReviewRecord(data: Record<string, unknown>, id: string, generation: number): HostedPrivateReview {
  if (data.id !== id || typeof data.state !== "string" ||
      !["pending", "installed", "rejected", "expired"].includes(data.state) ||
      (data.catalog_stale !== undefined && typeof data.catalog_stale !== "boolean") ||
      typeof data.expires_at !== "number" || !Number.isFinite(data.expires_at) ||
      typeof data.source !== "string" || typeof data.zip_sha256 !== "string" ||
      !DIGEST.test(data.zip_sha256) || !object(data.review)) throw invalid();
  const raw = data.review;
  let target: PrivateTarget | null = null;
  if (data.install_id != null) {
    if (typeof data.install_id !== "string" || typeof data.expected_digest !== "string" ||
        typeof raw.expected_generation !== "string" || typeof data.name !== "string") throw invalid();
    target = { installId: data.install_id, name: data.name, digest: data.expected_digest, generation: raw.expected_generation };
  }
  const review = parseReview(raw, target, generation);
  if (data.name !== review.name || data.digest !== review.digest || data.generation !== review.generation) throw invalid();
  const requestingAgent = boundedText(data.requesting_agent, 160);
  const authorizedBy = boundedText(data.authorized_by, 160);
  const willRevokeGrants = typeof data.will_revoke_grants === "boolean"
    ? data.will_revoke_grants
    : review.willRevokeGrants;
  const activeBindingsCount = typeof data.active_bindings_count === "number" &&
    Number.isInteger(data.active_bindings_count) && data.active_bindings_count >= 0
    ? data.active_bindings_count
    : review.activeBindingsCount;
  const installedInstallId = boundedText(data.applied_install_id, 160) ?? target?.installId;
  return {
    id,
    state: data.state as HostedPrivateReview["state"],
    catalogStale: data.catalog_stale === true,
    expiresAt: data.expires_at,
    source: data.source,
    zipSha256: data.zip_sha256,
    review,
    ...(requestingAgent ? { requestingAgent } : {}),
    ...(authorizedBy ? { authorizedBy } : {}),
    ...(willRevokeGrants !== undefined ? { willRevokeGrants } : {}),
    ...(activeBindingsCount !== undefined ? { activeBindingsCount } : {}),
    ...(installedInstallId ? { installedInstallId } : {}),
  };
}

function parseHostedReview(body: unknown, id: string, generation: number): HostedPrivateReview {
  if (!object(body) || body.ok !== true || !object(body.data)) throw invalid();
  return parseHostedReviewRecord(body.data, id, generation);
}

export async function listHostedPrivateReviews(signal?: AbortSignal): Promise<{ supported: boolean; reviews: HostedPrivateReview[] }> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch("/api/app-installations/private/reviews", {
      credentials: "same-origin", cache: "no-store", headers: { "X-Cadence-Board": "1", ...sessionHeaders() },
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (response.status === 403 || response.status === 404 || response.status === 501) {
      return { supported: false, reviews: [] };
    }
    if (!response.ok) {
      throw new ApiError(errorMessage(body, "Pending private app reviews are unavailable."), response.status, {
        code: refusalCode(body),
        requestId: errorRequestId(body, response),
      });
    }
    if (!object(body) || body.ok !== true || !object(body.data) || !Array.isArray(body.data.reviews)) throw invalid();
    const reviews: HostedPrivateReview[] = [];
    for (const row of body.data.reviews) {
      if (!object(row) || typeof row.id !== "string" || !HOSTED_REVIEW_ID.test(row.id)) throw invalid();
      reviews.push(parseHostedReviewRecord(row, row.id, operation.generation));
    }
    return { supported: true, reviews };
  }, signal);
}

export async function getHostedPrivateReview(id: string, signal?: AbortSignal): Promise<HostedPrivateReview> {
  if (!HOSTED_REVIEW_ID.test(id)) throw invalid();
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(`/api/app-installations/private/reviews/${encodeURIComponent(id)}`, {
      credentials: "same-origin", cache: "no-store", headers: { "X-Cadence-Board": "1", ...sessionHeaders() },
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) {
      throw new ApiError(errorMessage(body, "The review is unavailable."), response.status, {
        code: refusalCode(body),
        requestId: errorRequestId(body, response),
      });
    }
    return parseHostedReview(body, id, operation.generation);
  }, signal);
}

export async function authorizeHostedPrivateReview(request: HostedPrivateReview): Promise<HostedPrivateReview> {
  if (!HOSTED_REVIEW_ID.test(request.id) || request.catalogStale) throw invalid();
  return withPrivateOperation(async (operation) => {
    if (operation.generation !== request.review.sessionGeneration || request.state !== "pending") throw invalid();
    const response = await privateFetch(`/api/app-installations/private/reviews/${encodeURIComponent(request.id)}/authorize`, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify({ digest: request.review.digest, zip_sha256: request.zipSha256 }),
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    const code = refusalCode(body);
    const reconcile = response.status === 409 &&
      (code === "review_closed" || code === "installation_stale" || code === "app_exists");
    const refusal = () => new ApiError(errorMessage(body, "The authorization did not finish."), response.status, {
      code,
      requestId: errorRequestId(body, response),
    });
    if (!response.ok && !reconcile) throw refusal();
    const receipt = reconcile ? await getHostedPrivateReview(request.id) : parseHostedReview(body, request.id, operation.generation);
    operation.assertCurrent();
    if (receipt.review.digest !== request.review.digest || receipt.zipSha256 !== request.zipSha256) throw invalid();
    if (receipt.state === "pending" && reconcile && code !== "review_closed") throw refusal();
    return receipt;
  });
}

export async function decideHostedPrivateReview(request: HostedPrivateReview, decision: "install" | "reject"): Promise<HostedPrivateReview> {
  if (!HOSTED_REVIEW_ID.test(request.id)) throw invalid();
  if (decision === "install" && request.catalogStale) throw invalid();
  return withPrivateOperation(async (operation) => {
    if (operation.generation !== request.review.sessionGeneration || request.state !== "pending") throw invalid();
    const response = await privateFetch(`/api/app-installations/private/reviews/${encodeURIComponent(request.id)}/${decision}`, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify(decision === "install" ? { digest: request.review.digest, zip_sha256: request.zipSha256 } : {}),
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    const code = refusalCode(body);
    const reconcile = response.status === 409 &&
      (code === "review_closed" || code === "installation_stale" || code === "app_exists");
    const refusal = () => new ApiError(errorMessage(body, "The decision did not finish."), response.status, {
      code,
      requestId: errorRequestId(body, response),
    });
    if (!response.ok && !reconcile) throw refusal();
    if (decision === "install" && response.status === 201) {
      if (!object(body) || body.committed !== true || body.source_kind !== "private" ||
          body.digest !== request.review.digest || body.name !== request.review.name ||
          body.version !== request.review.version || typeof body.install_id !== "string" || !body.install_id ||
          (request.review.target && body.install_id !== request.review.target.installId)) throw invalid();
      return { ...request, state: "installed", installedInstallId: body.install_id };
    }
    // A competing decision can close this review after the initial check
    // but before activation. Read its receipt once, never replay the POST.
    const receipt = reconcile ? await getHostedPrivateReview(request.id) : parseHostedReview(body, request.id, operation.generation);
    operation.assertCurrent();
    if (receipt.review.digest !== request.review.digest || receipt.zipSha256 !== request.zipSha256) throw invalid();
    if (receipt.state === "pending" && reconcile && code !== "review_closed") throw refusal();
    if (receipt.state === "pending" || (!reconcile && decision === "install" && receipt.state !== "installed")) throw invalid();
    return receipt;
  });
}

function parseRoutinePolicy(value: unknown, expectedInstallId: string): PrivateAppRoutinePolicy | null {
  if (value === null || value === undefined) return null;
  if (!object(value) || typeof value.id !== "string" || !value.id ||
      value.install_id !== expectedInstallId ||
      typeof value.agent !== "string" || !AGENT_ACTOR.test(value.agent) ||
      (value.source_kind !== "github" && value.source_kind !== "staged_zip") ||
      typeof value.repository_id !== "number" || !Number.isInteger(value.repository_id) || value.repository_id < 0 ||
      typeof value.owner !== "string" || !value.owner ||
      typeof value.expires_at !== "number" || !Number.isFinite(value.expires_at) || value.expires_at <= 0 ||
      typeof value.installed_digest !== "string" || !DIGEST.test(value.installed_digest)) {
    throw invalid();
  }
  return {
    id: value.id,
    install_id: value.install_id,
    agent: value.agent,
    source_kind: value.source_kind,
    expires_at: value.expires_at,
    installed_digest: value.installed_digest,
  };
}

export async function readPrivateAppUpdatePolicy(
  installId: string,
  signal?: AbortSignal,
): Promise<{ supported: boolean; policy: PrivateAppRoutinePolicy | null }> {
  if (!INSTALL_ID.test(installId)) throw invalid();
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(`/api/app-installations/${encodeURIComponent(installId)}/private/update-policy`, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify({ operation: "read", install_id: installId }),
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (response.status === 404 || response.status === 501) {
      return { supported: false, policy: null };
    }
    if (!response.ok) {
      throw new ApiError(errorMessage(body, "Update automation settings are unavailable."), response.status, {
        code: refusalCode(body),
        requestId: errorRequestId(body, response),
      });
    }
    if (!object(body) || body.ok !== true || !object(body.data)) throw invalid();
    return { supported: true, policy: parseRoutinePolicy(body.data.policy, installId) };
  }, signal);
}

export async function grantPrivateAppUpdatePolicy(input: {
  installId: string;
  expectedDigest: string;
  agent: string;
  source?: "github" | "staged_zip";
  repositoryId?: number;
  expiresAt: number;
}): Promise<PrivateAppRoutinePolicy> {
  const source = input.source ?? "staged_zip";
  if (!INSTALL_ID.test(input.installId) || !DIGEST.test(input.expectedDigest) ||
      !AGENT_ACTOR.test(input.agent) || !Number.isInteger(input.expiresAt) || input.expiresAt <= 0 ||
      (source === "github" && (!Number.isInteger(input.repositoryId) || (input.repositoryId ?? 0) <= 0))) {
    throw invalid();
  }
  return withPrivateOperation(async (operation) => {
    const payload = source === "github"
      ? {
          operation: "grant",
          install_id: input.installId,
          expected_digest: input.expectedDigest,
          agent: input.agent,
          source: "github",
          repository_id: input.repositoryId,
          expires_at: input.expiresAt,
        }
      : {
          operation: "grant",
          install_id: input.installId,
          expected_digest: input.expectedDigest,
          agent: input.agent,
          source: "staged_zip",
          expires_at: input.expiresAt,
        };
    const response = await privateFetch(`/api/app-installations/${encodeURIComponent(input.installId)}/private/update-policy`, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify(payload),
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) {
      throw new ApiError(errorMessage(body, "Routine update permission could not be saved."), response.status, {
        code: refusalCode(body),
        requestId: errorRequestId(body, response),
      });
    }
    if (!object(body) || body.ok !== true || !object(body.data)) throw invalid();
    const policy = parseRoutinePolicy(body.data.policy, input.installId);
    if (!policy) throw invalid();
    return policy;
  });
}

export async function revokePrivateAppUpdatePolicy(installId: string, policyId: string): Promise<void> {
  if (!INSTALL_ID.test(installId) || !/^[A-Za-z0-9_-]{1,128}$/.test(policyId)) throw invalid();
  await withPrivateOperation(async (operation) => {
    const response = await privateFetch(`/api/app-installations/${encodeURIComponent(installId)}/private/update-policy`, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify({ operation: "revoke", install_id: installId, policy_id: policyId }),
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) {
      throw new ApiError(errorMessage(body, "Routine update permission could not be revoked."), response.status, {
        code: refusalCode(body),
        requestId: errorRequestId(body, response),
      });
    }
    if (!object(body) || body.ok !== true || !object(body.data) ||
        body.data.revoked !== true || body.data.install_id !== installId || body.data.policy_id !== policyId) {
      throw invalid();
    }
  });
}

/** The selected File is immutable; the same bytes are sent at review and apply. */
export async function checkPrivateApp(file: File, target: PrivateTarget | null): Promise<PrivateReview> {
  if (target && (!target.installId || !target.name || !DIGEST.test(target.digest) || !target.generation)) throw invalid();
  return withPrivateOperation(async (operation) => {
    const base = target ? `/api/app-installations/${encodeURIComponent(target.installId)}/private` : "/api/app-installations/private";
    const response = await privateFetch(`${base}/check`, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/zip", "X-Cadence-Board": "1", ...sessionHeaders(),
        ...(target ? { "x-cadence-app-current-digest": target.digest, "x-cadence-app-generation": target.generation } : {}) },
      body: file,
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) {
      throw new ApiError(errorMessage(body, "The package check failed."), response.status, {
        code: refusalCode(body),
        requestId: errorRequestId(body, response),
      });
    }
    return parseReview(body, target, operation.generation);
  });
}

/** A response is success only when the acknowledgement and readback both match the review. */
export async function applyPrivateApp(file: File, review: PrivateReview): Promise<Installation> {
  return withPrivateOperation(async (operation) => {
    if (operation.generation !== review.sessionGeneration || !review.compatible || review.warnings) throw invalid();
    const target = review.target;
    const base = target ? `/api/app-installations/${encodeURIComponent(target.installId)}/private` : "/api/app-installations/private";
    const response = await privateFetch(base, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/zip", "X-Cadence-Board": "1", ...sessionHeaders(),
        "x-cadence-app-digest": review.digest, "x-cadence-app-generation": review.generation,
        ...(target ? { "x-cadence-app-current-digest": target.digest } : {}) },
      body: file,
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) {
      throw new ApiError(errorMessage(body, "The installation did not finish."), response.status, {
        code: refusalCode(body),
        requestId: errorRequestId(body, response),
      });
    }
    try {
      if (!object(body) || body.committed !== true || body.source_kind !== "private" || body.digest !== review.digest ||
          body.name !== review.name || body.version !== review.version || typeof body.install_id !== "string" || !body.install_id ||
          (target !== null && body.install_id !== target.installId)) throw invalid();
      const installed = await workspaceApps.detail(body.install_id);
      operation.assertCurrent();
      if (installed.source_kind !== "private" || installed.digest !== review.digest || installed.name !== review.name ||
          installed.version !== review.version || installed.install_id !== body.install_id) throw invalid();
      return installed;
    } catch {
      // The POST succeeded; a missing or malformed readback cannot prove it did not commit.
      throw new ApiError("The installation outcome could not be confirmed.", 502);
    }
  });
}
