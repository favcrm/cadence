import { ApiError } from "../../../lib/api";
import { sessionHeaders } from "../../../lib/sessionKey";
import { displayEpoch, displayPlanKey, invalidateDisplay, observeRead, purgeDisplayCache } from "./screenDisplayCache";
import {
  LINKED_MEDIA_MAX_BYTES,
  LINKED_MEDIA_MAX_IMAGE_BYTES,
  RECORD_CALL_VERBS,
  RECORD_SLOT_VERBS,
  validLinkedMediaId,
  type ActionArgs,
  type ActionRefusal,
  type ActionResult,
  type RecordCallVerb,
  type RecordSlotVerb,
} from "./screenProtocol";
import type { MountReceipt } from "./screenLifecycle";
import type { RecordCard, SlotPlan } from "./screenSlot";

export type LinkedMediaKind = "image" | "video" | "audio" | "document";
export type LinkedStagedAttachment = {
  attachment_id: string;
  kind: LinkedMediaKind;
  mime_type: string;
  file_name: string | null;
  byte_length: number;
  sha256: string;
  expires_at: string;
};
export type WhatsAppMediaDownloadResult =
  | { ok: true; bytes: ArrayBuffer; mimeType: string; fileName: string | null; sha256: string }
  | { ok: false; refusal: ActionRefusal };

const ATTACHMENT_ID_RE = /^wam_[0-9a-f]{32}$/;
const SHA256_HEX_RE = /^[0-9a-f]{64}$/;
const LINKED_MEDIA_KINDS = new Set<string>(["image", "video", "audio", "document"]);
const SAFE_LINKED_MEDIA_MIMES = new Set<string>([
  "image/jpeg",
  "image/png",
  "image/webp",
  "video/mp4",
  "video/webm",
  "video/3gpp",
  "video/quicktime",
  "audio/mpeg",
  "audio/ogg",
  "audio/mp4",
  "audio/aac",
  "audio/wav",
  "audio/webm",
  "application/pdf",
  "text/plain",
  "text/csv",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation",
]);

function normalizedSafeMime(raw: string | null): string | null {
  const base = (raw ?? "").split(";")[0]?.trim().toLowerCase() ?? "";
  return SAFE_LINKED_MEDIA_MIMES.has(base) ? base : null;
}

async function sha256Hex(buffer: ArrayBuffer): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", buffer);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function formatMediaBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) {
    const kb = n / 1024;
    return `${kb % 1 === 0 ? kb.toFixed(0) : kb.toFixed(1)} KB`;
  }
  const mb = n / (1024 * 1024);
  return `${mb % 1 === 0 ? mb.toFixed(0) : mb.toFixed(1)} MB`;
}

function parseDispositionFileName(header: string | null): string | null {
  if (!header) return null;
  const match = /filename="([^"\r\n]+)"/i.exec(header);
  if (!match) return null;
  const cleaned = match[1].replace(/[/\\]/g, "").trim();
  if (!cleaned || cleaned === "." || cleaned === ".." || cleaned.length > 200) return null;
  for (let i = 0; i < cleaned.length; i++) {
    const code = cleaned.charCodeAt(i);
    if (code < 32 || code === 127) return null;
  }
  return cleaned;
}

export const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
export function exact(v: Record<string, unknown>, keys: readonly string[]): boolean { return Object.keys(v).every(k => keys.includes(k)); }
const TOKEN = /^[a-f0-9]{64}$/;
const WRITES = new Set([
  "records.create", "records.update", "records.batch", "records.merge", "segments.save", "exclusions.save",
  "suppressions.add", "audience.prepare", "content.save", "content.clone", "proposals.apply",
  "proposals.discard", "sender-bindings.save", "whatsapp.broadcast.draft", "whatsapp.handoff.set",
  "whatsapp.draft.discard", "works.create", "works.patch", "works.trash", "works.restore",
  "works.request_review", "campaign-work.save", "sends.cancel",
]);
const SLOTS = new Set<string>(RECORD_SLOT_VERBS);
/** Mirrors `ScreenOperation::is_screen_read` in `crates/cadence-core/src/app_screen_decl.rs`. */
export const SCREEN_READ_VERBS: ReadonlySet<string> = new Set([
  "records.list", "records.show", "catalog.status", "segments.list", "segments.show",
  "exclusions.list", "exclusions.show", "suppressions.list", "audience.preview",
  "content.list", "content.show", "content.render", "proposals.list",
  "proposals.show", "proposals.render", "sends.list", "sends.show",
  "smtp.show", "origin.show", "screen.route.patch", "screen.preview.show",
  "screen.preview.clear", "whatsapp.status", "whatsapp.conversations.list",
  "whatsapp.conversations.show", "whatsapp.messages", "whatsapp.reply.status",
  "whatsapp.templates.list", "whatsapp.template.show", "whatsapp.template.status",
  "whatsapp.broadcasts.list", "whatsapp.broadcast.show", "whatsapp.broadcast.estimate",
  "whatsapp.broadcast.results", "whatsapp.draft.show", "campaign-work.show", "calendar.query", "records.aggregate", "works.query", "works.get",
]);
const READ_ONLY_CALL_MESSAGE = "a read-only screen admits only read operations";

function serverErrorCode(value: unknown): string | undefined {
  if (!object(value)) return undefined;
  if (typeof value.code === "string" && value.code.trim()) return value.code.trim();
  if (object(value.error) && typeof value.error.code === "string" && value.error.code.trim()) return value.error.code.trim();
  return undefined;
}

function serverErrorCheck(value: unknown): string | undefined {
  if (!object(value)) return undefined;
  if (typeof value.check === "string" && value.check.trim()) return value.check.trim();
  if (object(value.error) && typeof value.error.check === "string" && value.error.check.trim()) return value.error.check.trim();
  return undefined;
}

function serverErrorText(value: unknown): string | null {
  if (!object(value)) return null;
  if (typeof value.error === "string" && value.error.trim()) return value.error.trim();
  if (object(value.error) && typeof value.error.message === "string" && value.error.message.trim()) return value.error.message.trim();
  if (typeof value.text === "string" && value.text.trim()) return value.text.trim();
  if (typeof value.message === "string" && value.message.trim()) return value.message.trim();
  return null;
}

function isNonReadOperation(operation?: RecordCallVerb | RecordSlotVerb): boolean {
  if (operation === undefined) return false;
  return SLOTS.has(operation) || ((RECORD_CALL_VERBS as readonly string[]).includes(operation) && !SCREEN_READ_VERBS.has(operation));
}

