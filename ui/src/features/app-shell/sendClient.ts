import { ApiError } from "../../lib/api";
import { hostErrorText } from "./shared/hostErrors";
import { sessionHeaders } from "../../lib/sessionKey";
import { assertRecordId, assertScope, type HostScope } from "./hostActions";

/**
 * Typed host action client for the CRM send routes (CAD-786 UI over
 * the CAD-785 sender binding and the CAD-786 approved bounded send).
 *
 * Unlike the record/content clients these routes are flat
 * `/api/crm-smtp/*` and `/api/crm-send/*` endpoints, so
 * `install_id`/`context_id` travel inside the POST body or the read
 * query — but only ever the trusted route scope's values, never a
 * form field, chat message or stored draft. Bodies carry exactly the
 * peer's allowlisted grammar (the Rust handlers are
 * `deny_unknown_fields`); anything else throws client-side before a
 * byte is sent. No actor claims, receipt-shaped fields, secrets or
 * project links ever enter this client. `unsubscribe_origin` is the
 * sole non-string body field and is either a string or JSON null.
 *
 * Honest copy: every send receipt carries `delivery_claim:
 * "smtp-acceptance-only"` — SMTP acceptance is an operational
 * result, never proof of inbox delivery or reading.
 */

export type SendScope = HostScope;

/** The label every acceptance receipt wears — never "delivered". */
export const DELIVERY_CLAIM = "smtp-acceptance-only";

/** Body keys the HTTP peers accept, per operation. */
const SMTP_SHOW_KEYS = ["install_id", "context_id"] as const;
const SMTP_BIND_KEYS = ["install_id", "context_id", "connection_id", "request_id"] as const;
const SMTP_REBIND_KEYS = ["install_id", "context_id", "connection_id", "expected_revision"] as const;
const SMTP_REVOKE_KEYS = ["install_id", "context_id", "expected_revision"] as const;
const SMTP_TEST_KEYS = ["install_id", "context_id", "campaign_id", "to_email"] as const;
const SEND_PREPARE_KEYS = [
  "install_id",
  "context_id",
  "campaign_id",
  "audience_freeze_id",
  "request_id",
] as const;
const SEND_APPROVE_KEYS = ["install_id", "context_id", "send_id", "send_digest"] as const;
const SEND_RESOLVE_KEYS = [
  "install_id",
  "context_id",
  "send_id",
  "customer_id",
  "resolution",
] as const;
const SEND_ORIGIN_KEYS = ["unsubscribe_origin"] as const;

function assertClean(body: Record<string, unknown>, allowed: readonly string[], what: string): void {
  for (const key of Object.keys(body)) {
    if (!allowed.includes(key)) {
      throw new ApiError(`send ${what} carries a forbidden field`, 400, {
        code: "forbidden_field",
      });
    }
  }
}

const part = encodeURIComponent;

async function post<T>(path: string, body: Record<string, unknown>): Promise<T> {
  const resp = await fetch(path, {
    method: "POST",
    credentials: "same-origin",
    cache: "no-store",
    headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
    body: JSON.stringify(body),
  });
  const value = await resp.json().catch(() => null);
  if (!resp.ok) {
    const serverError = (value as { error?: unknown } | null)?.error;
    throw new ApiError(
      typeof serverError === "string" && serverError !== ""
        ? serverError
        : `${resp.status} ${resp.statusText}`.trim(),
      resp.status,
    );
  }
  if (value === null) throw new ApiError("The server returned an invalid send receipt", 502);
  return value as T;
}

async function get<T>(path: string): Promise<T> {
  const resp = await fetch(path, {
    credentials: "same-origin",
    cache: "no-store",
    headers: { ...sessionHeaders() },
  });
  const value = await resp.json().catch(() => null);
  if (!resp.ok) {
    const serverError = (value as { error?: unknown } | null)?.error;
    throw new ApiError(
      typeof serverError === "string" && serverError !== ""
        ? serverError
        : `${resp.status} ${resp.statusText}`.trim(),
      resp.status,
    );
  }
  if (value === null) throw new ApiError("The server returned an invalid send receipt", 502);
  return value as T;
}

