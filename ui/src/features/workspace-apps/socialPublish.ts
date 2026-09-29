/** CAD-787 product client for the publish decision surface.
 *
 * Binds the approved preview flow to the CAD-771/AOS-94 exact-destination
 * states (landed staging 3d6ced85): the board relays operator-only HTTP
 * under `/api/social-publishes` to the daemon `social_publish_*` RPC, which
 * mirrors the versioned device preflight/execution contract (discovery,
 * `publish.send` grant with maxUses 1..=10, media import, byte-exact
 * status, default-off dispatch gate).
 *
 * The relay does not exist yet (771 backend unmerged) — this module states
 * the exact HTTP shape the relay must serve so UI, tests and the relay
 * converge. All calls are same-origin `/api/` paths; nothing here can
 * reach an external provider. No live post in any test: tests stub fetch.
 */

/** Operator-facing dispatch states.
 *
 * Mapping to the backend (771 @ aa2c2ca8 vs landed AOS-94 3d6ced85):
 * - Store rows: queued/cancelled/processing/posted/refused/held.
 * - Device byte-exact enum: posted/processing/refused only.
 * - `uncertain` is DERIVED, never stored: a processing intent whose
 *   response was lost after accept. The operator reconciles via the
 *   status query before any retry — never a second provider call.
 * - `held` is stored at dispatch recheck when grant, binding, app/context
 *   or digests mismatch (recheck-then-hold); the binding pins
 *   digest-form and the raw caption re-resolves from the run at dispatch.
 */
export type PublishState =
  | "queued"
  | "processing"
  | "posted"
  | "refused"
  | "uncertain"
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
  channel: "instagram" | "facebook";
  /** Exact operator-selected destination, never inferred from a source. */
  destination_id: string;
  destination_handle: string;
  caption_digest: string;
  image_digest: string | null;
  destination_digest: string;
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

async function request<T>(path: string, body?: unknown): Promise<T> {
  const response = await fetch(sameOrigin(path), {
    method: body === undefined ? "GET" : "POST",
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new Error(
      `Publish request failed (${response.status})${detail ? `: ${detail.slice(0, 200)}` : ""}`,
    );
  }
  return (await response.json()) as T;
}

export const socialPublish = {
  list: (installId: string, contextId: string | null) =>
    request<{ intents: PublishIntent[] }>(paths.list(installId, contextId)),
  show: (intentId: string) =>
    request<{ intent: PublishIntent }>(paths.show(intentId)),
  schedule: (body: SchedulePublishBody) =>
    request<{ intent: PublishIntent }>(paths.schedule(), body),
  cancel: (intentId: string) =>
    request<{ intent: PublishIntent }>(paths.cancel(intentId), {}),
};

export function publishStateText(state: PublishState): string {
  switch (state) {
    case "queued": return "Queued";
    case "processing": return "Processing";
    case "posted": return "Posted · verified";
    case "refused": return "Refused";
    case "uncertain": return "Uncertain · reconcile";
    case "cancelled": return "Cancelled";
    case "held": return "Held · reconnect";
  }
}

export function publishStateTone(
  state: PublishState,
): "ok" | "warn" | "fail" | "muted" {
  if (state === "posted") return "ok";
  if (state === "refused") return "fail";
  if (state === "uncertain" || state === "held") return "warn";
  return "muted";
}

/** Operator copy for the exact refusal codes the contract returns (mirror
 *  `SendBinding::validate`, `SendGrant::authorize`, `check_destination`).
 *  Unknown codes stay visible with their raw message, never silent. */
export function refusalCopy(refusal: PublishRefusal): string {
  switch (refusal.code) {
    case "send_disabled":
      return "Send dispatch is not enabled. The intent validated and changed nothing — no provider was called.";
    case "grant_bounds":
      return "The send grant allows 1 to 10 uses. This grant is outside that bound — nothing was published.";
    case "grant_exhausted":
      return "The send grant has no uses left. Nothing was published.";
    case "grant_revoked":
      return "The send grant was revoked. Nothing was published.";
    case "grant_mismatch":
      return "The grant id does not match this binding. Nothing was published.";
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
    case "bad_connection":
    case "bad_grant":
    case "bad_run":
    case "bad_effect":
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

export function canCancel(state: PublishState): boolean {
  return state === "queued" || state === "processing";
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