function classifyNonFatalForbidden(error: unknown, operation?: RecordCallVerb | RecordSlotVerb): ActionRefusal | null {
  if (!(error instanceof ApiError) || error.status !== 403) return null;
  // CAD-2691: an owner-only refusal is the app's to present, on a read as on a write; the mount stays.
  if (error.code === "owner_required" && !error.check) {
    return { code: "owner_required", text: isNonReadOperation(operation) ? "Only a workspace owner can perform this action." : "Only a workspace owner can view this." };
  }
  if (!isNonReadOperation(operation)) return null;
  if (!error.code && (error.check === "member_role" || error.check === "approver_not_allowed")) {
    return { code: "owner_required", text: "Only a workspace owner can perform this action." };
  }
  if ((!error.code && error.check === "read_only") || (error.code === "forbidden" && !error.check && error.message.trim().toLowerCase() === READ_ONLY_CALL_MESSAGE)) {
    return { code: "forbidden", text: "This screen is read-only; write actions are not permitted." };
  }
  if (error.code === "operation_not_declared" && !error.check) {
    return { code: "forbidden", text: "This operation is unavailable." };
  }
  return null;
}
const SECRET_FIELDS = new Set(["action_token", "confirmation_token", "confirmation_nonce", "confirm_token", "csv_confirm_token", "scope_proof", "password", "credentials", "credential", "secret", "session_key", "host_command"]);
/** Service-only DTO projection. Private CSV delegation stays in this closure. */
function sanitized(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sanitized);
  if (!object(value)) return value;
  return Object.fromEntries(Object.entries(value).filter(([k]) => !SECRET_FIELDS.has(k)).map(([k, v]) => [k, sanitized(v)]));
}
const CONNECTION_REFUSAL_CODES = new Set([
  "needs_reconnect",
  "account_not_authorized",
  "connection_access_denied",
  "rate_limited",
  "upstream_unavailable",
  "upstream_rejected",
  "not_configured",
  "validation_failed",
  "capability_unavailable",
]);
function classifyConnectionRefusal(error: unknown, operation?: RecordCallVerb | RecordSlotVerb): ActionRefusal | null {
  if (!operation?.startsWith("connections.") || !(error instanceof ApiError) || error.check) return null;
  if (!error.code || !CONNECTION_REFUSAL_CODES.has(error.code)) return null;
  const trimmed = error.message.trim();
  const text = trimmed.length > 0 && trimmed.length <= 240
    ? trimmed
    : "Cadence refused this request. Inspect fresh status before retrying an effect.";
  return { code: error.code, text };
}
function refusal(error: unknown, operation?: RecordCallVerb | RecordSlotVerb): ActionRefusal {
  const status = error instanceof ApiError ? error.status : 0;
  // The hosted create either rejects validation before writing or rolls it back.
  // Unknown outcomes must stay ambiguous so the screen cannot submit a duplicate.
  if (operation === "records.create" && status === 400 && error instanceof ApiError && error.code === "validation_failed") {
    return { code: "validation_failed", text: "The record was not created. Review its fields and try again." };
  }
  if (operation === "smtp.show" && error instanceof ApiError && status === 400 && (error.code === "smtp_sender_unbound" || error.code === "smtp_sender_revoked")) {
    return { code: error.code, text: "No live SMTP sender is bound. Configure a sender before sending." };
  }
  // CAD-1634 / CAD-2649: a WhatsApp refusal keeps the host's typed reason
  // (for example the door's `test_limit`, `window_closed`, or `stale_approval`),
  // so the screen and Official reply composer can name it.
  if (operation?.startsWith("whatsapp.") && error instanceof ApiError && (status === 400 || status === 409) && error.code && /^[A-Za-z0-9_-]{1,64}$/.test(error.code)) {
    const text = error.code === "window_closed"
      ? "The WhatsApp 24-hour reply window is closed. Send an approved template instead."
      : error.code === "stale_approval"
        ? "This prepared reply expired. Prepare it again."
        : error.code === "facts_changed"
          ? "That changed. Refresh before trying again."
          : error.code === "owner_required"
            ? "Only a workspace owner can perform this action."
            : "Cadence refused this request. Inspect fresh status before retrying an effect.";
    return { code: error.code, text };
  }
  // Apart from explicit sender-state codes on smtp.show, the child consumes
  // canonical classes (a revision_conflict must still reach the editor as conflict).
  // CAD-1656: the confirm-time recompute refusal keeps its own code.
  // CAD-1662: Works refusals keep their closed store vocabulary, so the screen
  // can tell a missing item from one blocked pending review.
  const WORKS_CODES = ["invalid", "forbidden", "not_found", "conflict", "needs_review", "facts_changed", "rejected", "unauthorized"];
  if (operation?.startsWith("works.") && error instanceof ApiError && status === 400 && error.code && WORKS_CODES.includes(error.code)) {
    return { code: error.code, text: "Cadence refused this Works request. Refresh before trying again." };
  }
  if (operation?.startsWith("catalog.") && error instanceof ApiError && status === 409 &&
      ["confirmation_required", "quote_changed", "grant_changed", "uncertain", "confirmation_invalid"].includes(error.code ?? ""))
    return { code: error.code!, text: error.message };
   if (status === 409 && error instanceof ApiError && error.code === "facts_changed") return { code: "facts_changed", text: "That changed. Refresh before trying again." };
  // CAD-1827: a stale campaign-work revision keeps its own code through the
  // bridge too. The app's stale-save recovery (re-read the row, keep the
  // operator's edits) is keyed on `stale_revision`, not on a generic
  // conflict, so collapsing it would strand the panel in an unusable state.
  if (operation?.startsWith("campaign-work.") && status === 409 && error instanceof ApiError && error.code === "stale_revision")
    return { code: "stale_revision", text: "These campaign work details changed. Reload them before saving again." };
  if (operation === "sends.approve" && error instanceof ApiError && error.code === "no_recipients") return { code: "no_recipients", text: "No one in this audience can be emailed." };
  const connRefusal = classifyConnectionRefusal(error, operation);
  if (connRefusal) return connRefusal;
  if (status === 403) {
    const nonFatal = classifyNonFatalForbidden(error, operation);
    if (nonFatal) return nonFatal;
  }
  const code = ({ 401: "unauthorized", 403: "forbidden", 404: "not_found", 409: "conflict", 503: "unavailable" } as Record<number, string>)[status] ?? "failed";
  return { code, text: status === 401 || status === 403 ? "This app's access is no longer available." : status === 409 ? "That changed. Refresh before trying again." : "Cadence refused this request. Inspect fresh status before retrying an effect." };
}
export type HostCommand = { operation: string; args: ActionArgs; conversation_id?: string | null };
export type ScreenRefusalReason = { status: number; code: string | null };
export interface RecordHost { current(): boolean; command(command: HostCommand): void; refused(reason?: ScreenRefusalReason): void }