export const sendPaths = {
  smtpShow: "/api/crm-smtp/show",
  smtpBind: "/api/crm-smtp/bind",
  smtpRebind: "/api/crm-smtp/rebind",
  smtpRevoke: "/api/crm-smtp/revoke",
  smtpTestSend: "/api/crm-smtp/test-send",
  sendPrepare: "/api/crm-send/prepare",
  sendApprove: "/api/crm-send/approve",
  sendResolve: "/api/crm-send/resolve",
  sendShow: "/api/crm-send/show",
  sendList: "/api/crm-send/list",
  sendOrigin: "/api/crm-send/origin",
} as const;

function assertRevision(revision: number): void {
  if (!Number.isInteger(revision) || revision <= 0) {
    throw new ApiError("expected binding revision must be a positive integer", 400);
  }
}

/** One live SMTP sender binding as the host reports it. */
export interface SmtpBinding {
  connectionId: string;
  authRevision: number;
  linkRevision: number;
  state: string;
  digest: string;
  sender: { name: string; address: string };
  transport: { host: string; port: number; tlsMode: string; username: string };
}

/** `null` where the host reports no live binding (its refusal is
 *  scoped to the binding state, not an HTTP failure). */
export function parseSmtpBinding(value: unknown): SmtpBinding {
  const binding = (value as { binding?: unknown } | null)?.binding;
  if (!binding || typeof binding !== "object") {
    throw new ApiError("The server returned an invalid sender receipt", 502);
  }
  const row = binding as Record<string, unknown>;
  const sender = (row.sender as Record<string, unknown> | null) ?? {};
  const transport = (row.transport as Record<string, unknown> | null) ?? {};
  if (
    typeof row.connection_id !== "string" ||
    typeof row.auth_revision !== "number" ||
    typeof row.link_revision !== "number" ||
    typeof row.state !== "string" ||
    typeof row.digest !== "string" ||
    typeof sender.name !== "string" ||
    typeof sender.address !== "string" ||
    typeof transport.host !== "string" ||
    typeof transport.port !== "number" ||
    typeof transport.tls_mode !== "string"
  ) {
    throw new ApiError("The server returned an invalid sender receipt", 502);
  }
  return {
    connectionId: row.connection_id,
    authRevision: row.auth_revision,
    linkRevision: row.link_revision,
    state: row.state,
    digest: row.digest,
    sender: { name: sender.name, address: sender.address },
    transport: {
      host: transport.host,
      port: transport.port,
      tlsMode: transport.tls_mode,
      username: typeof transport.username === "string" ? transport.username : "",
    },
  };
}

/** The one-recipient test-send receipt — SMTP acceptance only. */
export interface TestSendReceipt {
  accepted: boolean;
  smtpCode: number | null;
  smtpMessage: string;
  to: string;
  contentRevision: number;
  contentDigest: string;
  linkDigest: string;
  deliveryClaim: string;
}

export function parseTestSendReceipt(value: unknown): TestSendReceipt {
  const receipt = (value as { receipt?: unknown } | null)?.receipt;
  if (!receipt || typeof receipt !== "object") {
    throw new ApiError("The server returned an invalid test-send receipt", 502);
  }
  const row = receipt as Record<string, unknown>;
  if (typeof row.accepted !== "boolean" || typeof row.content_digest !== "string") {
    throw new ApiError("The server returned an invalid test-send receipt", 502);
  }
  return {
    accepted: row.accepted,
    smtpCode: typeof row.smtp_code === "number" ? row.smtp_code : null,
    smtpMessage: typeof row.smtp_message === "string" ? row.smtp_message : "",
    to: typeof row.to_email === "string" ? row.to_email : "",
    contentRevision: typeof row.content_revision === "number" ? row.content_revision : -1,
    contentDigest: row.content_digest,
    linkDigest: typeof row.link_digest === "string" ? row.link_digest : "",
    deliveryClaim: typeof row.delivery_claim === "string" ? row.delivery_claim : DELIVERY_CLAIM,
  };
}

/** The frozen send row every prepare/show/approve returns. */
export interface SendRow {
  sendId: string;
  campaignId: string;
  state: string;
  sendDigest: string;
  contentRevision: number;
  contentDigest: string;
  audienceFreezeId: string;
  audienceDigest: string;
  connectionId: string;
  linkRevision: number;
  maxRecipients: number;
  unsubscribeOrigin: string;
  closeReason: string | null;
  created: number;
  approvedAt: number | null;
}

