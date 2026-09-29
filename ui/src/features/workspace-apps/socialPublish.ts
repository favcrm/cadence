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

export interface PublishIntent {
  intent_id: string;
  install_id: string;
  context_id: string | null;
  run_id: string;
  effect_id: string;
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
  /** Taken from the schedule request — the relay derives it there. */
  idempotency_key: string;
  due_epoch: number;
  timezone: string;
  grant_id: string;
  approval_id: string;
  writer: string;
  reviewer: string;
  permalink: string | null;
  receipt: unknown | null;
  refusal: PublishRefusal | null;
  /** Relay-provided reconcile evidence, set when the status query found a
   *  lost response after accept for this processing intent. Absent means
   *  no uncertain display. Never a stored state. */
  reconcile: { lost_response: boolean; checked_epoch: number } | null;
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
};

function sameOrigin(path: string): string {
  if (!path.startsWith("/api/"))
    throw new Error("Publish client is same-origin only.");
  return path;
}

/** Body keys the relay accepts per operation (mirrors the hostActions
guard pattern): anything else is forged and never serializes. Schedule
carries the artifact-freeze grammar only — digests derive server-side.
Cancel carries an empty body. */
const SCHEDULE_KEYS = [
  "request_id", "install_id", "context_id", "run_id", "effect_id",
  "artifact_id", "bundle_digest", "slot", "destination_id", "toolkit",
  "media_key", "grant_id", "approval_id", "due_epoch", "timezone",
] as const;

function assertCleanBody(body: Record<string, unknown>, allowed: readonly string[]): void {
  for (const key of Object.keys(body)) {
    if (!allowed.includes(key)) throw new ApiError(`refused publish field: ${key}`, 400);
  }
}

/** Reads are abortable and uncached so receipts never outlive an operator
session; transport mirrors workspaceApps/hostActions (same-origin
credentials, session headers, board marker on writes). */
async function request<T>(path: string, signal?: AbortSignal, body?: Record<string, unknown>): Promise<T> {
  if (body !== undefined) assertCleanBody(body, SCHEDULE_KEYS);
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
  list: (installId: string, contextId: string | null, signal?: AbortSignal) =>
    request<{ intents: PublishIntent[] }>(paths.list(installId, contextId), signal),
  show: (intentId: string, signal?: AbortSignal) =>
    request<{ intent: PublishIntent }>(paths.show(intentId), signal),
  schedule: (body: SchedulePublishBody) =>
    request<{ intent: PublishIntent }>(paths.schedule(), undefined, body as unknown as Record<string, unknown>),
  cancel: (intentId: string) =>
    request<{ intent: PublishIntent }>(paths.cancel(intentId), undefined, {}),
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
 *  with lost-response evidence from the status query. The persisted
 *  intent remains processing — no stored enum is invented. Every other
 *  combination (no evidence, or any non-processing state) shows no
 *  uncertain display. */
export function showsUncertainReading(intent: PublishIntent): boolean {
  return (
    intent.state === "processing" &&
    intent.reconcile?.lost_response === true
  );
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
    case "image_required":
      return "Instagram needs a reviewed provider-accessible image. Nothing was published.";
    default:
      return refusal.message
        ? `${refusal.message} (${refusal.code}). Nothing was published.`
        : `Refused (${refusal.code}). Nothing was published.`;
  }
}

/** Landed `cadenceApprovalId` bound: non-empty, max 120 characters.
 *  The UI gates scheduling on it so the contract never sees an
 *  oversize approval from this surface. */
export function isApprovalIdUsable(approvalId: string): boolean {
  return approvalId.length > 0 && approvalId.length <= 120;
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
