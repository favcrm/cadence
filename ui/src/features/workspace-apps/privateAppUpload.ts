import { ApiError, errorMessage } from "../../lib/apiError";
import { privateFetch, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import { workspaceApps, type AccessDiff, type Installation } from "./workspaceApps";

const DIGEST = /^sha256:[0-9a-f]{64}$/;

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
  return object(value) && value.schema === 1 && typeof value.declared === "boolean" &&
    typeof value.ok === "boolean" && typeof value.compatible === "boolean" &&
    value.ok === value.compatible && Array.isArray(value.checks) &&
    value.checks.every((check) => object(check) && typeof check.ok === "boolean" &&
      (check.kind === "core"
        ? typeof check.required === "string" && typeof check.host === "string"
        : check.kind === "contract" && typeof check.contract === "string" &&
          Array.isArray(check.required) && check.required.every(Number.isInteger) &&
          Array.isArray(check.supported) && check.supported.every(Number.isInteger)));
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