/** Validate a `connections.connect` host_command payload (`{ url: "https://..." }` only). */
export function hostedConnectCommandUrl(args: Record<string, unknown>): string | null {
  const keys = Object.keys(args);
  if (keys.length !== 1 || keys[0] !== "url") return null;
  const raw = args.url;
  if (typeof raw !== "string" || raw.length === 0 || raw.length > 4096) return null;
  for (let i = 0; i < raw.length; i += 1) {
    const code = raw.charCodeAt(i);
    if (code <= 0x1f || code === 0x7f) return null;
  }
  try {
    const parsed = new URL(raw);
    if (parsed.protocol !== "https:" || parsed.username || parsed.password) return null;
    return parsed.toString();
  } catch {
    return null;
  }
}

/** One admitted mount; no caller-selected native route or RPC. */
export function recordServices(receipt: MountReceipt, domain: string, host: RecordHost) {
  const csv = new Map<string, { token: string; expires: number }>();
  let retired = false;
  let previewRevision = 0;
  let latestShow: { id: string; revision: number } | null = null;
  const current = () => !retired && host.current();
  const permitted = (op: string) => current() && receipt.host_contract === "screen-records.v1" && receipt.operations?.includes(op) && receipt.action_token;
  async function post(path: "call" | "slot-prepare" | "slot-confirm" | "status", body: ActionArgs, catalog = false): Promise<unknown> {
    if (!current()) throw new ApiError("Mount retired", 403);
    const target = catalog
      ? `/api/app-screen-catalog/${path === "slot-prepare" ? "prepare" : path === "slot-confirm" ? "confirm" : path}`
      : `/api/app-screen-services/${path}`;
    const response = await fetch(target, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify({ action_token: receipt.action_token, ...body }),
    });
    if (!current()) throw new ApiError("Mount retired", 403);
    const text = await response.text();
    if (!current()) throw new ApiError("Mount retired", 403);
    if (new TextEncoder().encode(text).byteLength > 1048576) throw new ApiError("Reply exceeds budget", 502);
    let value: unknown;
    try { value = JSON.parse(text); } catch { throw new ApiError("Invalid service reply", 502); }
    if (!response.ok) {
      const code = serverErrorCode(value);
      const check = serverErrorCheck(value);
      throw new ApiError(
        serverErrorText(value) ?? "Service refused",
        response.status,
        code || check ? { ...(code ? { code } : {}), ...(check ? { check } : {}) } : undefined,
      );
    }
    return value;
  }
  function catalogArgs(args: ActionArgs): ActionArgs {
    if (!exact(args, ["slot", "request_id", "input"]) || typeof args.slot !== "string" ||
        typeof args.request_id !== "string" || !object(args.input))
      throw new ApiError("Invalid app tool request", 400);
    return { slot: args.slot, request_id: args.request_id, input: args.input };
  }
  function failed(error: unknown, operation?: RecordCallVerb | RecordSlotVerb): ActionRefusal {
    csv.clear();
    if (
      current() &&
      error instanceof ApiError &&
      classifyConnectionRefusal(error, operation) === null &&
      (error.status === 401 || (error.status === 403 && classifyNonFatalForbidden(error, operation) === null))
    ) {
      retired = true;
      purgeDisplayCache();
      host.refused({ status: error.status, code: error.code ?? null });
    }
    return refusal(error, operation);
  }
  function deliver(raw: unknown, executeCommand = true): unknown {
    if (!current()) throw new ApiError("Mount retired", 403);
    if (object(raw) && raw.host_command !== undefined) {
      const c = raw.host_command;
      if (!object(c) || !exact(c, ["operation", "args", "conversation_id"]) || typeof c.operation !== "string" || !object(c.args) || (c.conversation_id !== undefined && c.conversation_id !== null && (typeof c.conversation_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(c.conversation_id)))) throw new ApiError("Invalid host command", 502);
      if (executeCommand) host.command(c as unknown as HostCommand);
    }
    return sanitized(raw);
  }
  return {
    retire() { retired = true; csv.clear(); },
    async call(operation: RecordCallVerb, args: ActionArgs): Promise<ActionResult> {
      if (!permitted(operation) || !(RECORD_CALL_VERBS as readonly string[]).includes(operation)) return { ok: false, refusal: { code: "forbidden", text: "This operation is unavailable." } };
      const epoch = displayEpoch(domain);
      const previewId = (operation === "screen.preview.show" || operation === "screen.preview.clear") && typeof args.preview_id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(args.preview_id) ? args.preview_id : null;
      let revision = previewRevision;
      if (previewId && operation === "screen.preview.show") {
        revision = ++previewRevision;
        latestShow = { id: previewId, revision };
      } else if (previewId && operation === "screen.preview.clear" && latestShow?.id === previewId) latestShow = null;
      try {
        const raw = operation === "catalog.call" || operation === "catalog.status"
          ? await post(operation === "catalog.status" ? "status" : "call", catalogArgs(args), true)
          : await post("call", { operation, args });
        const stalePreview = !!previewId && ((operation === "screen.preview.show" && (latestShow?.id !== previewId || latestShow.revision !== revision)) || (operation === "screen.preview.clear" && latestShow?.id === previewId && latestShow.revision > revision));
        let data = deliver(raw, !stalePreview);
        if (operation === "catalog.status" && object(data) && (data.state === "not_started" || data.state === "review_required")) {
          if (!exact(data, ["state"])) throw new ApiError("Invalid app tool status", 502);
          data = { state: "uncertain" };
        }
        if (WRITES.has(operation)) invalidateDisplay(domain);
        else if (operation !== "catalog.status" && operation !== "connections.connect" && operation !== "connections.ads.campaigns" && receipt.display_scope_key) observeRead(receipt.display_scope_key, receipt.display_session_key!, domain, operation, args, data, epoch);
        return { ok: true, data };
      } catch (error) {
        const refusal = failed(error, operation);
        if (current() && previewId && operation === "screen.preview.show" && latestShow?.id === previewId && latestShow.revision === revision) host.command({ operation: "screen.preview.clear", args: { preview_id: previewId } });
        return { ok: false, refusal };
      }
    },
    async planner(operation: RecordSlotVerb, args: ActionArgs): Promise<SlotPlan | ActionRefusal> {
      if (!permitted(operation) || !(RECORD_SLOT_VERBS as readonly string[]).includes(operation)) return { code: "forbidden", text: "This operation is unavailable." };
      try {
        const raw = operation === "catalog.confirm"
          ? await post("slot-prepare", catalogArgs(args), true)
          : await post("slot-prepare", { operation, args });
        if (!object(raw) || !exact(raw, ["confirmation_token", "card"]) || typeof raw.confirmation_token !== "string" || !TOKEN.test(raw.confirmation_token) || !object(raw.card) || !exact(raw.card, operation === "whatsapp.broadcast.stop" || operation === "whatsapp.handoff.release" ? ["operation", "facts", "live"] : ["operation", "facts"]) || raw.card.operation !== operation || !object(raw.card.facts)) throw new ApiError("Invalid confirmation card", 502);
        const facts = raw.card.facts;
        const card = recordCard(operation, facts, object(raw.card.live) ? raw.card.live : undefined);
        let used = false;
        const expires = Date.now() + 60000;
        return { label: card.confirm ?? "Confirm", records: true, recordCard: card, run: async input => {
          if (used || !current() || Date.now() >= expires) return { ok: false, refusal: { code: "expired", text: "This confirmation has expired." } };
          used = true; // Never retry a confirmation, including an uncertain network outcome.
          try {
            const confirmation: ActionArgs = { confirmed: true };
            if (card.input) {
              const field = card.input.field;
              if (field === "brief" && input !== card.input.value) throw new ApiError("Brief changed; request a new card", 400);
              if (card.input.choices && !card.input.choices.some(c => c.value === input)) throw new ApiError("Select a current host choice", 400);
              confirmation[field] = field === "unsubscribe_origin" && !input?.trim() ? null : input ?? "";
            }
            if (operation === "records.csv.import") {
              const key = displayPlanKey(args), held = csv.get(key);
              csv.delete(key);
              if (!held || Date.now() >= held.expires) throw new ApiError("Confirm this exact CSV plan first", 409);
              confirmation.csv_confirm_token = held.token;
            }
            const result = await post("slot-confirm", { confirmation_token: raw.confirmation_token, confirmation }, operation === "catalog.confirm");
            if (operation === "records.csv.confirm") {
              if (!object(result) || typeof result.confirm_token !== "string" || !/^confirm-[a-f0-9]{32}$/.test(result.confirm_token)) throw new ApiError("Missing private CSV receipt", 502);
              for (const [key, held] of csv) if (Date.now() >= held.expires) csv.delete(key);
              if (csv.size >= 4) csv.clear();
              csv.set(displayPlanKey(args), { token: result.confirm_token, expires: Date.now() + 60000 });
            }
            const data = deliver(operation === "records.csv.confirm" && object(result)
              ? { ...result, confirmation_required: result.state === "open" } : result);
            if (!["smtp.manage", "chat.campaign.open", "chat.campaign.brief", "chat.whatsapp.brief", "records.csv.confirm"].includes(operation)) invalidateDisplay(domain);
            return { ok: true, data };
          } catch (error) { return { ok: false, refusal: failed(error, operation) }; }
        } };
      } catch (error) { return failed(error, operation); }
    },
    async stageWhatsAppMedia(file: File): Promise<{ ok: true; data: LinkedStagedAttachment } | { ok: false; refusal: ActionRefusal }> {
      if (!permitted("whatsapp.reply.prepare") || !permitted("whatsapp.reply.send")) {
        return { ok: false, refusal: { code: "forbidden", text: "This operation is unavailable." } };
      }
      const actionToken = receipt.action_token!;
      const form = new FormData();
      form.set("action_token", actionToken);
      form.set("provider", "linked");
      form.set("file", file);
      let response: Response;
      try {
        response = await fetch("/api/app-screen-services/whatsapp-media/stage", {
          method: "POST",
          credentials: "same-origin",
          cache: "no-store",
          headers: { "X-Cadence-Board": "1", ...sessionHeaders() },
          body: form,
        });
      } catch {
        return { ok: false, refusal: { code: "failed", text: "Could not upload attachment." } };
      }
      if (!current()) return { ok: false, refusal: { code: "forbidden", text: "Mount retired." } };
      if (response.status === 404) {
        return {
          ok: false,
          refusal: {
            code: "unsupported_host",
            text: "Attachment uploads are not supported by this workspace host yet.",
          },
        };
      }
      let value: unknown = null;
      try {
        const text = await response.text();
        value = text ? JSON.parse(text) : null;
      } catch {
        value = null;
      }
      if (!response.ok) {
        const code = serverErrorCode(value);
        const message = serverErrorText(value) ?? "Could not upload attachment.";
        if (response.status === 401 || response.status === 403) {
          return {
            ok: false,
            refusal: failed(new ApiError(message, response.status, code ? { code } : undefined), "whatsapp.reply.prepare"),
          };
        }
        if (response.status === 413) {
          return { ok: false, refusal: { code: "payload_too_large", text: message } };
        }
        if (response.status === 415) {
          return { ok: false, refusal: { code: "unsupported_media_type", text: message } };
        }
        return {
          ok: false,
          refusal: {
            code: code && /^[a-z][a-z0-9_]{0,63}$/.test(code) ? code : "failed",
            text: message,
          },
        };
      }
      if (
        !object(value) ||
        !exact(value, ["attachment_id", "kind", "mime_type", "file_name", "byte_length", "sha256", "expires_at"]) ||
        typeof value.attachment_id !== "string" || !ATTACHMENT_ID_RE.test(value.attachment_id) ||
        typeof value.kind !== "string" || !LINKED_MEDIA_KINDS.has(value.kind) ||
        typeof value.mime_type !== "string" || !normalizedSafeMime(value.mime_type) ||
        (value.file_name !== null && typeof value.file_name !== "string") ||
        typeof value.byte_length !== "number" || !Number.isSafeInteger(value.byte_length) || value.byte_length < 1 ||
        typeof value.sha256 !== "string" || !SHA256_HEX_RE.test(value.sha256) ||
        typeof value.expires_at !== "string"
      ) {
        return { ok: false, refusal: { code: "failed", text: "Invalid staged attachment receipt." } };
      }
      return {
        ok: true,
        data: {
          attachment_id: value.attachment_id,
          kind: value.kind as LinkedMediaKind,
          mime_type: value.mime_type,
          file_name: value.file_name,
          byte_length: value.byte_length,
          sha256: value.sha256,
          expires_at: value.expires_at,
        },
      };
    },
    async readStagedWhatsAppMedia(
      attachmentId: string,
    ): Promise<{ ok: true; blob: Blob; mimeType: string; sha256: string } | { ok: false; refusal: ActionRefusal }> {
      if (!permitted("whatsapp.reply.prepare") || !ATTACHMENT_ID_RE.test(attachmentId)) {
        return { ok: false, refusal: { code: "forbidden", text: "This operation is unavailable." } };
      }
      let response: Response;
      try {
        response = await fetch("/api/app-screen-services/whatsapp-media/stage/content", {
          method: "POST",
          credentials: "same-origin",
          cache: "no-store",
          headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
          body: JSON.stringify({ action_token: receipt.action_token, provider: "linked", attachment_id: attachmentId }),
        });
      } catch {
        return { ok: false, refusal: { code: "failed", text: "Could not read staged attachment." } };
      }
      if (!current()) return { ok: false, refusal: { code: "forbidden", text: "Mount retired." } };
      if (!response.ok) {
        let value: unknown = null;
        try {
          value = await response.json();
        } catch {
          value = null;
        }
        const code = serverErrorCode(value);
        const message = serverErrorText(value) ?? "Could not read staged attachment.";
        if (response.status === 401 || response.status === 403) {
          return {
            ok: false,
            refusal: failed(new ApiError(message, response.status, code ? { code } : undefined), "whatsapp.reply.prepare"),
          };
        }
        return {
          ok: false,
          refusal: { code: code && /^[a-z][a-z0-9_]{0,63}$/.test(code) ? code : "failed", text: message },
        };
      }
      const mimeType = normalizedSafeMime(response.headers.get("content-type"));
      const expectedSha = (response.headers.get("x-linked-media-sha256") ?? "").trim().toLowerCase();
      if (!mimeType || !SHA256_HEX_RE.test(expectedSha)) {
        return { ok: false, refusal: { code: "failed", text: "Invalid attachment metadata." } };
      }
      const maxBytes = mimeType.startsWith("image/") ? LINKED_MEDIA_MAX_IMAGE_BYTES : LINKED_MEDIA_MAX_BYTES;
      const buf = await response.arrayBuffer();
      if (!current() || buf.byteLength < 1 || buf.byteLength > maxBytes) {
        return { ok: false, refusal: { code: "failed", text: "Attachment exceeds size limit." } };
      }
      if ((await sha256Hex(buf)) !== expectedSha) {
        return { ok: false, refusal: { code: "failed", text: "Attachment integrity check failed." } };
      }
      return { ok: true, blob: new Blob([buf], { type: mimeType }), mimeType, sha256: expectedSha };
    },
    async deleteStagedWhatsAppMedia(
      attachmentId: string,
    ): Promise<{ ok: true } | { ok: false; refusal: ActionRefusal }> {
      if (!permitted("whatsapp.reply.prepare") || !ATTACHMENT_ID_RE.test(attachmentId)) {
        return { ok: false, refusal: { code: "forbidden", text: "This operation is unavailable." } };
      }
      let response: Response;
      try {
        response = await fetch("/api/app-screen-services/whatsapp-media/stage/delete", {
          method: "POST",
          credentials: "same-origin",
          cache: "no-store",
          headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
          body: JSON.stringify({ action_token: receipt.action_token, provider: "linked", attachment_id: attachmentId }),
        });
      } catch {
        return { ok: false, refusal: { code: "failed", text: "Could not remove attachment." } };
      }
      if (!current()) return { ok: false, refusal: { code: "forbidden", text: "Mount retired." } };
      if (!response.ok) {
        let value: unknown = null;
        try {
          value = await response.json();
        } catch {
          value = null;
        }
        const code = serverErrorCode(value);
        const message = serverErrorText(value) ?? "Could not remove attachment.";
        if (response.status === 401 || response.status === 403) {
          return {
            ok: false,
            refusal: failed(new ApiError(message, response.status, code ? { code } : undefined), "whatsapp.reply.prepare"),
          };
        }
        return {
          ok: false,
          refusal: { code: code && /^[a-z][a-z0-9_]{0,63}$/.test(code) ? code : "failed", text: message },
        };
      }
      return { ok: true };
    },
    async downloadWhatsAppMedia(
      conversationId: string,
      messageId: string,
      signal?: AbortSignal,
    ): Promise<WhatsAppMediaDownloadResult> {
      if (!permitted("whatsapp.messages")) {
        return { ok: false, refusal: { code: "forbidden", text: "This operation is unavailable." } };
      }
      if (!validLinkedMediaId(conversationId) || !validLinkedMediaId(messageId)) {
        return { ok: false, refusal: { code: "validation_failed", text: "Invalid media request." } };
      }
      if (signal?.aborted) {
        return { ok: false, refusal: { code: "cancelled", text: "Download was cancelled." } };
      }
      let response: Response;
      try {
        response = await fetch("/api/app-screen-services/whatsapp-media/download", {
          method: "POST",
          credentials: "same-origin",
          cache: "no-store",
          headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
          body: JSON.stringify({
            action_token: receipt.action_token,
            provider: "linked",
            conversation_id: conversationId,
            message_id: messageId,
          }),
          ...(signal ? { signal } : {}),
        });
      } catch {
        return {
          ok: false,
          refusal: signal?.aborted
            ? { code: "cancelled", text: "Download was cancelled." }
            : { code: "failed", text: "Could not download attachment." },
        };
      }
      if (!current()) return { ok: false, refusal: { code: "forbidden", text: "Mount retired." } };
      if (!response.ok) {
        let value: unknown = null;
        try {
          value = await response.json();
        } catch {
          value = null;
        }
        const code = serverErrorCode(value);
        const message = serverErrorText(value) ?? "Attachment is unavailable.";
        if (response.status === 401 || response.status === 403) {
          return {
            ok: false,
            refusal: failed(new ApiError(message, response.status, code ? { code } : undefined), "whatsapp.messages"),
          };
        }
        return {
          ok: false,
          refusal: {
            code: code && /^[a-z][a-z0-9_]{0,63}$/.test(code) ? code : "media_unavailable",
            text: message,
          },
        };
      }
      const mimeType = normalizedSafeMime(response.headers.get("content-type"));
      const expectedSha = (response.headers.get("x-linked-media-sha256") ?? "").trim().toLowerCase();
      if (!mimeType || !SHA256_HEX_RE.test(expectedSha)) {
        return { ok: false, refusal: { code: "media_unavailable", text: "Attachment metadata is invalid." } };
      }
      const maxBytes = mimeType.startsWith("image/") ? LINKED_MEDIA_MAX_IMAGE_BYTES : LINKED_MEDIA_MAX_BYTES;
      let buf: ArrayBuffer;
      try {
        buf = await response.arrayBuffer();
      } catch {
        return {
          ok: false,
          refusal: signal?.aborted
            ? { code: "cancelled", text: "Download was cancelled." }
            : { code: "failed", text: "Could not read attachment." },
        };
      }
      if (signal?.aborted) {
        return { ok: false, refusal: { code: "cancelled", text: "Download was cancelled." } };
      }
      if (!current() || buf.byteLength < 1 || buf.byteLength > maxBytes) {
        return { ok: false, refusal: { code: "too_large", text: "Attachment exceeds size limit." } };
      }
      if ((await sha256Hex(buf)) !== expectedSha) {
        return { ok: false, refusal: { code: "media_unavailable", text: "Attachment integrity check failed." } };
      }
      if (signal?.aborted) {
        return { ok: false, refusal: { code: "cancelled", text: "Download was cancelled." } };
      }
      const fileName = parseDispositionFileName(response.headers.get("content-disposition"));
      return { ok: true, bytes: buf, mimeType, fileName, sha256: expectedSha };
    },
  };
}

