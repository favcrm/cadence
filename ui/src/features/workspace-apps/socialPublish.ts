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

/** Operator copy for every refusal code the contract can return, including
 *  the landed default-off gate. Unknown codes stay visible, never silent. */
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
    case "destination_mismatch":
      return "The destination does not match the owner-authorized binding. Nothing was published.";
    case "image_required":
      return "Instagram needs a reviewed provider-accessible image. Nothing was published.";
    case "digest_changed":
      return "Caption, media or destination changed after approval. Held for a new human decision — never silently published.";
    case "binding_changed":
      return "Binding, app or context changed at dispatch. Held for a new human decision.";
    default:
      return refusal.message || `Refused (${refusal.code}). Nothing was published.`;
  }
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
