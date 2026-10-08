import { ApiError } from "../../lib/api";
import { sessionHeaders } from "../../lib/sessionKey";

/** CAD-787 product client for the publish decision surface.
 *
 * Binds the approved preview flow to the CAD-771/AOS-94 exact-destination
 * surface (landed staging 3d6ced85, UI-binding confirmation from 771): the
 * board relays operator-only HTTP under `/api/social-publishes` to the
 * daemon `social_publish_*` RPC. Schedule validates synchronously and the
 * approval IS the schedule-with-approval-identity; recheck plus material
 * re-proof run at claim (daemon-side, never a UI call). There is no
 * preflight RPC — this client never calls one. Handle-to-id mapping is
 * backend discovery-owned: the UI displays the pilot pair, never infers it.
 *
 * The relay does not exist yet (771 backend unmerged) — this module states
 * the exact HTTP shape the relay must serve so UI, tests and the relay
 * converge. All calls are same-origin `/api/` paths; nothing here can
 * reach an external provider. No live post in any test: tests stub fetch.
 */

/** Operator-facing dispatch states — exactly the store rows
 *  (queued/cancelled/processing/posted/refused/held). There is no
 *  pending, no uncertain and no held-reconnect state. `send_disabled` is
 *  a refusal code, not a state.
 *
 * Uncertain READS as processing-or-held (never stored): a processing
 * intent with a lost response is unconfirmed — reconcile via the status
 * query before any retry, never a second provider call; a recheck
 * mismatch stores held, which needs a human. The binding pins
 * digest-form and the raw caption re-resolves from the run at dispatch.
 */
export type PublishState =
  | "queued"
  | "processing"
  | "posted"
  | "refused"
  | "cancelled"
  | "held";

export interface PublishRefusal {
  code: string;
  message: string;
}

/** Backend envelope as the relay returns it: the store envelope passed
 *  through untouched, plus relay-joined writer/reviewer (null when the
 *  run is unresolvable — the intent still serves). */
interface BackendIntent {
  intent_id: string;
  request: string;
  state: string;
  frozen: Record<string, unknown>;
  frozen_digest: string;
  receipt: unknown;
  upstream: unknown;
  claim_after_epoch?: number;
  claim_armed?: boolean;
  writer?: unknown;
  reviewer?: unknown;
}

export interface PublishIntent {
  intent_id: string;
  install_id: string;
  context_id: string | null;
  run_id: string;
  effect_id: string;
  /** The reviewed artifact whose text the intent froze. Absent or null
   *  (a legacy explicit-digest intent): send-now cannot confirm it. */
  artifact_id?: string | null;
  state: PublishState;
  /** Channel is the request toolkit, echoed back. */
  channel: "instagram" | "facebook";
  /** Exact operator-selected destination id, echoed by the relay only.
   *  The handle is NEVER relay-vouched (id mapping is backend
   *  discovery-owned): the UI renders its PILOT_DESTINATION display
   *  constant and never infers a destination from any handle. */
  destination_id: string;
  caption_digest: string;
  image_digest: string | null;
  /** The exact-binding digest. No separate destination_digest exists. */
  frozen_digest: string;
  /** The schedule request key, echoed as the idempotency material. */
  idempotency_key: string;
  due_epoch: number;
  /** Server-owned post-queue undo maturity; never derived from AOS not_before. */
  claim_after_epoch?: number;
  /** False only while an owner attachment awaits its post-commit arm. */
  claim_armed?: boolean;
  timezone: string;
  grant_id: string;
  approval_id: string;
  writer: string | null;
  reviewer: string | null;
  permalink: string | null;
  receipt: unknown | null;
  refusal: PublishRefusal | null;
  /** Upstream dispatch evidence, passed through. Present on a processing
   *  intent means dispatch went out with no reported outcome — the
   *  uncertain reading. Never a stored state. */
  upstream: unknown | null;
}

/** The daemon caps `GET /api/social-publishes` at this many rows (ORDER BY
 *  intent_id). One constant for every reader of that cap. */
export const PUBLISH_LIST_CAP = 100;

const STATES = ["queued", "processing", "posted", "refused", "cancelled", "held"] as const;

