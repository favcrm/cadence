import { withPrivateOperation } from "../../lib/privateSession";

/**
 * AOS-816 (CAD-2885): a member asks an owner for access to a private `project`
 * or a restricted `wiki` path (`projects/<key>/...`, owner-managed `global/...`,
 * or `agents/<alias>/knowledge/...`), decided by an owner from Home's To do
 * rail or the Wiki Access panel.
 *
 * Pure parsing, scope-narrowing helpers, and API calls — shared cleanly across
 * Member Home card, Owner To-do drawer, Owner Wiki Access panel, and unit tests.
 */

export type AccessRequestKind = "project" | "wiki";
export type AccessRequestScope = "project_member" | "wiki_read" | "wiki_write";
export type AccessRequestDuration = "ongoing" | "7d" | "30d";
export type AccessRequestStatus = "pending" | "approved" | "declined" | "cancelled";

export interface AccessCardModel {
  id: string;
  status: AccessRequestStatus;
  kind: AccessRequestKind;
  target: string;
  targetLabel: string;
  requestedScope: AccessRequestScope;
  grantedScope?: AccessRequestScope;
  duration?: AccessRequestDuration;
  reason?: string;
  requestedAt: string;
  decidedAt?: string;
  decidedBy?: string;
  note?: string;
}

export interface AccessOffer {
  id: string;
  status: "offered" | "expired" | "submitting" | "failed" | "already_requested";
  conversation: string;
  kind: AccessRequestKind;
  target: string;
  targetLabel: string;
  requestedScope: AccessRequestScope;
  reason?: string;
  expiresAt: string;
}

export interface AccessInput {
  requestId: string;
  requester: string;
  requesterHandle?: string;
  kind: AccessRequestKind;
  target: string;
  targetLabel: string;
  requestedScope: AccessRequestScope;
  grantedScope?: AccessRequestScope;
  duration?: AccessRequestDuration;
  reason?: string;
  requestedAt: string;
}

export const ACCESS_REQUEST_ID = /^ar_[0-9a-f]{32}$/;
export const ACCESS_REQUEST_OFFER_ID = /^aro_[0-9a-f]{32}$/;
export const ACCESS_REQUEST_REASON_MAX = 500;
export const ACCESS_REQUEST_NOTE_MAX = 500;