/** The subject as an owner reads it: the merge tag shows as "{first name}", never its fallback. */
const plainSubject = (subject: string) => subject.replace(/\{\{\s*first_name\s*(?:\|[^}]*)?\}\}/g, "{first name}");

/**
 * CAD-2146: send approval, content approval and test send in plain words, the
 * values an owner decides on first and the ids, digests and revisions collapsed.
 * Native and hosted prepare replies carry different facts, so every row is
 * optional except what recordCard already requires; a missing fact is omitted.
 */
const PLAIN_RETRY = "If the result is unclear, check its status before trying again.";
function plainCard(operation: "sends.approve" | "content.approve" | "smtp.test-send", facts: ActionArgs, content: ActionArgs, freeze: ActionArgs | null): RecordCard {
  const facts_: RecordCard["facts"] = [], details: NonNullable<RecordCard["details"]> = [];
  const add = (rows: { label: string; value: string }[], label: string, value: unknown) => { if (value !== undefined && value !== null && value !== "") rows.push({ label, value: typeof value === "object" ? JSON.stringify(value) : String(value) }); };
  const subject = typeof facts.subject === "string" ? facts.subject : typeof content.subject === "string" ? content.subject : undefined;
  const outer = object(facts.binding) ? facts.binding : null;
  const binding = outer && object(outer.binding) ? outer.binding : outer;
  const sender = binding && object(binding.sender) ? binding.sender : object(facts.sender) ? facts.sender : null;
  const address = sender && typeof sender.address === "string" ? sender.address : undefined;
  const from = address ? (typeof sender?.name === "string" && sender.name ? `${sender.name} <${address}>` : address) : undefined;
  const bindingDetails = () => { if (binding) for (const [label, key] of [["SMTP connection", "connection_id"], ["Binding revision", "link_revision"], ["Binding state", "state"], ["Binding digest", "digest"]]) add(details, label, binding[key]); };
  if (subject !== undefined) add(facts_, operation === "content.approve" ? "Subject" : "Email", plainSubject(subject));
  if (operation === "content.approve") {
    add(facts_, "Version", content.revision ?? facts.revision);
    add(details, "Campaign", content.campaign_id ?? facts.campaign_id); add(details, "Name", content.name); add(details, "Content digest", content.content_digest ?? facts.content_digest);
    return { operation, title: "Approve this email?", note: "Nothing is sent yet.", retry: PLAIN_RETRY, confirm: "Approve", facts: facts_, details };
  }
  if (operation === "smtp.test-send") {
    add(facts_, "From", from);
    add(details, "Version", content.revision); add(details, "Name", content.name);
    add(details, "Campaign", content.campaign_id ?? facts.campaign_id); bindingDetails();
    return { operation, title: "Send one test email?", note: "Sends this email to one address.", retry: PLAIN_RETRY, confirm: "Send test", facts: facts_, details, input: { field: "to_email", label: "Send to" } };
  }
  const count = Number(freeze?.final_count), people = `${count.toLocaleString("en")} ${count === 1 ? "person" : "people"}`;
  const base = freeze && object(freeze.base) ? freeze.base : null;
  const excluded = freeze?.exclusion_list_id !== undefined && freeze?.exclusion_list_id !== null;
  let audience: string | undefined;
  if (base?.mode === "all") audience = excluded ? "Everyone who can be emailed, except an exclusion list" : "Everyone who can be emailed";
  else if (base?.mode === "segment") audience = typeof base.segment_name === "string" && base.segment_name ? base.segment_name : "A saved segment";
  else if (base?.mode === "custom") {
    const chosen = Array.isArray(base.customer_ids) ? base.customer_ids.length : base.customer_count;
    if (typeof chosen === "number" && Number.isSafeInteger(chosen)) audience = chosen === 1 ? "1 chosen customer" : `${chosen.toLocaleString("en")} chosen customers`;
  }
  if (audience && excluded && base?.mode !== "all") audience += ", except an exclusion list";
  add(facts_, "Audience", audience); facts_.push({ label: "Recipients", value: count.toLocaleString("en"), strong: true }); add(facts_, "From", from);
  add(details, "Send", facts.send_id); add(details, "State", facts.state); add(details, "Campaign", facts.campaign_id); add(details, "Content version", facts.content_revision); add(details, "Content digest", facts.content_digest);
  add(details, "Send digest", facts.send_digest); add(details, "Audience id", freeze?.freeze_id); add(details, "Segment", base?.mode === "segment" ? base.segment_id : undefined); add(details, "Exclusion list", freeze?.exclusion_list_id); add(details, "Audience digest", freeze?.digest); add(details, "Recipient limit", freeze?.max_recipients);
  bindingDetails(); add(details, "Unsubscribe origin", facts.unsubscribe_origin);
  return { operation, title: `Send to ${people}?`, note: "This can't be undone.", retry: PLAIN_RETRY, confirm: `Send to ${people}`, facts: facts_, details };
}