function str(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

/** Fail-closed envelope mapping: anything off-shape throws before render. */
export function toPublishIntent(envelope: BackendIntent): PublishIntent {
  const frozen = envelope.frozen;
  const state = envelope.state;
  const channel = frozen.toolkit;
  const dueEpoch = frozen.due_epoch;
  const claimAfterEpoch = envelope.claim_after_epoch ?? 0;
  const claimArmed = envelope.claim_armed ?? true;
  if (!(STATES as readonly string[]).includes(state))
    throw new ApiError(`unknown publish state: ${state}`, 502);
  if (channel !== "instagram" && channel !== "facebook")
    throw new ApiError(`unknown publish channel: ${String(channel)}`, 502);
  if (!Number.isInteger(dueEpoch))
    throw new ApiError("publish due time is not an integer epoch", 502);
  if (!Number.isSafeInteger(claimAfterEpoch) || claimAfterEpoch < 0 || typeof claimArmed !== "boolean")
    throw new ApiError("publish claim maturity is not a valid epoch", 502);
  const get = (key: string) => str(frozen[key]);
  const required = ["install_id", "run_id", "effect_id", "destination_id", "caption_digest", "grant_id", "approval_id", "timezone"] as const;
  for (const key of required) {
    if (get(key) === null) throw new ApiError(`publish intent misses ${key}`, 502);
  }
  const receipt = (envelope.receipt ?? null) as Record<string, unknown> | null;
  const refusal: PublishRefusal | null =
    state === "refused" && receipt && typeof receipt.error === "string"
      ? { code: typeof receipt.code === "string" ? receipt.code : "", message: receipt.error }
      : state === "held" && receipt && typeof receipt.reason === "string"
        ? { code: "", message: receipt.reason }
        : null;
  return {
    intent_id: envelope.intent_id,
    install_id: get("install_id") as string,
    context_id: get("context_id"),
    run_id: get("run_id") as string,
    effect_id: get("effect_id") as string,
    artifact_id: get("artifact_id"),
    state: state as PublishState,
    channel,
    destination_id: get("destination_id") as string,
    caption_digest: get("caption_digest") as string,
    image_digest: get("image_digest") ?? null,
    frozen_digest: envelope.frozen_digest,
    idempotency_key: envelope.request,
    due_epoch: dueEpoch as number,
    claim_after_epoch: claimAfterEpoch as number,
    claim_armed: claimArmed,
    timezone: get("timezone") as string,
    grant_id: get("grant_id") as string,
    approval_id: get("approval_id") as string,
    writer: str(envelope.writer),
    reviewer: str(envelope.reviewer),
    permalink:
      state === "posted" && receipt && typeof receipt.permalink === "string"
        ? receipt.permalink
        : null,
    receipt,
    refusal,
    upstream: (envelope.upstream ?? null) as unknown | null,
  };
}

/** Artifact-freeze schedule body: digests derive server-side from the
 *  approved run's reviewed material (771 FreezeFromArtifact). The UI never
 *  invents a digest. Post now is `due_epoch` at now; Schedule is a future
 *  `due_epoch` with an explicit timezone. */
export interface SchedulePublishBody {
  request_id: string;
  install_id: string;
  context_id?: string;
  run_id: string;
  effect_id: string;
  artifact_id: string;
  bundle_digest: string;
  slot: "publication";
  destination_id: string;
  toolkit: "instagram" | "facebook";
  media_key?: string;
  grant_id: string;
  approval_id: string;
  due_epoch: number;
  timezone: string;
}

/** CAD-1143 prepare body: only the host-minted id and selected run/mode;
 *  scope, account, grant and immutable publication material derive server-side. */
export interface PublishIntentPrepareBody {
  request_id: string;
  run_id: string;
  mode: "now" | "schedule";
  due_epoch?: number;
}

export interface PreparedOwnerIntent {
  prepared_id: string;
  request: string;
  state: "prepared" | "authorized";
  descriptor_digest: string;
  install_id: string;
  context_id: string | null;
  run_id: string;
  mode: "now" | "schedule";
  due_epoch: number;
  not_before_epoch: number;
  owner_intent: {
    version: "social-owner-intent.v1";
    intent_id: string;
    intent_digest: string;
    expires_at: number;
  };
  owner_action_url: string;
}

export interface PublishIntentAttachBody {
  prepared_id: string;
  install_id: string;
  context_id?: string;
}

export interface PreparedOwnerScopeBody {
  prepared_id: string;
  install_id: string;
  context_id: string | null;
}

export interface AttachedPublishIntent {
  prepared_id: string;
  queued: {
    intent_id: string;
    request: string;
    state: PublishState;
    due_epoch: number;
    claim_after_epoch: number;
    claim_armed: true;
  };
}

export type OwnerCompletionStatus = "ready" | "pending" | "unknown" | "refused";
export type PreparedOwnerLifecycle = "prepared" | "authorized" | "cancelled" | "superseded" | "refused";
export interface PreparedOwnerStatus {
  prepared_id: string;
  install_id: string;
  context_id: string | null;
  state: PreparedOwnerLifecycle;
  owner_status: OwnerCompletionStatus;
  queued: null | {
    intent_id: string;
    state: PublishState;
    due_epoch: number;
    claim_armed: boolean;
    claim_after_epoch: number | null;
  };
}

export interface PreparedOwnerCancellation {
  prepared_id: string;
  install_id: string;
  context_id: string | null;
  state: "cancelled";
  queued: PreparedOwnerStatus["queued"];
}

/** CAD-1123 HP4 reschedule body: the intent's own scope, the time the
 *  operator saw and the new time (compare-and-swap on the queued intent). */
export interface PublishRescheduleBody {
  install_id: string;
  context_id?: string;
  expected_due_epoch: number;
  due_epoch: number;
}

/** Pilot destination identity (771 revalidated): operator-named Instagram
 *  professional account. The source receipt handle (@juicysuite_crm) is a
 *  different role and must never appear here. */
export const PILOT_DESTINATION = {
  channel: "instagram",
  handle: "@sakeboyhk",
  account_id: "17841400008460056",
} as const;

export const PUBLISH_TIMEZONE = "Asia/Hong_Kong";

const paths = {
  list: (installId: string, contextId: string | null) =>
    `/api/social-publishes?install_id=${encodeURIComponent(installId)}${contextId ? `&context_id=${encodeURIComponent(contextId)}` : ""}`,
  show: (intentId: string) =>
    `/api/social-publishes/${encodeURIComponent(intentId)}`,
  schedule: () => "/api/social-publishes",
  cancel: (intentId: string) =>
    `/api/social-publishes/${encodeURIComponent(intentId)}/cancel`,
  importMedia: () => "/api/social-media-imports",
  prepareIntent: () => "/api/social-publish-intents/prepare",
  attachIntent: () => "/api/social-publish-intents/attach",
  statusIntent: () => "/api/social-publish-intents/status",
  cancelPreparedIntent: () => "/api/social-publish-intents/cancel",
  sendNow: (intentId: string) =>
    `/api/social-publishes/${encodeURIComponent(intentId)}/send-now`,
};

/** CAD-979 media import body: the approved run's provenance + scope only —
 *  never caller bytes, path or URL. The daemon reads the reviewed retained
 *  asset by receipt custody and returns the minted `media_key`. */
export interface MediaImportBody {
  request_id: string;
  install_id: string;
  context_id?: string;
  run_id: string;
  artifact_id: string;
  bundle_digest: string;
  slot: "publication";
  toolkit: "instagram" | "facebook";
  destination_id: string;
}

export interface MediaImportReply {
  media_key: string;
  image_digest: string;
}

function sameOrigin(path: string): string {
  if (!path.startsWith("/api/"))
    throw new Error("Publish client is same-origin only.");
  return path;
}

/** Body keys the relay accepts per operation (mirrors the hostActions
guard pattern): anything else is forged and never serializes. Schedule
carries the artifact-freeze grammar only — digests derive server-side.
Queued cancel names its exact install/context; prepared status/cancel always
carry all three exact scope selectors. */
const SCHEDULE_KEYS = [
  "request_id", "install_id", "context_id", "run_id", "effect_id",
  "artifact_id", "bundle_digest", "slot", "destination_id", "toolkit",
  "media_key", "grant_id", "approval_id", "due_epoch", "timezone",
] as const;
const PUBLISH_INTENT_PREPARE_KEYS = ["request_id", "run_id", "mode", "due_epoch"] as const;
const PUBLISH_INTENT_ATTACH_KEYS = ["prepared_id", "install_id", "context_id"] as const;
const PUBLISH_INTENT_SCOPE_KEYS = ["prepared_id", "install_id", "context_id"] as const;
/** CAD-1123 HP4: the reschedule grammar mirrors the relay's `Reschedule`
 *  struct exactly — the intent's own scope plus the two epochs. */
const PUBLISH_RESCHEDULE_KEYS = ["install_id", "context_id", "expected_due_epoch", "due_epoch"] as const;
const CANCEL_KEYS = ["install_id", "context_id"] as const;
const MEDIA_IMPORT_KEYS = [
  "request_id", "install_id", "context_id", "run_id", "artifact_id",
  "bundle_digest", "slot", "toolkit", "destination_id",
] as const;

function assertCleanBody(body: Record<string, unknown>, allowed: readonly string[]): void {
  for (const key of Object.keys(body)) {
    if (!allowed.includes(key)) throw new ApiError(`refused publish field: ${key}`, 400);
  }
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function validOwnerActionUrl(
  value: string,
  preparedId: string,
  intentDigest: string,
): boolean {
  try {
    const rawAuthority = /^https:\/\/([^/?#]+)/.exec(value)?.[1];
    if (!rawAuthority || rawAuthority.includes(":") || rawAuthority.includes("@")) return false;
    const url = new URL(value);
    const keys: string[] = [];
    url.searchParams.forEach((_value, key) => keys.push(key));
    const slug = url.searchParams.get("company_slug") ?? "";
    return url.protocol === "https:" &&
      url.username === "" && url.password === "" && url.pathname === "/social-owner-action" &&
      url.hash === "" && keys.length === 3 && new Set(keys).size === 3 &&
      ["company_slug", "intent_id", "intent_digest"].every(key => url.searchParams.getAll(key).length === 1) &&
      /^[a-z0-9](?:[a-z0-9-]{0,40}[a-z0-9])?$/.test(slug) && !slug.endsWith("-staging") &&
      url.searchParams.get("intent_id") === preparedId &&
      url.searchParams.get("intent_digest") === intentDigest;
  } catch {
    return false;
  }
}

function toPreparedOwnerIntent(
  value: unknown,
  expected: { installId: string; contextId: string | null; runId: string; mode: "now" | "schedule" },
): PreparedOwnerIntent {
  const root = record(value) ? value : null;
  const envelope = root && record(root.prepared) ? root.prepared : null;
  const owner = envelope && record(envelope.owner_intent) ? envelope.owner_intent : null;
  const descriptor = envelope && record(envelope.descriptor) ? envelope.descriptor : null;
  const preparedId = envelope ? str(envelope.prepared_id) : null;
  const request = envelope ? str(envelope.request) : null;
  const state = envelope ? str(envelope.state) : null;
  const descriptorDigest = envelope ? str(envelope.descriptor_digest) : null;
  const intentId = owner ? str(owner.intent_id) : null;
  const intentDigest = owner ? str(owner.intent_digest) : null;
  const expiresAt = owner?.expires_at;
  const ownerActionUrl = root ? str(root.owner_action_url) : null;
  const installId = descriptor ? str(descriptor.install_id) : null;
  const contextId = descriptor && Object.prototype.hasOwnProperty.call(descriptor, "context_id")
    ? (descriptor.context_id === null ? null : str(descriptor.context_id)) : undefined;
  const runId = descriptor ? str(descriptor.run_id) : null;
  const mode = descriptor ? str(descriptor.mode) : null;
  const dueEpoch = descriptor?.due_epoch;
  const notBeforeEpoch = descriptor?.not_before_epoch;
  const expiresEpoch = descriptor?.expires_epoch;
  if (!preparedId || preparedId !== intentId || !/^sprep-[0-9a-f]{32}$/.test(preparedId) ||
      !request || !/^[A-Za-z0-9_-]{8,128}$/.test(request) ||
      (state !== "prepared" && state !== "authorized") ||
      !descriptorDigest || !/^[a-f0-9]{64}$/.test(descriptorDigest) ||
      installId !== expected.installId || contextId !== expected.contextId ||
      runId !== expected.runId || mode !== expected.mode ||
      !Number.isSafeInteger(dueEpoch) || (dueEpoch as number) <= 0 ||
      notBeforeEpoch !== dueEpoch || !Number.isSafeInteger(expiresEpoch) ||
      (expiresEpoch as number) < (dueEpoch as number) || expiresAt !== expiresEpoch ||
      !owner || owner.version !== "social-owner-intent.v1" || !intentDigest ||
      !/^[a-f0-9]{64}$/.test(intentDigest) || !Number.isSafeInteger(expiresAt) ||
      (expiresAt as number) < 0 || !ownerActionUrl ||
      !validOwnerActionUrl(ownerActionUrl, preparedId, intentDigest))
    throw new ApiError("The server returned an invalid prepared owner intent", 502);
  return {
    prepared_id: preparedId, request, state, descriptor_digest: descriptorDigest,
    install_id: expected.installId,
    context_id: expected.contextId,
    run_id: expected.runId,
    mode: expected.mode,
    due_epoch: dueEpoch as number,
    not_before_epoch: notBeforeEpoch as number,
    owner_intent: {
      version: "social-owner-intent.v1", intent_id: intentId,
      intent_digest: intentDigest, expires_at: expiresAt as number,
    },
    owner_action_url: ownerActionUrl,
  };
}

function toAttachedPublishIntent(value: unknown, expected: PreparedOwnerIntent): AttachedPublishIntent {
  const envelope = record(value) ? value : null;
  const prepared = envelope && record(envelope.prepared) ? envelope.prepared : null;
  const queued = envelope && record(envelope.queued) ? envelope.queued : null;
  const returnedId = prepared ? str(prepared.prepared_id) : null;
  const intentId = queued ? str(queued.intent_id) : null;
  const requestId = queued ? str(queued.request) : null;
  const state = queued ? str(queued.state) : null;
  const dueEpoch = queued?.due_epoch;
  const claimAfterEpoch = queued?.claim_after_epoch;
  const claimArmed = queued?.claim_armed;
  const descriptorDigest = prepared ? str(prepared.descriptor_digest) : null;
  const descriptor = prepared && record(prepared.descriptor) ? prepared.descriptor : null;
  const installId = descriptor ? str(descriptor.install_id) : null;
  const contextId = descriptor && Object.prototype.hasOwnProperty.call(descriptor, "context_id")
    ? (descriptor.context_id === null ? null : str(descriptor.context_id)) : undefined;
  const runId = descriptor ? str(descriptor.run_id) : null;
  const mode = descriptor ? str(descriptor.mode) : null;
  const descriptorDueEpoch = descriptor?.due_epoch;
  const descriptorNotBeforeEpoch = descriptor?.not_before_epoch;
  if (!returnedId || returnedId !== expected.prepared_id ||
      !intentId || !/^spub-[0-9a-f]{32}$/.test(intentId) ||
      !requestId || requestId !== expected.request ||
      !prepared || prepared.state !== "authorized" || prepared.request !== expected.request ||
      descriptorDigest !== expected.descriptor_digest ||
      installId !== expected.install_id || contextId !== expected.context_id ||
      runId !== expected.run_id || mode !== expected.mode ||
      descriptorDueEpoch !== expected.due_epoch ||
      descriptorNotBeforeEpoch !== expected.not_before_epoch ||
      !(STATES as readonly string[]).includes(String(state)) ||
      !Number.isSafeInteger(dueEpoch) || dueEpoch !== expected.due_epoch || (dueEpoch as number) <= 0 ||
      !Number.isSafeInteger(claimAfterEpoch) ||
      (claimAfterEpoch as number) <= 0 || claimArmed !== true)
    throw new ApiError("The server returned an invalid owner intent attachment", 502);
  return {
    prepared_id: returnedId,
    queued: {
      intent_id: intentId, request: requestId, state: state as PublishState,
      due_epoch: dueEpoch as number, claim_after_epoch: claimAfterEpoch as number,
      claim_armed: true,
    },
  };
}

function toPreparedOwnerStatus(
  value: unknown,
  expected: { preparedId: string; installId: string; contextId: string | null },
): PreparedOwnerStatus {
  const root = record(value) ? value : null;
  if (!root || !exactRecordKeys(root, [
    "prepared_id", "install_id", "context_id", "state", "owner_status", "queued",
  ])) throw new ApiError("The server returned an invalid owner intent status", 502);
  const preparedId = str(root.prepared_id);
  const installId = str(root.install_id);
  if (root.context_id !== null && typeof root.context_id !== "string")
    throw new ApiError("The server returned an invalid owner intent scope", 502);
  const contextId = root.context_id === null ? null : root.context_id as string;
  const state = str(root.state);
  const ownerStatus = str(root.owner_status);
  if (preparedId !== expected.preparedId || installId !== expected.installId ||
      contextId !== expected.contextId ||
      !["prepared", "authorized", "cancelled", "superseded", "refused"].includes(String(state)) ||
      !["ready", "pending", "unknown", "refused"].includes(String(ownerStatus)))
    throw new ApiError("The server returned an invalid owner intent status", 502);

  let queued: PreparedOwnerStatus["queued"] = null;
  if (root.queued !== null) {
    const row = record(root.queued) ? root.queued : null;
    if (!row || !exactRecordKeys(row, ["intent_id", "state", "due_epoch", "claim_armed", "claim_after_epoch"]))
      throw new ApiError("The server returned an invalid owner queue status", 502);
    const intentId = str(row.intent_id);
    const queueState = str(row.state);
    const dueEpoch = row.due_epoch;
    const claimArmed = row.claim_armed;
    const claimAfter = row.claim_after_epoch;
    if (!intentId || !/^spub-[0-9a-f]{32}$/.test(intentId) ||
        !(STATES as readonly string[]).includes(String(queueState)) ||
        !Number.isSafeInteger(dueEpoch) || (dueEpoch as number) <= 0 ||
        typeof claimArmed !== "boolean" ||
        (claimArmed ? !Number.isSafeInteger(claimAfter) || (claimAfter as number) <= 0 : claimAfter !== null))
      throw new ApiError("The server returned an invalid owner queue status", 502);
    queued = {
      intent_id: intentId,
      state: queueState as PublishState,
      due_epoch: dueEpoch as number,
      claim_armed: claimArmed,
      claim_after_epoch: claimArmed ? claimAfter as number : null,
    };
  }
  if ((state === "prepared" && queued !== null) ||
      (state === "authorized" && queued === null) ||
      ((ownerStatus === "ready" || ownerStatus === "pending") && state !== "prepared") ||
      (state === "cancelled" && queued !== null && queued.state !== "cancelled"))
    throw new ApiError("The server returned an inconsistent owner intent lifecycle", 502);
  return {
    prepared_id: preparedId as string,
    install_id: installId as string,
    context_id: contextId,
    state: state as PreparedOwnerLifecycle,
    owner_status: ownerStatus as OwnerCompletionStatus,
    queued,
  };
}

function toPreparedOwnerCancellation(
  value: unknown,
  expected: { preparedId: string; installId: string; contextId: string | null },
): PreparedOwnerCancellation {
  const root = record(value) ? value : null;
  if (!root || !exactRecordKeys(root, ["prepared_id", "install_id", "context_id", "state", "queued"]))
    throw new ApiError("The server returned an invalid owner intent cancellation", 502);
  const preparedId = str(root.prepared_id);
  const installId = str(root.install_id);
  if (root.context_id !== null && typeof root.context_id !== "string")
    throw new ApiError("The server returned an invalid owner intent cancellation scope", 502);
  const contextId = root.context_id === null ? null : root.context_id as string;
  if (preparedId !== expected.preparedId || installId !== expected.installId ||
      contextId !== expected.contextId || root.state !== "cancelled")
    throw new ApiError("The server returned an invalid owner intent cancellation", 502);

  let queued: PreparedOwnerStatus["queued"] = null;
  if (root.queued !== null) {
    const row = record(root.queued) ? root.queued : null;
    if (!row || !exactRecordKeys(row, ["intent_id", "state", "due_epoch", "claim_armed", "claim_after_epoch"]))
      throw new ApiError("The server returned an invalid cancelled queue status", 502);
    const intentId = str(row.intent_id);
    const dueEpoch = row.due_epoch;
    const claimArmed = row.claim_armed;
    const claimAfter = row.claim_after_epoch;
    if (!intentId || !/^spub-[0-9a-f]{32}$/.test(intentId) || row.state !== "cancelled" ||
        !Number.isSafeInteger(dueEpoch) || (dueEpoch as number) <= 0 ||
        typeof claimArmed !== "boolean" ||
        (claimArmed ? !Number.isSafeInteger(claimAfter) || (claimAfter as number) <= 0 : claimAfter !== null))
      throw new ApiError("The server returned an invalid cancelled queue status", 502);
    queued = {
      intent_id: intentId,
      state: "cancelled",
      due_epoch: dueEpoch as number,
      claim_armed: claimArmed,
      claim_after_epoch: claimArmed ? claimAfter as number : null,
    };
  }
  return {
    prepared_id: preparedId as string,
    install_id: installId as string,
    context_id: contextId,
    state: "cancelled",
    queued,
  };
}

function exactRecordKeys(value: Record<string, unknown>, keys: string[]): boolean {
  return Object.keys(value).sort().join() === [...keys].sort().join();
}

/** Reads are abortable and uncached so receipts never outlive an operator
session; transport mirrors workspaceApps/hostActions (same-origin
credentials, session headers, board marker on writes). */
async function request<T>(path: string, signal?: AbortSignal, body?: Record<string, unknown>, allowed: readonly string[] = SCHEDULE_KEYS): Promise<T> {
  if (body !== undefined) assertCleanBody(body, allowed);
  const response = await fetch(sameOrigin(path), {
    method: body === undefined ? "GET" : "POST", signal,
    credentials: "same-origin", cache: "no-store",
    headers: body === undefined ? sessionHeaders() : {
      "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders(),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  const value = await response.json().catch(() => null);
  if (!response.ok) throw new ApiError(value?.error ?? `${response.status} ${response.statusText}`, response.status);
  if (value === null) throw new ApiError("The server returned an invalid publish receipt", 502);
  return value as T;
}

export const socialPublish = {
  list: async (installId: string, contextId: string | null, signal?: AbortSignal) => {
    const reply = await request<{ intents: BackendIntent[] }>(paths.list(installId, contextId), signal);
    if (!Array.isArray(reply.intents)) throw new ApiError("The server returned an invalid publish list", 502);
    return { intents: reply.intents.map(toPublishIntent) };
  },
  show: async (intentId: string, signal?: AbortSignal) => {
    const reply = await request<{ intent: BackendIntent }>(paths.show(intentId), signal);
    return { intent: toPublishIntent(reply.intent) };
  },
  schedule: async (body: SchedulePublishBody) => {
    const reply = await request<{ intent: BackendIntent }>(paths.schedule(), undefined, body as unknown as Record<string, unknown>);
    return { intent: toPublishIntent(reply.intent) };
  },
  importMedia: async (body: MediaImportBody): Promise<MediaImportReply> => {
    const reply = await request<Record<string, unknown>>(paths.importMedia(), undefined, body as unknown as Record<string, unknown>, MEDIA_IMPORT_KEYS);
    const key = str(reply.media_key), digest = str(reply.image_digest);
    if (!key || !digest) throw new ApiError("The server returned an invalid media import receipt", 502);
    return { media_key: key, image_digest: digest };
  },
  /** CAD-1143: prepare is nondispatchable. The daemon derives all immutable
   *  publication fields, then exposes only the signed-contract selector the
   *  trusted top-level owner portal needs; caller material/grants never pass. */
  prepareIntent: async (
    body: PublishIntentPrepareBody,
    expected: { installId: string; contextId: string | null },
    signal?: AbortSignal,
  ): Promise<PreparedOwnerIntent> => {
    const wire: Record<string, unknown> = { request_id: body.request_id, run_id: body.run_id, mode: body.mode };
    if (body.mode === "schedule") wire.due_epoch = body.due_epoch;
    const reply = await request<unknown>(paths.prepareIntent(), signal, wire, PUBLISH_INTENT_PREPARE_KEYS);
    return toPreparedOwnerIntent(reply, {
      ...expected, runId: body.run_id, mode: body.mode,
    });
  },
  /** CAD-1143: advisory status is an uncached, exact-scope read. A `ready`
   *  result is not attach authority and never carries a receipt/JTI/grant. */
  statusIntent: async (
    body: PreparedOwnerScopeBody,
    expected: { installId: string; contextId: string | null },
    signal?: AbortSignal,
  ): Promise<PreparedOwnerStatus> => {
    const wire: Record<string, unknown> = {
      prepared_id: body.prepared_id, install_id: body.install_id, context_id: body.context_id,
    };
    if (body.install_id !== expected.installId || body.context_id !== expected.contextId)
      throw new ApiError("The owner intent status scope changed", 400);
    const reply = await request<unknown>(paths.statusIntent(), signal, wire, PUBLISH_INTENT_SCOPE_KEYS);
    return toPreparedOwnerStatus(reply, {
      preparedId: body.prepared_id, installId: expected.installId, contextId: expected.contextId,
    });
  },
  /** Cancellation is a separate exact-scope local terminal transition; it
   *  never claims remote AOS deletion or revocation. */
  cancelPreparedIntent: async (
    body: PreparedOwnerScopeBody,
    expected: { installId: string; contextId: string | null },
    signal?: AbortSignal,
  ): Promise<PreparedOwnerCancellation> => {
    const wire: Record<string, unknown> = {
      prepared_id: body.prepared_id, install_id: body.install_id, context_id: body.context_id,
    };
    if (body.install_id !== expected.installId || body.context_id !== expected.contextId)
      throw new ApiError("The owner intent cancel scope changed", 400);
    const reply = await request<unknown>(paths.cancelPreparedIntent(), signal, wire, PUBLISH_INTENT_SCOPE_KEYS);
    return toPreparedOwnerCancellation(reply, {
      preparedId: body.prepared_id, installId: expected.installId, contextId: expected.contextId,
    });
  },
  /** CAD-1143: attach sends only the exact prepared selector and scope. The
   *  daemon obtains/verifies AOS queue validation and atomically attaches. */
  attachIntent: async (
    body: PublishIntentAttachBody,
    expected: PreparedOwnerIntent,
    signal?: AbortSignal,
  ): Promise<AttachedPublishIntent> => {
    const wire: Record<string, unknown> = {
      prepared_id: body.prepared_id, install_id: body.install_id,
      ...(body.context_id ? { context_id: body.context_id } : {}),
    };
    if (body.prepared_id !== expected.prepared_id || body.install_id !== expected.install_id ||
        (body.context_id ?? null) !== expected.context_id)
      throw new ApiError("The owner intent attach scope changed", 400);
    const reply = await request<unknown>(paths.attachIntent(), signal, wire, PUBLISH_INTENT_ATTACH_KEYS);
    return toAttachedPublishIntent(reply, expected);
  },
  /** CAD-1123 HP4: move a queued intent, compare-and-swap on the time the
   *  operator saw. Names the intent's own install and exact context, as
   *  cancel does — the daemon refuses any other scope. */
  reschedule: async (intentId: string, body: PublishRescheduleBody) => {
    const reply = await request<{ intent: BackendIntent }>(`/api/social-publishes/${encodeURIComponent(intentId)}/reschedule`,
      undefined, body as unknown as Record<string, unknown>, PUBLISH_RESCHEDULE_KEYS);
    return { intent: toPublishIntent(reply.intent) };
  },
  /** CAD-1027: cancel names the intent's own install and exact context;
   *  the daemon refuses any other scope. */
  cancel: async (intentId: string, installId: string, contextId: string | null) => {
    const body = { install_id: installId, ...(contextId ? { context_id: contextId } : {}) };
    const reply = await request<{ intent: BackendIntent }>(paths.cancel(intentId), undefined, body, CANCEL_KEYS);
    return { intent: toPublishIntent(reply.intent) };
  },
  /** CAD-1041: the operator's explicit "send this queued intent now".
   *  The daemon claims the named row by identity, stages it, dispatches
   *  once and reconciles through status — one click, one provider call,
   *  never another intent. A refused call leaves the row for a human.
   *  Like cancel, it names the intent's own install and exact context. */
  sendNow: async (intentId: string, installId: string, contextId: string | null) => {
    const body = { install_id: installId, ...(contextId ? { context_id: contextId } : {}) };
    const reply = await request<{ intent: BackendIntent }>(paths.sendNow(intentId), undefined, body, CANCEL_KEYS);
    return { intent: toPublishIntent(reply.intent) };
  },
};

export function publishStateText(state: PublishState): string {
  switch (state) {
    case "queued": return "Queued";
    case "processing": return "Processing";
    case "posted": return "Posted · verified";
    case "refused": return "Refused";
    case "cancelled": return "Cancelled";
    case "held": return "Held · needs human";
  }
}

export function publishStateTone(
  state: PublishState,
): "ok" | "warn" | "fail" | "muted" {
  if (state === "posted") return "ok";
  if (state === "refused") return "fail";
  if (state === "held") return "warn";
  return "muted";
}

/** Derived display flag: uncertain shows ONLY for a processing intent
 *  with upstream dispatch evidence and no reported outcome. The
 *  persisted intent remains processing — no stored enum is invented.
 *  Every other combination shows no uncertain display. */
export function showsUncertainReading(intent: PublishIntent): boolean {
  return intent.state === "processing" && intent.upstream != null;
}

/** Guidance for the two states the uncertain question can mean. Held
 *  needs a human decision (intent held vs provider reconnect_needed are
 *  different layers — the intent never auto-resumes). */
export function reconcileReading(state: PublishState): string | null {
  if (state === "held")
    return "Needs a human decision: the intent is held after a recheck mismatch (provider reconnect_needed is a separate layer). Reconnect, then re-approve — never auto-resume.";
  return null;
}

/** Operator copy for the exact 28-code refusal vocabulary (771
 *  UI-binding confirmation): bad_key/connection/destination/
 *  caption_digest/image_digest/run/effect/grant/intent/revision/timezone,
 *  cancel_closed, cross_workspace, grant_mismatch/binding_mismatch/
 *  revoked/exhausted/window/approval/bounds, image_required, key_conflict,
 *  not_publishable, wrong_connection/destination/toolkit, unknown_key,
 *  send_disabled. Unknown codes stay visible with their raw message.
 *
 *  Transport note (CAD-802): install/context identity always arrives via
 *  props from the route-owned WorkspaceApp — never from a form field —
 *  and the shell observes context selection for chat scope with zero
 *  panel work. Body keys are allowlisted per operation like hostActions.
 *
 * Field mappings (771 binding): idempotency_key comes from the schedule
 * request; channel is the request toolkit; frozen_digest is the
 * exact-binding digest (writer/reviewer resolve via a relay read-only run
 * join); destination_id is echoed only, never handle-vouched.
 */
export function refusalCopy(refusal: PublishRefusal): string {
  switch (refusal.code) {
    case "send_disabled":
      return "Send dispatch is not enabled (refusal code, not a state). The intent validated and changed nothing — no provider was called.";
    case "grant_bounds":
      return "The send grant allows 1 to 10 uses. This grant is outside that bound — nothing was published.";
    case "grant_exhausted":
      return "The send grant has no uses left. Nothing was published.";
    case "grant_revoked":
      return "The send grant was revoked. Nothing was published.";
    case "grant_mismatch":
      return "The grant id does not match this binding. Nothing was published.";
    case "grant_binding_mismatch":
      return "The binding does not match this grant and destination. Nothing was published.";
    case "grant_window":
      return "The send grant is outside its validity window. Nothing was published.";
    case "grant_approval":
      return "The grant names no usable operator approval (empty or over 120 characters). Nothing was published.";
    case "bad_caption_digest":
      return "The caption digest is not 64 lowercase hex. Nothing was published.";
    case "bad_image_digest":
      return "The image digest is not 64 lowercase hex. Nothing was published.";
    case "bad_destination":
    case "wrong_destination":
      return "The destination does not match the discovered owner-authorized account. Nothing was published.";
    case "wrong_connection":
      return "The binding names a different connection than discovery. Nothing was published.";
    case "wrong_toolkit":
      return "The binding toolkit differs from the discovered destination. Nothing was published.";
    case "not_publishable":
      return "The destination is not active, linked and open. Nothing was published.";
    case "bad_key":
      return "The idempotency key shape is invalid. Nothing was stored.";
    case "key_conflict":
      return "The same key carries changed content or destination — it fails instead of sending twice.";
    case "unknown_key":
      return "No intent or ledger row matches that key. Nothing to reconcile.";
    case "cancel_closed":
      return "Only a queued intent can cancel — this one already left queued.";
    case "cross_workspace":
      return "The credential workspace does not match. Cross-workspace sends are refused.";
    case "bad_connection":
    case "bad_grant":
    case "bad_run":
    case "bad_effect":
    case "bad_intent":
    case "bad_revision":
    case "bad_timezone":
      return `An identity shape is invalid (${refusal.code}). Nothing was stored.`;
    case "approval_replay":
      return "This approval already authorized another post. Review again to approve this one — nothing was published.";
    case "bad_approval":
      return "The approval id is not one this panel minted. Review again — nothing was published.";
    case "image_required":
      return "Instagram needs a reviewed provider-accessible image. Nothing was published.";
    default:
      return refusal.message
        ? `${refusal.message} (${refusal.code}). Nothing was published.`
        : `Refused (${refusal.code}). Nothing was published.`;
  }
}

/** Daemon refusals arrive as `code: detail`; a known Cadence code gets its
 *  operator copy, anything else stays raw. */
export function refusalFromError(text: string): string {
  const match = /^([a-z_]+): (.+)$/s.exec(text);
  return match ? refusalCopy({ code: match[1], message: match[2] }) : text;
}

/** AgenticOS send-grant id shape, mirroring `valid_grant_id` in
 *  publish.rs: `dpq_` then 8–64 ASCII letters, digits, `_` or `-`. The
 *  daemon re-validates; this only keeps a malformed id from scheduling. */
export function isGrantIdUsable(grantId: string): boolean {
  return /^dpq_[A-Za-z0-9_-]{8,64}$/.test(grantId);
}

/** CAD-1027: one operator confirmation mints one approval identity,
 *  `apv-` + 32 random hex (also the schedule request id, so a retry or a
 *  double submit of the same confirmation replays the same intent). The
 *  daemon lets an approval authorize exactly one intent: reusing it under
 *  any other request refuses `approval_replay`. `getRandomValues` works on
 *  a plain-http board, where `randomUUID` does not. */
export function mintApprovalId(): string {
  return `apv-${randomHex()}`;
}

/** 32 random lowercase hex characters. */
export function randomHex(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** CAD-1027: an import is usable only when it carries exactly the reviewed
 *  image — the receipt's `sha256:<hex>` asset digest — and its key binds
 *  that digest (`dp1.<ws>.<connection>.<digest[..32]>`). Anything else is
 *  refused before scheduling; the daemon re-proves the key at freeze. */
export function importMatchesReviewed(reply: MediaImportReply, reviewedDigest: string): boolean {
  const hex = reviewedDigest.startsWith("sha256:") ? reviewedDigest.slice(7) : "";
  return /^[a-f0-9]{64}$/.test(hex) && reply.image_digest === hex
    && reply.media_key.endsWith(`.${hex.slice(0, 32)}`);
}

/** Only a queued intent can cancel — past queued the contract answers
 *  `cancel_closed`. */
export function canCancel(state: PublishState): boolean {
  return state === "queued";
}

/** `2026-09-30 18:30 Asia/Hong_Kong (epoch …)` — timezone always shown. */
export function dueLabel(dueEpoch: number, timezone: string): string {
  const date = new Date(dueEpoch * 1000);
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: timezone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).formatToParts(date);
  const get = (type: string) => parts.find((p) => p.type === type)?.value ?? "";
  return `${get("year")}-${get("month")}-${get("day")} ${get("hour")}:${get("minute")} ${timezone} (epoch ${dueEpoch})`;
}

/** Parse a `datetime-local` value as a Unix epoch. Returns null when the
 *  value is not a complete local date/time — the UI keeps Schedule gated. */
export function parseDueEpoch(value: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(value)) return null;
  const epoch = Math.floor(new Date(`${value}:00`).getTime() / 1000);
  return Number.isSafeInteger(epoch) ? epoch : null;
}