export function parseSendRow(value: unknown): SendRow {
  const send = (value as { send?: unknown } | null)?.send ?? value;
  if (!send || typeof send !== "object") {
    throw new ApiError("The server returned an invalid send receipt", 502);
  }
  const row = send as Record<string, unknown>;
  if (
    typeof row.send_id !== "string" ||
    typeof row.campaign_id !== "string" ||
    typeof row.state !== "string" ||
    typeof row.send_digest !== "string"
  ) {
    throw new ApiError("The server returned an invalid send receipt", 502);
  }
  return {
    sendId: row.send_id,
    campaignId: row.campaign_id,
    state: row.state,
    sendDigest: row.send_digest,
    contentRevision: typeof row.content_revision === "number" ? row.content_revision : -1,
    contentDigest: typeof row.content_digest === "string" ? row.content_digest : "",
    audienceFreezeId: typeof row.audience_freeze_id === "string" ? row.audience_freeze_id : "",
    audienceDigest: typeof row.audience_digest === "string" ? row.audience_digest : "",
    connectionId: typeof row.connection_id === "string" ? row.connection_id : "",
    linkRevision: typeof row.link_revision === "number" ? row.link_revision : -1,
    maxRecipients: typeof row.max_recipients === "number" ? row.max_recipients : -1,
    unsubscribeOrigin: typeof row.unsubscribe_origin === "string" ? row.unsubscribe_origin : "",
    closeReason: typeof row.close_reason === "string" ? row.close_reason : null,
    created: typeof row.created === "number" ? row.created : 0,
    approvedAt: typeof row.approved_at === "number" ? row.approved_at : null,
  };
}

/** `prepare` plus its counts and bounded masked sample. */
export interface PreparedSend {
  send: SendRow;
  counts: {
    included: number;
    excluded: number;
    suppressedNow: number;
    final: number;
    maxRecipients: number;
  };
  sample: { customerId: string; email: string }[];
  sendDigest: string;
}

export function parsePreparedSend(value: unknown): PreparedSend {
  const row = (value as Record<string, unknown> | null) ?? {};
  const counts = (row.counts as Record<string, unknown> | null) ?? {};
  const sample = Array.isArray(row.sample) ? row.sample : [];
  if (typeof row.send_digest !== "string" || row.send === undefined) {
    throw new ApiError("The server returned an invalid send receipt", 502);
  }
  return {
    send: parseSendRow(row.send),
    counts: {
      included: typeof counts.included === "number" ? counts.included : -1,
      excluded: typeof counts.excluded === "number" ? counts.excluded : -1,
      suppressedNow: typeof counts.suppressed_now === "number" ? counts.suppressed_now : -1,
      final: typeof counts.final === "number" ? counts.final : -1,
      maxRecipients: typeof counts.max_recipients === "number" ? counts.max_recipients : -1,
    },
    sample: sample.flatMap((entry) => {
      const item = (entry as Record<string, unknown> | null) ?? {};
      return typeof item.customer_id === "string" && typeof item.email === "string"
        ? [{ customerId: item.customer_id, email: item.email }]
        : [];
    }),
    sendDigest: row.send_digest,
  };
}

/** Per-recipient outcome counts as the host reports them. */
export interface SendCounts {
  queued: number;
  submitting: number;
  accepted: number;
  failed: number;
  uncertain: number;
  suppressed: number;
  closed: number;
}

function parseCounts(value: unknown): SendCounts {
  const row = (value as Record<string, unknown> | null) ?? {};
  const n = (key: string) => (typeof row[key] === "number" ? (row[key] as number) : 0);
  return {
    queued: n("queued"),
    submitting: n("submitting"),
    accepted: n("accepted"),
    failed: n("failed"),
    uncertain: n("uncertain"),
    suppressed: n("suppressed"),
    closed: n("closed"),
  };
}

/** One masked delivery row — the show/list wire never carries a
 *  full address. */
export interface DeliveryRow {
  sendId: string;
  customerId: string;
  email: string;
  state: string;
  attempts: number;
  smtpCode: number | null;
  reason: string | null;
  resolvedBy: string | null;
}

