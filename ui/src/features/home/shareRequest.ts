/**
 * AOS-711 (CAD-2705): a member asks an owner to share an owner-only report.
 * The contract is agenticos-v2's `packages/contracts/src/share-request.ts`
 * (copied fixtures: tests/fixtures/share-request). Pure parsing and the
 * decision call — no React — so the card, the owner drawer and the tests share
 * one reading. Nothing here grants access: the owner's share renders the view
 * to a PDF and the member gets that file only.
 *
 * Strict like the contract's `strictObject`: a payload that does not match
 * exactly is not a card (the entry's text line shows instead). Every string
 * came from a person, so it is shown as plain text.
 */
import { withPrivateOperation } from "../../lib/privateSession";
import { routeParams, type RouteParams } from "../workspace-apps/screen/routeParams";

export type ShareStatus = "pending" | "shared" | "declined" | "cancelled";

export interface ShareFile {
  id: string;
  version: number;
  title: string;
  bytes: number;
}

/** The member's card, `payload` of a `share_request` entry (or `GET /api/share-requests/:id`'s `request`). */
export interface ShareCardModel {
  id: string;
  status: ShareStatus;
  app: string;
  appLabel: string;
  view: string;
  viewLabel: string;
  title: string;
  params?: RouteParams;
  reason?: string;
  requestedAt: string;
  decidedAt?: string;
  decidedBy?: string;
  note?: string;
  file?: ShareFile;
}

/** An owner's staged `share_request` action: `input` of the row. `params` is a URL query. */
export interface ShareInput {
  requestId: string;
  requester: string;
  app: string;
  appLabel: string;
  view: string;
  viewLabel: string;
  params: RouteParams | undefined;
  reason?: string;
  requestedAt: string;
}

export const SHARE_REQUEST_ID = /^sr_[0-9a-f]{32}$/;
export const SHARE_NOTE_MAX = 500;
const APP = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;
const VIEW = /^[A-Za-z0-9_-]{1,128}$/;
const FILE_ID = /^[A-Za-z0-9_-]{1,128}$/;
const NO_CONTROLS = /^[^\u0000-\u001f\u007f-\u009f]*$/;
const STATUSES: readonly string[] = ["pending", "shared", "declined", "cancelled"];

const isObject = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === "object" && !Array.isArray(v);
const plain = (v: unknown, max: number): v is string => typeof v === "string" && v.length >= 1 && v.length <= max && NO_CONTROLS.test(v);
const iso = (v: unknown): v is string => typeof v === "string" && v.length <= 40 && !Number.isNaN(Date.parse(v));
const only = (v: Record<string, unknown>, required: string[], optional: string[]): boolean => {
  const keys = Object.keys(v);
  return required.every((k) => keys.includes(k)) && keys.every((k) => required.includes(k) || optional.includes(k));
};

function parseFile(v: unknown): ShareFile | null {
  if (!isObject(v) || !only(v, ["id", "version", "title", "bytes"], [])) return null;
  const { id, version, title, bytes } = v;
  if (typeof id !== "string" || !FILE_ID.test(id) || !plain(title, 200)) return null;
  if (!Number.isInteger(version) || (version as number) < 1 || !Number.isInteger(bytes) || (bytes as number) < 1) return null;
  return { id, version: version as number, title, bytes: bytes as number };
}

/** The card payload, or null when it is not exactly the contract's. */
export function parseShareRequestPayload(p: unknown): ShareCardModel | null {
  if (!isObject(p) || p.v !== 1) return null;
  if (!only(p, ["v", "id", "status", "app", "app_label", "view", "view_label", "title", "requested_at"], ["params", "reason", "decided_at", "decided_by", "note", "file"])) return null;
  const { id, status, app, app_label, view, view_label, title, params, reason, requested_at, decided_at, decided_by, note, file } = p;
  if (typeof id !== "string" || !SHARE_REQUEST_ID.test(id)) return null;
  if (typeof status !== "string" || !STATUSES.includes(status)) return null;
  if (typeof app !== "string" || !APP.test(app) || typeof view !== "string" || !VIEW.test(view)) return null;
  if (!plain(app_label, 60) || !plain(view_label, 60) || !plain(title, 120) || !iso(requested_at)) return null;
  if ("params" in p && !routeParams(params)) return null;
  if ("reason" in p && !plain(reason, 500)) return null;
  if ("decided_at" in p && !iso(decided_at)) return null;
  if ("decided_by" in p && !plain(decided_by, 80)) return null;
  if ("note" in p && !plain(note, SHARE_NOTE_MAX)) return null;
  const parsedFile = "file" in p ? parseFile(file) : null;
  if ("file" in p && parsedFile === null) return null;
  return {
    id, status: status as ShareStatus, app, appLabel: app_label, view, viewLabel: view_label, title, requestedAt: requested_at,
    ...("params" in p ? { params: params as RouteParams } : {}),
    ...("reason" in p ? { reason: reason as string } : {}),
    ...("decided_at" in p ? { decidedAt: decided_at as string } : {}),
    ...("decided_by" in p ? { decidedBy: decided_by as string } : {}),
    ...("note" in p ? { note: note as string } : {}),
    ...(parsedFile ? { file: parsedFile } : {}),
  };
}