const CONVERSATION_ID = /^tc_[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/;
const NO_CONTROLS = /^[^\u0000-\u001f\u007f-\u009f]*$/;
const KINDS: readonly AccessRequestKind[] = ["project", "wiki"];
const SCOPES: readonly AccessRequestScope[] = ["project_member", "wiki_read", "wiki_write"];
const DURATIONS: readonly AccessRequestDuration[] = ["ongoing", "7d", "30d"];
const STATUSES: readonly AccessRequestStatus[] = ["pending", "approved", "declined", "cancelled"];
const OFFER_STATUSES: readonly AccessOffer["status"][] = [
  "offered",
  "expired",
  "submitting",
  "failed",
  "already_requested",
];

const isObject = (v: unknown): v is Record<string, unknown> =>
  v !== null && typeof v === "object" && !Array.isArray(v);
const plain = (v: unknown, max: number): v is string =>
  typeof v === "string" && v.length >= 1 && v.length <= max && NO_CONTROLS.test(v);
const iso = (v: unknown): v is string =>
  typeof v === "string" && v.length <= 40 && !Number.isNaN(Date.parse(v));
const only = (v: Record<string, unknown>, required: string[], optional: string[]): boolean => {
  const keys = Object.keys(v);
  return required.every((k) => keys.includes(k)) && keys.every((k) => required.includes(k) || optional.includes(k));
};

/** Owner may grant the requested scope or narrow it cleanly — never widen it. */
export function allowedGrantScopes(
  kind: AccessRequestKind,
  requested: AccessRequestScope,
): readonly AccessRequestScope[] {
  if (kind === "project") {
    return requested === "project_member" ? ["project_member", "wiki_read"] : ["wiki_read"];
  }
  return requested === "wiki_write" ? ["wiki_write", "wiki_read"] : ["wiki_read"];
}

export function canNarrowAccessScope(
  kind: AccessRequestKind,
  requested: AccessRequestScope,
  granted: AccessRequestScope,
): boolean {
  return allowedGrantScopes(kind, requested).includes(granted);
}

export function scopeLabel(scope: AccessRequestScope, t: (english: string) => string = (s) => s): string {
  switch (scope) {
    case "project_member":
      return t("Project member (tickets & wiki)");
    case "wiki_write":
      return t("Wiki Read & Write");
    case "wiki_read":
      return t("Wiki Read-only");
  }
}

export function scopeShortLabel(scope: AccessRequestScope, t: (english: string) => string = (s) => s): string {
  switch (scope) {
    case "project_member":
      return t("Full member");
    case "wiki_write":
      return t("Wiki edit");
    case "wiki_read":
      return t("Wiki read-only");
  }
}

export function durationLabel(duration: AccessRequestDuration, t: (english: string) => string = (s) => s): string {
  switch (duration) {
    case "ongoing":
      return t("Ongoing");
    case "7d":
      return t("7 days");
    case "30d":
      return t("30 days");
  }
}

/** Destination href inside the board once approved (or when opening target). */
export function accessTargetHref(
  kind: AccessRequestKind,
  target: string,
  grantedScope?: AccessRequestScope,
): string {
  if (kind === "project" && grantedScope === "project_member") {
    return `/projects/${encodeURIComponent(target)}`;
  }
  const wikiPath = kind === "project" ? `projects/${target}` : target;
  return `/wiki/${wikiPath.split("/").map(encodeURIComponent).join("/")}`;
}

/** Strict card parser mirroring `accessRequestCardPayloadSchema`. */
export function parseAccessRequestPayload(p: unknown): AccessCardModel | null {
  if (!isObject(p) || p.v !== 1) return null;
  if (
    !only(
      p,
      ["v", "id", "status", "kind", "target", "target_label", "requested_scope", "requested_at"],
      ["granted_scope", "duration", "reason", "decided_at", "decided_by", "note"],
    )
  ) {
    return null;
  }
  const {
    id,
    status,
    kind,
    target,
    target_label,
    requested_scope,
    granted_scope,
    duration,
    reason,
    requested_at,
    decided_at,
    decided_by,
    note,
  } = p;
  if (typeof id !== "string" || !ACCESS_REQUEST_ID.test(id)) return null;
  if (typeof status !== "string" || !STATUSES.includes(status as AccessRequestStatus)) return null;
  if (typeof kind !== "string" || !KINDS.includes(kind as AccessRequestKind)) return null;
  if (!plain(target, 200) || !plain(target_label, 120)) return null;
  if (typeof requested_scope !== "string" || !SCOPES.includes(requested_scope as AccessRequestScope)) return null;
  if (!iso(requested_at)) return null;
  if ("granted_scope" in p && (typeof granted_scope !== "string" || !SCOPES.includes(granted_scope as AccessRequestScope))) {
    return null;
  }
  if ("duration" in p && (typeof duration !== "string" || !DURATIONS.includes(duration as AccessRequestDuration))) {
    return null;
  }
  if ("reason" in p && !plain(reason, ACCESS_REQUEST_REASON_MAX)) return null;
  if ("decided_at" in p && !iso(decided_at)) return null;
  if ("decided_by" in p && !plain(decided_by, 80)) return null;
  if ("note" in p && !plain(note, ACCESS_REQUEST_NOTE_MAX)) return null;
  return {
    id,
    status: status as AccessRequestStatus,
    kind: kind as AccessRequestKind,
    target,
    targetLabel: target_label,
    requestedScope: requested_scope as AccessRequestScope,
    requestedAt: requested_at,
    ...("granted_scope" in p ? { grantedScope: granted_scope as AccessRequestScope } : {}),
    ...("duration" in p ? { duration: duration as AccessRequestDuration } : {}),
    ...("reason" in p ? { reason: reason as string } : {}),
    ...("decided_at" in p ? { decidedAt: decided_at as string } : {}),
    ...("decided_by" in p ? { decidedBy: decided_by as string } : {}),
    ...("note" in p ? { note: note as string } : {}),
  };
}

export function parseAccessRequestEntry(entry: {
  role: string;
  kind: string;
  payload?: Record<string, unknown> | null;
}): AccessCardModel | null {
  return entry.kind === "access_request" && entry.role === "agent"
    ? parseAccessRequestPayload(entry.payload)
    : null;
}

export function parseAccessOfferEntry(entry: {
  role: string;
  kind: string;
  payload?: Record<string, unknown> | null;
}): AccessOffer | null {
  const p = entry.payload;
  if (
    entry.role !== "agent" ||
    entry.kind !== "access_request" ||
    !isObject(p) ||
    p.v !== 1 ||
    !only(
      p,
      ["v", "id", "status", "conversation", "kind", "target", "target_label", "requested_scope", "expires_at"],
      ["reason"],
    )
  ) {
    return null;
  }
  const { id, status, conversation, kind, target, target_label, requested_scope, reason, expires_at } = p;
  if (
    typeof id !== "string" ||
    !ACCESS_REQUEST_OFFER_ID.test(id) ||
    typeof conversation !== "string" ||
    !CONVERSATION_ID.test(conversation) ||
    typeof status !== "string" ||
    !OFFER_STATUSES.includes(status as AccessOffer["status"]) ||
    typeof kind !== "string" ||
    !KINDS.includes(kind as AccessRequestKind) ||
    !plain(target, 200) ||
    !plain(target_label, 120) ||
    typeof requested_scope !== "string" ||
    !SCOPES.includes(requested_scope as AccessRequestScope) ||
    !iso(expires_at) ||
    ("reason" in p && !plain(reason, ACCESS_REQUEST_REASON_MAX))
  ) {
    return null;
  }
  return {
    id,
    status: status as AccessOffer["status"],
    conversation,
    kind: kind as AccessRequestKind,
    target,
    targetLabel: target_label,
    requestedScope: requested_scope as AccessRequestScope,
    expiresAt: expires_at,
    ...(reason === undefined ? {} : { reason: reason as string }),
  };
}

export function parseAccessInput(input: unknown): AccessInput | null {
  if (
    !isObject(input) ||
    !only(
      input,
      ["request_id", "requester", "kind", "target", "target_label", "requested_scope", "requested_at"],
      ["requester_handle", "granted_scope", "duration", "reason"],
    )
  ) {
    return null;
  }
  const {
    request_id,
    requester,
    requester_handle,
    kind,
    target,
    target_label,
    requested_scope,
    granted_scope,
    duration,
    reason,
    requested_at,
  } = input;
  if (typeof request_id !== "string" || !ACCESS_REQUEST_ID.test(request_id)) return null;
  if (!plain(requester, 80) || !plain(target, 200) || !plain(target_label, 120) || !iso(requested_at)) return null;
  if (typeof kind !== "string" || !KINDS.includes(kind as AccessRequestKind)) return null;
  if (typeof requested_scope !== "string" || !SCOPES.includes(requested_scope as AccessRequestScope)) return null;
  if ("requester_handle" in input && !plain(requester_handle, 80)) return null;
  if ("granted_scope" in input && (typeof granted_scope !== "string" || !SCOPES.includes(granted_scope as AccessRequestScope))) {
    return null;
  }
  if ("duration" in input && (typeof duration !== "string" || !DURATIONS.includes(duration as AccessRequestDuration))) {
    return null;
  }
  if ("reason" in input && !plain(reason, ACCESS_REQUEST_REASON_MAX)) return null;
  return {
    requestId: request_id,
    requester,
    kind: kind as AccessRequestKind,
    target,
    targetLabel: target_label,
    requestedScope: requested_scope as AccessRequestScope,
    requestedAt: requested_at,
    ...("requester_handle" in input ? { requesterHandle: requester_handle as string } : {}),
    ...("granted_scope" in input ? { grantedScope: granted_scope as AccessRequestScope } : {}),
    ...("duration" in input ? { duration: duration as AccessRequestDuration } : {}),
    ...("reason" in input ? { reason: reason as string } : {}),
  };
}

export const declinedAccessLine = (
  model: Pick<AccessCardModel, "decidedBy" | "note">,
  t: (english: string) => string,
  owner: string,
): string => {
  const name = model.decidedBy ?? owner;
  return model.note
    ? t("{name} declined: {note}").replace("{name}", () => name).replace("{note}", () => model.note ?? "")
    : t("{owner} declined.").replace("{owner}", () => name);
};

export type ConfirmAccessOfferOutcome =
  | "sent"
  | "already_requested"
  | "missing"
  | "inactive"
  | "expired"
  | "unavailable"
  | "access"
  | "unknown";

export async function confirmAccessOffer(
  offer: AccessOffer,
  reason?: string,
): Promise<ConfirmAccessOfferOutcome> {
  const body =
    reason !== undefined
      ? JSON.stringify({ reason: reason.trim().slice(0, ACCESS_REQUEST_REASON_MAX) })
      : null;
  try {
    const { status, reply } = await withPrivateOperation(async (operation) => {
      const response = await operation.fetch(
        `/api/access-requests/offers/${offer.id}/confirm?conversation=${encodeURIComponent(offer.conversation)}`,
        {
          method: "POST",
          credentials: "same-origin",
          headers: {
            accept: "application/json",
            ...(body === null ? {} : { "content-type": "application/json" }),
          },
          ...(body === null ? {} : { body }),
        },
      );
      return { status: response.status, reply: (await response.json().catch(() => null)) as unknown };
    });
    if (status === 401 || status === 403) return "access";
    if (status === 404) return "missing";
    if (status === 409) return "inactive";
    if (status === 410) return "expired";
    if (status === 503 && isObject(reply) && isObject(reply.error) && reply.error.code === "owners_unavailable") {
      return "unavailable";
    }
    if (
      status >= 200 &&
      status < 300 &&
      isObject(reply) &&
      reply.ok === true &&
      (reply.outcome === "sent" || reply.outcome === "already_requested")
    ) {
      return reply.outcome;
    }
  } catch {
    // Request may have reached the server; polling reconciles.
  }
  return "unknown";
}

export async function readAccessRequest(requestId: string): Promise<AccessCardModel | null> {
  try {
    const reply = await withPrivateOperation(async (operation) => {
      const response = await operation.fetch(`/api/access-requests/${requestId}`, {
        credentials: "same-origin",
        headers: { accept: "application/json" },
      });
      return response.ok ? ((await response.json().catch(() => null)) as unknown) : null;
    });
    return isObject(reply) && reply.ok === true ? parseAccessRequestPayload(reply.request) : null;
  } catch {
    return null;
  }
}

export type AccessVerb = "approve" | "decline" | "cancel";

export type AccessOutcome =
  | {
      kind: "done";
      status: "approved" | "declined" | "cancelled";
      grantedScope?: AccessRequestScope;
      duration?: AccessRequestDuration;
    }
  | { kind: "already"; status: AccessRequestStatus | null; by: string | null }
  | { kind: "access" }
  | { kind: "missing" }
  | { kind: "refused"; code: string; message: string }
  | { kind: "unknown" };

const accessStatus = (v: unknown): v is AccessRequestStatus =>
  typeof v === "string" && STATUSES.includes(v as AccessRequestStatus);

export async function decideAccessRequest(
  requestId: string,
  verb: AccessVerb,
  opts?: { scope?: AccessRequestScope; duration?: AccessRequestDuration; note?: string },
): Promise<AccessOutcome> {
  if (!ACCESS_REQUEST_ID.test(requestId)) return { kind: "missing" };
  let body: string | null = null;
  if (verb === "approve" && (opts?.scope !== undefined || opts?.duration !== undefined)) {
    body = JSON.stringify({
      ...(opts.scope !== undefined ? { scope: opts.scope } : {}),
      ...(opts.duration !== undefined ? { duration: opts.duration } : {}),
    });
  } else if (verb === "decline" && typeof opts?.note === "string" && opts.note.trim() !== "") {
    body = JSON.stringify({ note: opts.note.slice(0, ACCESS_REQUEST_NOTE_MAX) });
  }
  let status = 0;
  let reply: unknown = null;
  try {
    ({ status, reply } = await withPrivateOperation(async (operation) => {
      const response = await operation.fetch(`/api/access-requests/${requestId}/${verb}`, {
        method: "POST",
        credentials: "same-origin",
        headers: {
          accept: "application/json",
          ...(body === null ? {} : { "content-type": "application/json" }),
        },
        ...(body === null ? {} : { body }),
      });
      return { status: response.status, reply: (await response.json().catch(() => null)) as unknown };
    }));
  } catch {
    return { kind: "unknown" };
  }
  if (status === 401 || status === 403) return { kind: "access" };
  if (status === 404) return { kind: "missing" };
  if (status === 409) {
    const r = isObject(reply) ? reply : {};
    const err = isObject(r.error) ? r.error : null;
    if (err?.code === "project_archived") {
      return {
        kind: "refused",
        code: "project_archived",
        message: typeof err.message === "string" ? err.message : "Target project is archived.",
      };
    }
    return {
      kind: "already",
      status: accessStatus(r.status) ? r.status : null,
      by: typeof r.decided_by === "string" && r.decided_by.trim() !== "" ? r.decided_by : null,
    };
  }
  if (status === 400 || status === 503) {
    const err = isObject(reply) && isObject(reply.error) ? reply.error : null;
    return {
      kind: "refused",
      code: typeof err?.code === "string" ? err.code : "refused",
      message: typeof err?.message === "string" ? err.message : "Could not complete request.",
    };
  }
  if (
    status >= 200 &&
    status < 300 &&
    isObject(reply) &&
    reply.ok === true &&
    (reply.status === "approved" || reply.status === "declined" || reply.status === "cancelled")
  ) {
    const grantedScope =
      typeof reply.granted_scope === "string" && SCOPES.includes(reply.granted_scope as AccessRequestScope)
        ? (reply.granted_scope as AccessRequestScope)
        : undefined;
    const duration =
      typeof reply.duration === "string" && DURATIONS.includes(reply.duration as AccessRequestDuration)
        ? (reply.duration as AccessRequestDuration)
        : undefined;
    return {
      kind: "done",
      status: reply.status,
      ...(grantedScope ? { grantedScope } : {}),
      ...(duration ? { duration } : {}),
    };
  }
  return { kind: "unknown" };
}

export function alreadyAccessText(
  outcome: Extract<AccessOutcome, { kind: "already" }>,
  t: (english: string) => string,
): string {
  const by = outcome.by;
  if (by === null) return t("This request was already decided.");
  const text =
    outcome.status === "approved"
      ? "{name} already approved this."
      : outcome.status === "declined"
        ? "{name} already declined this."
        : outcome.status === "cancelled"
          ? "{name} already cancelled this."
          : "{name} already decided this.";
  return t(text).replace("{name}", () => by);
}