function recordCard(operation: RecordSlotVerb, facts: ActionArgs, live?: ActionArgs): RecordCard {
  const rows: RecordCard["facts"] = [];
  if (operation === "catalog.confirm") {
    const price = object(facts.price) ? facts.price : null;
    if (!exact(facts, ["tool", "slug", "effect", "price", "input", "account"]) ||
        typeof facts.tool !== "string" || !facts.tool || facts.tool.length > 120 ||
        typeof facts.slug !== "string" || !/^[a-z][a-z0-9_.-]{0,63}$/.test(facts.slug) ||
        (facts.effect !== "read" && facts.effect !== "draft") || !price ||
        !exact(price, ["currency", "scale", "amount"]) || price.currency !== "USD" || price.scale !== 6 ||
        typeof price.amount !== "string" || !/^(0|[1-9]\d{0,9})\.\d{6}$/.test(price.amount) || !object(facts.input) ||
        (facts.account !== undefined && (typeof facts.account !== "string" || !/^[A-Za-z0-9][A-Za-z0-9_.-]{0,159}$/.test(facts.account))))
      throw new ApiError("Invalid app tool confirmation", 502);
    return { operation, title: "Run this app tool?", confirm: "Run tool",
      note: "The listed input is sent to the selected tool. Charges use the workspace balance.",
      retry: "If the outcome is uncertain, check it before requesting another call.",
      facts: [{ label: "Tool", value: facts.tool }, { label: "Effect", value: facts.effect === "read" ? "Read" : "Draft" },
        ...(typeof facts.account === "string" ? [{ label: "Connected account", value: facts.account }] : []),
        { label: "Price", value: `USD ${price.amount}` }, { label: "Input", value: JSON.stringify(facts.input) }] };
  }
  const show = (label: string, value: unknown) => { if (value !== undefined && value !== null) rows.push({ label, value: typeof value === "object" ? JSON.stringify(value) : String(value) }); };
  const contentOuter = object(facts.content) ? facts.content : facts;
  const content = object(contentOuter.content) ? contentOuter.content : contentOuter;
  for (const [label, key] of [["Campaign", "campaign_id"], ["Subject", "subject"], ["Revision", "revision"], ["Name", "name"]]) show(label, content[key]);
  show("Send", facts.send_id); show("Customer", facts.customer_id); show("Recipient", facts.email); show("State", facts.state);
  show("Content revision", facts.content_revision); show("Content digest", facts.content_digest); show("CSV rows", facts.row_count); show("CSV plan", facts.summary);
  const audience = object(facts.audience) ? facts.audience : null;
  const freeze = audience && object(audience.freeze) ? audience.freeze : null;
  if (freeze) { show("Audience", freeze.freeze_id); show("Recipients", freeze.final_count); show("Recipient ceiling", freeze.max_recipients); show("Audience digest", freeze.digest); }
  if (operation === "sends.approve" && freeze?.final_count === 0) throw new ApiError("No recipients", 400, { code: "no_recipients" });
  if (operation === "sends.approve" && (!freeze || !Number.isSafeInteger(freeze.final_count) || Number(freeze.final_count) < 1 || typeof facts.content_revision !== "number" || typeof facts.send_digest !== "string")) throw new ApiError("Incomplete frozen send facts", 502);
  if (operation === "sends.approve" || operation === "content.approve" || operation === "smtp.test-send") return plainCard(operation, facts, content, freeze);
  show("Frozen send digest", facts.send_digest);
  const outer = object(facts.binding) ? facts.binding : null;
  const binding = outer && object(outer.binding) ? outer.binding : outer;
  if (binding) for (const [label, key] of [["SMTP connection", "connection_id"], ["Binding revision", "link_revision"], ["Binding state", "state"], ["Binding digest", "digest"]]) show(label, binding[key]);
  if (operation === "whatsapp.reply.send") {
    // CAD-1562: recipient, window and text come from the daemon's
    // host-computed card for the prepared digest, never from the frame.
    const recipient = object(facts.recipient) ? facts.recipient : null;
    if (!recipient || typeof facts.text !== "string" || typeof facts.digest !== "string" || typeof facts.conversation_id !== "string") throw new ApiError("Incomplete reply facts", 502);
    let attachments: { kind: LinkedMediaKind; mime_type: string; file_name: string | null; byte_length: number; sha256: string }[] | null = null;
    if (facts.attachments !== undefined) {
      if (!Array.isArray(facts.attachments) || facts.attachments.length !== 1 || !object(facts.attachments[0])) {
        throw new ApiError("Incomplete reply facts", 502);
      }
      const item = facts.attachments[0];
      if (
        typeof item.attachment_id !== "string" || !ATTACHMENT_ID_RE.test(item.attachment_id) ||
        typeof item.kind !== "string" || !LINKED_MEDIA_KINDS.has(item.kind) ||
        typeof item.mime_type !== "string" || !item.mime_type ||
        (item.file_name !== null && (typeof item.file_name !== "string" || !item.file_name)) ||
        typeof item.byte_length !== "number" || !Number.isSafeInteger(item.byte_length) || item.byte_length < 1 ||
        typeof item.sha256 !== "string" || !SHA256_HEX_RE.test(item.sha256)
      ) {
        throw new ApiError("Incomplete reply facts", 502);
      }
      attachments = [item as { kind: LinkedMediaKind; mime_type: string; file_name: string | null; byte_length: number; sha256: string }];
    }
    show("To", [recipient.name, recipient.phone].filter(v => typeof v === "string" && v).join(" · ") || facts.conversation_id);
    show("Conversation", facts.conversation_id);
    if (!attachments || facts.text !== "") show("Message", facts.text);
    show("Reply to message", facts.reply_to_message_id);
    if (attachments) {
      const att = attachments[0];
      show("Attachment", `${att.file_name ?? att.kind} (${att.kind}, ${att.mime_type}, ${formatMediaBytes(att.byte_length)})`);
      show("Attachment SHA-256", att.sha256);
    }
    show("Free-text window open until", facts.freeform_until); show("Reply digest", facts.digest);
  }
  if (operation === "whatsapp.template.send") {
    // CAD-1590: recipient from the live conversation; template, revision
    // and rendered body from the live approved template under the host digest.
    const recipient = object(facts.recipient) ? facts.recipient : null;
    const template = object(facts.template) ? facts.template : null;
    if (!recipient || !template || typeof facts.rendered_body !== "string" || typeof facts.digest !== "string") throw new ApiError("Incomplete template facts", 502);
    show("To", [recipient.name, recipient.phone].filter(v => typeof v === "string" && v).join(" · ") || facts.conversation_id);
    show("Number", facts.connection_id); show("Template", `${template.name} (${template.language})`); show("Template revision", template.revision);
    show("Variables", facts.variables); show("Message", facts.rendered_body);
    show("Reply window", facts.reply_mode === "freeform" ? "Open" : "Closed — approved templates may still be sent");
    show("Send digest", facts.digest);
  }
  if (operation === "whatsapp.broadcast.test.send") {
    const template = object(facts.template) ? facts.template : null;
    if (typeof facts.to !== "string" || typeof facts.digest !== "string") throw new ApiError("Incomplete test facts", 502);
    show("Test number", facts.to);
    if (template) { show("Template", `${template.name} (${template.language})`); show("Template revision", template.revision); }
    show("Sample message", facts.rendered_sample); show("Limit", facts.limit_note); show("Send digest", facts.digest);
  }
  if (operation === "whatsapp.broadcast.send") {
    // CAD-1590: the snapshot FavCRM froze at review; nothing from the frame.
    const snap = object(facts.snapshot) ? facts.snapshot : null;
    const template = snap && object(snap.template) ? snap.template : null;
    const audience = snap && object(snap.audience) ? snap.audience : null;
    if (!snap || !template || !audience || !Number.isSafeInteger(facts.eligible) || !Number.isSafeInteger(facts.excluded) || typeof facts.digest !== "string") throw new ApiError("Incomplete broadcast facts", 502);
    const connection = object(snap.connection) ? snap.connection : {};
    show("Number", connection.connectionId);
    show("Template", `${template.name} (${template.language})`); show("Template revision", template.revision);
    show("Variables", facts.body_parameters); show("Header media", facts.header_media_url);
    show("Audience", audience.source === "segment" ? `Segment ${audience.segmentId}` : `${audience.accounts} selected contacts`);
    show("Eligible recipients", facts.eligible); show("Excluded", facts.excluded); show("Exclusion breakdown", snap.counts);
    show("Send time", typeof facts.send_at === "string" ? facts.send_at : "Now, after a 60-second undo window");
    show("Opt-outs", "Rechecked at send; the recipient count can only fall, never grow.");
    show("Messaging limit", "Numbers above your Meta messaging limit fail and appear in results.");
    show("Billing", "Meta charges your WhatsApp Business Account per delivered marketing message.");
    show("Review expires", snap.expires_at); show("Snapshot digest", snap.snapshotDigest); show("Send digest", facts.digest);
  }
  if (operation === "whatsapp.broadcast.stop") {
    const template = object(facts.template) ? facts.template : null;
    const stats = live && object(live.delivery_stats) ? live.delivery_stats : null;
    if (template) { show("Template", `${template.name} (${template.language})`); show("Template revision", template.revision); }
    show("Scheduled for", facts.scheduled_at);
    if (live) { show("Status (approximate, as of this card)", live.status); show("Delivery so far (approximate, still changing)", stats); }
    show("Effect", "Stops unsent recipients only. Messages already sent cannot be recalled. A stopped broadcast cannot resume; inside the undo window it returns to a draft instead.");
  }
  if (operation === "works.review" || operation === "works.export") {
    // CAD-1662: the card draws the doc's current revision the host re-proved;
    // the reviewer or exporter confirms that exact revision, nothing else.
    show("Title", facts.title); show("Kind", facts.kind); show("Revision", facts.revision); show("Author", facts.author); show("Digest", facts.contentDigest);
    if (operation === "works.review") show("Decision", "Approve this revision, or request changes.");
    if (operation === "works.export") show("Format", "Export receipt for this approved revision only.");
  }
  if (operation === "whatsapp.handoff.release") {
    // CAD-1651: the takeover time is the bound fact; the contact is live and unbound.
    const contact = live && object(live.contact) ? live.contact : null;
    if (contact) show("Contact", [contact.name, contact.phone].filter(v => typeof v === "string" && v).join(" · "));
    show("Conversation", facts.conversation_id); show("Taken over since", facts.handoff_at);
    show("Effect", "FavCRM's AI may reply automatically again on this conversation.");
  }
  if (typeof facts.state === "string" && facts.state === "pending" && operation.startsWith("whatsapp.")) show("Approval", "Awaiting the workspace owner's approval; tapping again after approval sends it once.");
  show("Scope", facts.scope); show("Unsubscribe origin", facts.unsubscribe_origin ?? (operation === "origin.manage" ? "Not configured" : undefined));
  const card: RecordCard = { operation, facts: rows };
  if (operation === "works.review") card.input = { field: "choice", label: "Your decision on this exact revision", choices: [{ value: "approve", label: "Approve this revision" }, { value: "changes", label: "Request changes" }] };
  if (operation === "works.export") card.input = { field: "choice", label: "Export format for this approved revision", choices: [{ value: "html", label: "HTML" }, { value: "markdown", label: "Markdown" }] };
  if (operation === "sends.resolve") card.input = { field: "resolution", label: "Resolve uncertain outcome — this does not resend", choices: [{ value: "accepted", label: "Accepted by the server" }, { value: "failed", label: "Failed" }] };
  if (operation === "origin.manage") card.input = { field: "unsubscribe_origin", label: "Daemon-global unsubscribe origin (blank clears it)", value: typeof facts.unsubscribe_origin === "string" ? facts.unsubscribe_origin : "" };
  if (operation === "chat.campaign.brief") { show("Effect", facts.effect); card.input = { field: "brief", label: "Send this brief to the assistant", value: typeof facts.brief === "string" ? facts.brief : "" }; }
  if (operation === "chat.whatsapp.brief") {
    // CAD-1636: the host names the contact, the bound and the model; the guidance is shown read-only.
    if (typeof facts.bound !== "string" || typeof facts.model !== "string") throw new ApiError("Incomplete brief facts", 502);
    show("Contact", facts.contact ?? "Unknown contact"); show("What the assistant reads", facts.bound); show("Model", facts.model); show("Effect", facts.effect);
    card.input = { field: "brief", label: "Your guidance for the assistant", value: typeof facts.guidance === "string" ? facts.guidance : "" };
  }
  if (operation === "smtp.bind" || operation === "smtp.rebind") {
    const choices = Array.isArray(facts.connections) ? facts.connections.filter(object).filter(c => typeof c.connection_id === "string").map(c => ({ value: c.connection_id as string, label: `${c.connection_id} — ${JSON.stringify(c.sender)}` })) : [];
    card.input = { field: "connection_id", label: "Admitted SMTP connection", choices };
  }
  return card;
}