/** A `share_request` chat entry, or null (the text line shows instead). */
export function parseShareRequestEntry(entry: { role: string; kind: string; payload?: Record<string, unknown> | null }): ShareCardModel | null {
  return entry.kind === "share_request" && entry.role === "agent" ? parseShareRequestPayload(entry.payload) : null;
}

export interface ShareOffer {
  id: string;
  status: "offered" | "expired" | "submitting" | "failed" | "already_requested";
  conversation: string;
  appLabel: string;
  viewLabel: string;
  title: string;
  params?: RouteParams;
  reason?: string;
  expiresAt: string;
}

/** Transitional offer; invalid or future versions retain the entry's safe text fallback. */
export function parseShareOfferEntry(entry: { role: string; kind: string; payload?: Record<string, unknown> | null }): ShareOffer | null {
  const p = entry.payload;
  if (entry.role !== "agent" || entry.kind !== "share_request" || !isObject(p) || p.v !== 1 ||
      !only(p, ["v", "id", "status", "conversation", "app", "app_label", "view", "view_label", "title", "expires_at"], ["params", "reason"])) return null;
  const { id, status, conversation, app, app_label, view, view_label, title, params, reason, expires_at } = p;
  if (typeof id !== "string" || !/^sro_[0-9a-f]{32}$/.test(id) ||
      typeof conversation !== "string" || !/^tc_[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(conversation) ||
      typeof status !== "string" || !["offered", "expired", "submitting", "failed", "already_requested"].includes(status) ||
      typeof app !== "string" || !APP.test(app) || typeof view !== "string" || !VIEW.test(view) ||
      !plain(app_label, 60) || !plain(view_label, 60) || !plain(title, 120) || !iso(expires_at) ||
      ("reason" in p && !plain(reason, 500)) ||
      ("params" in p && !routeParams(params))) return null;
  return { id, status: status as ShareOffer["status"], conversation, appLabel: app_label, viewLabel: view_label, title,
    expiresAt: expires_at, ...(params === undefined ? {} : { params: params as RouteParams }),
    ...(reason === undefined ? {} : { reason: reason as string }) };
}

export type ConfirmOfferOutcome = "sent" | "already_requested" | "missing" | "inactive" | "expired" | "unavailable" | "access" | "unknown";

/** Explicit check and confirm both address the same durable operation. */
export async function confirmShareOffer(offer: ShareOffer): Promise<ConfirmOfferOutcome> {
  try {
    const { status, reply } = await withPrivateOperation(async (operation) => {
      const response = await operation.fetch(`/api/share-requests/offers/${offer.id}/confirm?conversation=${encodeURIComponent(offer.conversation)}`, {
        method: "POST", credentials: "same-origin", headers: { accept: "application/json" },
      });
      return { status: response.status, reply: await response.json().catch(() => null) as unknown };
    });
    if (status === 401 || status === 403) return "access";
    if (status === 404) return "missing";
    if (status === 409) return "inactive";
    if (status === 410) return "expired";
    if (status === 503 && isObject(reply) && isObject(reply.error) && reply.error.code === "owners_unavailable") return "unavailable";
    if (status >= 200 && status < 300 && isObject(reply) && reply.ok === true &&
        (reply.outcome === "sent" || reply.outcome === "already_requested")) return reply.outcome;
  } catch {
    // The server may have accepted the request before the connection failed.
  }
  return "unknown";
}

/** `name=value&...` (percent-encoded) as a view's params; none is `undefined`, a set the app would refuse is null. */
export function paramsOfQuery(query: string): RouteParams | undefined | null {
  const q = new URLSearchParams(query);
  const found: RouteParams = {};
  let count = 0;
  for (const [name, value] of q) {
    found[name] = value;
    count++;
  }
  if (count === 0) return undefined;
  return routeParams(found) && Object.keys(found).length === count ? found : null;
}

/** The input of an owner's staged action (`shareRequestActionInputSchema`), or null. */
export function parseShareInput(input: unknown): ShareInput | null {
  if (!isObject(input) || !only(input, ["request_id", "requester", "app", "app_label", "view", "view_label", "params", "requested_at"], ["reason"])) return null;
  const { request_id, requester, app, app_label, view, view_label, params, reason, requested_at } = input;
  if (typeof request_id !== "string" || !SHARE_REQUEST_ID.test(request_id)) return null;
  if (!plain(requester, 80) || !plain(app_label, 60) || !plain(view_label, 60) || !iso(requested_at)) return null;
  if (typeof app !== "string" || !APP.test(app) || typeof view !== "string" || !VIEW.test(view)) return null;
  if (typeof params !== "string" || params.length > 1200) return null;
  if ("reason" in input && !plain(reason, 500)) return null;
  const parsed = paramsOfQuery(params);
  if (parsed === null) return null;
  // The row carries a sorted query; the period reads first, as on the member's card.
  const rank = (name: string) => (name === "from" ? 0 : name === "to" ? 1 : 2);
  const ordered = parsed === undefined ? undefined : Object.fromEntries(Object.entries(parsed).sort(([a], [b]) => rank(a) - rank(b)));
  return {
    requestId: request_id, requester, app, appLabel: app_label, view, viewLabel: view_label, params: ordered, requestedAt: requested_at,
    ...("reason" in input ? { reason: reason as string } : {}),
  };
}

/** The card's one line for a reader that draws no card, from the stored text (a person's words, as typed). */
export const declinedLine = (model: Pick<ShareCardModel, "decidedBy" | "note">, t: (english: string) => string, owner: string): string => {
  const name = model.decidedBy ?? owner;
  return model.note ? t("{name} declined: {note}").replace("{name}", () => name).replace("{note}", () => model.note ?? "") : t("{owner} declined.").replace("{owner}", () => name);
};

// ---- the decisions: POST /api/share-requests/:id/{share,decline,cancel} ----

export type ShareVerb = "share" | "decline" | "cancel";

export type ShareOutcome =
  | { kind: "done"; status: "shared" | "declined" | "cancelled"; file?: ShareFile }
  /** 409: first decision wins; names the winner. */
  | { kind: "already"; status: ShareStatus | null; by: string | null }
  /** 503: the PDF could not be made; the request stays pending. */
  | { kind: "snapshot" }
  /** 401/403: not an owner (or not the requester). */
  | { kind: "access" }
  /** 404: gone, or not this person's to see. */
  | { kind: "missing" }
  /** The reply never arrived or could not be read: it may have gone through. */
  | { kind: "unknown" };

const sharedStatus = (v: unknown): v is ShareStatus => typeof v === "string" && STATUSES.includes(v);

/**
 * One decision. Never retried here: the caller re-reads to learn the truth after `unknown`.
 * `note` goes with a decline only, as typed (the server trims it; blank is none).
 */
export async function decideShareRequest(requestId: string, verb: ShareVerb, note?: string): Promise<ShareOutcome> {
  if (!SHARE_REQUEST_ID.test(requestId)) return { kind: "missing" };
  const body = verb === "decline" && typeof note === "string" && note.trim() !== "" ? JSON.stringify({ note: note.slice(0, SHARE_NOTE_MAX) }) : null;
  let status = 0;
  let reply: unknown = null;
  try {
    ({ status, reply } = await withPrivateOperation(async (operation) => {
      const response = await operation.fetch(`/api/share-requests/${requestId}/${verb}`, {
        method: "POST",
        credentials: "same-origin",
        headers: { accept: "application/json", ...(body === null ? {} : { "content-type": "application/json" }) },
        ...(body === null ? {} : { body }),
      });
      return { status: response.status, reply: await response.json().catch(() => null) as unknown };
    }));
  } catch {
    return { kind: "unknown" };
  }
  if (status === 401 || status === 403) return { kind: "access" };
  if (status === 404) return { kind: "missing" };
  if (status === 503) return { kind: "snapshot" };
  if (status === 409) {
    const r = isObject(reply) ? reply : {};
    return { kind: "already", status: sharedStatus(r.status) ? r.status : null, by: typeof r.decided_by === "string" && r.decided_by.trim() !== "" ? r.decided_by : null };
  }
  if (status >= 200 && status < 300 && isObject(reply) && reply.ok === true && (reply.status === "shared" || reply.status === "declined" || reply.status === "cancelled")) {
    const file = reply.file === undefined ? null : parseFile(reply.file);
    return { kind: "done", status: reply.status, ...(file ? { file } : {}) };
  }
  return { kind: "unknown" };
}

/** `GET /api/share-requests/:id`: the request as the card draws it, or null when it cannot be read. */
export async function readShareRequest(requestId: string): Promise<ShareCardModel | null> {
  try {
    const reply = await withPrivateOperation(async (operation) => {
      const response = await operation.fetch(`/api/share-requests/${requestId}`, { credentials: "same-origin", headers: { accept: "application/json" } });
      return response.ok ? await response.json().catch(() => null) as unknown : null;
    });
    return isObject(reply) && reply.ok === true ? parseShareRequestPayload(reply.request) : null;
  } catch {
    return null;
  }
}

/** Who decided, in words: "Chris already shared this." */
export function alreadyText(outcome: Extract<ShareOutcome, { kind: "already" }>, t: (english: string) => string): string {
  const by = outcome.by;
  if (by === null) return t("This request was already decided.");
  const text = outcome.status === "shared" ? "{name} already shared this."
    : outcome.status === "declined" ? "{name} already declined this."
    : outcome.status === "cancelled" ? "{name} already cancelled this."
    : "{name} already decided this.";
  return t(text).replace("{name}", () => by);
}
