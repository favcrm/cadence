import { ApiError, errorMessage } from "../../lib/apiError";
import { privateFetch, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import { workspaceApps, type AccessDiff, type Installation } from "./workspaceApps";

const DIGEST = /^sha256:[0-9a-f]{64}$/;
const ZIP_LIMIT = 8 * 1024 * 1024;
const GITHUB_SOURCE = "/api/app-installations/private/github";

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
      throw new ApiError(errorMessage(body, "The GitHub package could not be fetched."), response.status, { code: refusalCode(body) });
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
  };
}

export type HostedPrivateReview = {
  id: string;
  state: "pending" | "installed" | "rejected" | "expired";
  expiresAt: number;
  source: string;
  zipSha256: string;
  review: PrivateReview;
};

function parseHostedReview(body: unknown, id: string, generation: number): HostedPrivateReview {
  if (!object(body) || body.ok !== true || !object(body.data)) throw invalid();
  const data = body.data;
  if (data.id !== id || !["pending", "installed", "rejected", "expired"].includes(String(data.state)) ||
      typeof data.expires_at !== "number" || !Number.isFinite(data.expires_at) ||
      typeof data.source !== "string" || typeof data.zip_sha256 !== "string" ||
      !/^(?:sha256:)?[0-9a-f]{64}$/.test(data.zip_sha256) || !object(data.review)) throw invalid();
  const raw = data.review;
  let target: PrivateTarget | null = null;
  if (data.install_id != null) {
    if (typeof data.install_id !== "string" || typeof data.expected_digest !== "string" ||
        typeof raw.expected_generation !== "string" || typeof data.name !== "string") throw invalid();
    target = { installId: data.install_id, name: data.name, digest: data.expected_digest, generation: raw.expected_generation };
  }
  const review = parseReview(raw, target, generation);
  if (data.name !== review.name || data.digest !== review.digest || data.generation !== review.generation) throw invalid();
  return { id, state: data.state as HostedPrivateReview["state"], expiresAt: data.expires_at,
    source: data.source, zipSha256: data.zip_sha256, review };
}

export async function getHostedPrivateReview(id: string, signal?: AbortSignal): Promise<HostedPrivateReview> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(`/api/app-installations/private/reviews/${encodeURIComponent(id)}`, {
      credentials: "same-origin", cache: "no-store", headers: { "X-Cadence-Board": "1", ...sessionHeaders() },
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw new ApiError(errorMessage(body, "The review is unavailable."), response.status);
    return parseHostedReview(body, id, operation.generation);
  }, signal);
}

export async function decideHostedPrivateReview(request: HostedPrivateReview, decision: "install" | "reject"): Promise<HostedPrivateReview> {
  return withPrivateOperation(async (operation) => {
    if (operation.generation !== request.review.sessionGeneration || request.state !== "pending") throw invalid();
    const response = await privateFetch(`/api/app-installations/private/reviews/${encodeURIComponent(request.id)}/${decision}`, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify(decision === "install" ? { digest: request.review.digest, zip_sha256: request.zipSha256 } : {}),
    }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw new ApiError(errorMessage(body, "The decision did not finish."), response.status);
    if (decision === "install" && response.status === 201) {
      if (!object(body) || body.committed !== true || body.source_kind !== "private" ||
          body.digest !== request.review.digest || body.name !== request.review.name ||
          body.version !== request.review.version || typeof body.install_id !== "string" || !body.install_id ||
          (request.review.target && body.install_id !== request.review.target.installId)) throw invalid();
      return { ...request, state: "installed" };
    }
    const receipt = parseHostedReview(body, request.id, operation.generation);
    if (receipt.state !== (decision === "install" ? "installed" : "rejected") ||
        receipt.review.digest !== request.review.digest || receipt.zipSha256 !== request.zipSha256) throw invalid();
    return receipt;
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
    if (!response.ok) throw new ApiError(errorMessage(body, "The package check failed."), response.status, { code: refusalCode(body) });
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
    if (!response.ok) throw new ApiError(errorMessage(body, "The installation did not finish."), response.status);
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