function parseDelivery(value: unknown): DeliveryRow {
  const row = (value as Record<string, unknown> | null) ?? {};
  return {
    sendId: typeof row.send_id === "string" ? row.send_id : "",
    customerId: typeof row.customer_id === "string" ? row.customer_id : "",
    email: typeof row.email === "string" ? row.email : "",
    state: typeof row.state === "string" ? row.state : "",
    attempts: typeof row.attempts === "number" ? row.attempts : 0,
    smtpCode: typeof row.smtp_code === "number" ? row.smtp_code : null,
    reason: typeof row.reason === "string" ? row.reason : null,
    resolvedBy: typeof row.resolved_by === "string" ? row.resolved_by : null,
  };
}

/** `show`/`approve` — the send, live counts and masked deliveries. */
export interface SendView {
  send: SendRow;
  counts: SendCounts;
  deliveries: DeliveryRow[];
  deliveryClaim: string;
}

export function parseSendView(value: unknown): SendView {
  const row = (value as Record<string, unknown> | null) ?? {};
  if (row.send === undefined) {
    throw new ApiError("The server returned an invalid send receipt", 502);
  }
  return {
    send: parseSendRow(row.send),
    counts: parseCounts(row.counts),
    deliveries: Array.isArray(row.deliveries) ? row.deliveries.map(parseDelivery) : [],
    deliveryClaim: typeof row.delivery_claim === "string" ? row.delivery_claim : DELIVERY_CLAIM,
  };
}

/** One `list` row — the send plus its counts, no deliveries. */
export interface SendListEntry {
  sendId: string;
  campaignId: string;
  state: string;
  sendDigest: string;
  created: number;
  approvedAt: number | null;
  counts: SendCounts;
  deliveryClaim: string;
}

export function parseSendList(value: unknown): SendListEntry[] {
  const rows = (value as { sends?: unknown } | null)?.sends;
  if (!Array.isArray(rows)) {
    throw new ApiError("The server returned an invalid send receipt", 502);
  }
  return rows.map((entry) => {
    const row = (entry as Record<string, unknown> | null) ?? {};
    return {
      sendId: typeof row.send_id === "string" ? row.send_id : "",
      campaignId: typeof row.campaign_id === "string" ? row.campaign_id : "",
      state: typeof row.state === "string" ? row.state : "",
      sendDigest: typeof row.send_digest === "string" ? row.send_digest : "",
      created: typeof row.created === "number" ? row.created : 0,
      approvedAt: typeof row.approved_at === "number" ? row.approved_at : null,
      counts: parseCounts(row.counts),
      deliveryClaim: typeof row.delivery_claim === "string" ? row.delivery_claim : DELIVERY_CLAIM,
    };
  });
}

/** `GET /api/crm-send/origin` — the effective unsubscribe origin
 *  plus whether it is an operator-stored override. */
export interface OriginReceipt {
  unsubscribeOrigin: string | null;
  stored: boolean;
}

export function parseOriginReceipt(value: unknown): OriginReceipt {
  const row = (value as Record<string, unknown> | null) ?? {};
  return {
    unsubscribeOrigin: typeof row.unsubscribe_origin === "string" ? row.unsubscribe_origin : null,
    stored: row.stored === true,
  };
}

/** The terminal states a send reaches — the progress poll stops on
 *  any of these (and on unmount, and at its own bound). */
export function sendTerminal(state: string): boolean {
  return state === "completed" || state === "closed";
}

export const sendClient = {
  paths: sendPaths,
  DELIVERY_CLAIM,
  sendTerminal,
  /* ------------------------ CAD-785 sender binding ----------------------- */
  /** `POST /api/crm-smtp/show` — body is exactly
   *  `{install_id, context_id}`. The host 409s "no SMTP sender is
   *  bound…" when none is live; callers catch that as `null`. */
  smtpShow(scope: SendScope): Promise<unknown> {
    assertScope(scope);
    const body: Record<string, unknown> = {
      install_id: scope.installId,
      context_id: scope.contextId,
    };
    assertClean(body, SMTP_SHOW_KEYS, "smtp show");
    return post(sendPaths.smtpShow, body);
  },
  /** `POST /api/crm-smtp/bind` — body is exactly
   *  `{install_id, context_id, connection_id, request_id}`. */
  smtpBind(scope: SendScope, connectionId: string, requestId: string): Promise<unknown> {
    assertScope(scope);
    assertRecordId(connectionId);
    assertRecordId(requestId);
    const body: Record<string, unknown> = {
      install_id: scope.installId,
      context_id: scope.contextId,
      connection_id: connectionId,
      request_id: requestId,
    };
    assertClean(body, SMTP_BIND_KEYS, "smtp bind");
    return post(sendPaths.smtpBind, body);
  },
  /** `POST /api/crm-smtp/rebind` — body is exactly
   *  `{install_id, context_id, connection_id, expected_revision}`. */
  smtpRebind(
    scope: SendScope,
    connectionId: string,
    expectedRevision: number,
  ): Promise<unknown> {
    assertScope(scope);
    assertRecordId(connectionId);
    assertRevision(expectedRevision);
    const body: Record<string, unknown> = {
      install_id: scope.installId,
      context_id: scope.contextId,
      connection_id: connectionId,
      expected_revision: expectedRevision,
    };
    assertClean(body, SMTP_REBIND_KEYS, "smtp rebind");
    return post(sendPaths.smtpRebind, body);
  },
  /** `POST /api/crm-smtp/revoke` — body is exactly
   *  `{install_id, context_id, expected_revision}`. */
  smtpRevoke(scope: SendScope, expectedRevision: number): Promise<unknown> {
    assertScope(scope);
    assertRevision(expectedRevision);
    const body: Record<string, unknown> = {
      install_id: scope.installId,
      context_id: scope.contextId,
      expected_revision: expectedRevision,
    };
    assertClean(body, SMTP_REVOKE_KEYS, "smtp revoke");
    return post(sendPaths.smtpRevoke, body);
  },
  /** `POST /api/crm-smtp/test-send` — one operator-typed address,
   *  body exactly `{install_id, context_id, campaign_id, to_email}`.
   *  The receipt records SMTP acceptance/refusal only — never
   *  delivery, never reads. */
  smtpTestSend(scope: SendScope, campaignId: string, toEmail: string): Promise<unknown> {
    assertScope(scope);
    assertRecordId(campaignId);
    const trimmed = toEmail.trim();
    if (
      trimmed === "" ||
      trimmed.length > 254 ||
      !trimmed.includes("@") ||
      /[\s<>"'`;,()[\]\\]/.test(trimmed)
    ) {
      throw new ApiError("SMTP test recipient must be one operator address", 400);
    }
    const body: Record<string, unknown> = {
      install_id: scope.installId,
      context_id: scope.contextId,
      campaign_id: campaignId,
      to_email: trimmed,
    };
    assertClean(body, SMTP_TEST_KEYS, "test send");
    return post(sendPaths.smtpTestSend, body);
  },
  /* ------------------------ CAD-786 bounded send ------------------------- */
  /** `POST /api/crm-send/prepare` — body is exactly
   *  `{install_id, context_id, campaign_id, audience_freeze_id,
   *  request_id}`. The host refuses on stale content, invalid freeze,
   *  missing binding, missing origin or missing accepted test send. */
  sendPrepare(
    scope: SendScope,
    campaignId: string,
    audienceFreezeId: string,
    requestId: string,
  ): Promise<unknown> {
    assertScope(scope);
    assertRecordId(campaignId);
    assertRecordId(audienceFreezeId);
    assertRecordId(requestId);
    const body: Record<string, unknown> = {
      install_id: scope.installId,
      context_id: scope.contextId,
      campaign_id: campaignId,
      audience_freeze_id: audienceFreezeId,
      request_id: requestId,
    };
    assertClean(body, SEND_PREPARE_KEYS, "send prepare");
    return post(sendPaths.sendPrepare, body);
  },
  /** `POST /api/crm-send/approve` — body is exactly
   *  `{install_id, context_id, send_id, send_digest}` — the digest
   *  the prepared view showed, never recomputed in the browser. */
  sendApprove(scope: SendScope, sendId: string, sendDigest: string): Promise<unknown> {
    assertScope(scope);
    assertRecordId(sendId);
    const body: Record<string, unknown> = {
      install_id: scope.installId,
      context_id: scope.contextId,
      send_id: sendId,
      send_digest: sendDigest,
    };
    assertClean(body, SEND_APPROVE_KEYS, "send approve");
    return post(sendPaths.sendApprove, body);
  },
  /** `POST /api/crm-send/resolve` — body is exactly
   *  `{install_id, context_id, send_id, customer_id, resolution}`
   *  with resolution "accepted" | "failed" — the only states an
   *  `uncertain` row may take. Never resends. */
  sendResolve(
    scope: SendScope,
    sendId: string,
    customerId: string,
    resolution: "accepted" | "failed",
  ): Promise<unknown> {
    assertScope(scope);
    assertRecordId(sendId);
    assertRecordId(customerId);
    if (resolution !== "accepted" && resolution !== "failed") {
      throw new ApiError("resolution must be 'accepted' or 'failed'", 400);
    }
    const body: Record<string, unknown> = {
      install_id: scope.installId,
      context_id: scope.contextId,
      send_id: sendId,
      customer_id: customerId,
      resolution,
    };
    assertClean(body, SEND_RESOLVE_KEYS, "send resolve");
    return post(sendPaths.sendResolve, body);
  },
  /** `GET /api/crm-send/show?install_id=…&context_id=…&send_id=…`. */
  sendShow(scope: SendScope, sendId: string): Promise<unknown> {
    assertScope(scope);
    assertRecordId(sendId);
    return get(
      `${sendPaths.sendShow}?install_id=${part(scope.installId)}&context_id=${part(
        scope.contextId,
      )}&send_id=${part(sendId)}`,
    );
  },
  /** `GET /api/crm-send/list?install_id=…&context_id=…[&campaign_id=…]`. */
  sendList(scope: SendScope, campaignId?: string): Promise<unknown> {
    assertScope(scope);
    let url = `${sendPaths.sendList}?install_id=${part(scope.installId)}&context_id=${part(
      scope.contextId,
    )}`;
    if (campaignId !== undefined) {
      assertRecordId(campaignId);
      url += `&campaign_id=${part(campaignId)}`;
    }
    return get(url);
  },
  /** `GET /api/crm-send/origin` — the effective unsubscribe origin. */
  sendOriginShow(): Promise<unknown> {
    return get(sendPaths.sendOrigin);
  },
  /** `POST /api/crm-send/origin` — body is exactly
   *  `{unsubscribe_origin}`; `null` clears the stored override.
   *  Client shape mirrors the host: https anywhere, http on a
   *  loopback host, no path/query/fragment/credentials, ≤200 bytes.
   *  Host refusals surface verbatim. */
  sendOriginSet(origin: string | null): Promise<unknown> {
    if (origin !== null) {
      const trimmed = origin.trim().replace(/\/+$/, "");
      // Host rule: https anywhere or http on a loopback host, no
      // path beyond "/", no query/fragment/credentials, ≤200 bytes.
      const rest = trimmed.replace(/^https?:\/\//, "");
      const slashAt = rest.indexOf("/");
      const ok =
        trimmed.length > 0 &&
        trimmed.length <= 200 &&
        (/^https:\/\//.test(trimmed) || /^http:\/\//.test(trimmed)) &&
        !/[?#@]/.test(rest) &&
        (slashAt === -1 || rest.slice(slashAt) === "/") &&
        (/^https:\/\//.test(trimmed)
          ? rest.replace(/\/$/, "").length > 0
          : /^http:\/\/(localhost|127\.0\.0\.1|\[::1\])(:\d+)?\/?$/.test(trimmed));
      if (!ok) {
        throw new ApiError(
          "unsubscribe origin must be https anywhere (or http on a loopback host), with no path, query, fragment or credentials, and at most 200 bytes",
          400,
        );
      }
      const body: Record<string, unknown> = { unsubscribe_origin: trimmed };
      assertClean(body, SEND_ORIGIN_KEYS, "origin set");
      return post(sendPaths.sendOrigin, body);
    }
    const body: Record<string, unknown> = { unsubscribe_origin: null };
    assertClean(body, SEND_ORIGIN_KEYS, "origin clear");
    return post(sendPaths.sendOrigin, body);
  },
};

/** Server refusals stay verbatim; transport failures name the retry. */
export function friendlySendError(error: unknown): string {
  return hostErrorText(error, "The send request was refused — retry.");
}

/** `smtpShow`'s "none bound" refusal is the host's report of an
 *  empty state, not a transport failure — callers map it to null. */
export function isNoSenderBound(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    (error.message.includes("no SMTP sender is bound") ||
      error.message.includes("sender binding is revoked") ||
      error.message.includes("sender refused or unavailable"))
  );
}
