import { ApiError } from "../../lib/api";
import { sessionHeaders } from "../../lib/sessionKey";

/**
 * Typed host action client for versioned campaign email content (CAD-782).
 *
 * Independent of CAD-781's record client: the installation and context
 * IDs always come from the trusted route/scope — never from a form
 * field, a chat message or a stored draft. Request bodies carry only
 * the operator-checked grammar the HTTP peer accepts; anything else
 * throws client-side before a byte is sent, and the server's
 * `deny_unknown_fields` refuses it again. No actor claims, receipt
 * claims (`assistant_receipt`, `turn_id`, `nonce`), HTML, SMTP
 * secrets or project links ever enter this client. Authority stays
 * server-checked; drafts submitted here are recorded as operator
 * work and stay inert until the operator explicitly applies them.
 * Sender/unsubscribe material travels only as typed preview-only
 * operator bindings; every render and test preparation is labelled
 * preview-only and final-send preparation refuses until CAD-785/786
 * supply host-verified evidence.
 */

/** URL-bound scope: the only identity this client will use. */
export interface ContentScope {
  installId: string;
  contextId: string;
}

export type ContentBlock =
  | { type: "heading"; text: string }
  | { type: "paragraph"; text: string }
  | { type: "button"; label: string; url: string };

/** Body keys the HTTP peer accepts, per operation. */
const SAVE_KEYS = [
  "campaign_id",
  "subject",
  "preheader",
  "blocks",
  "html",
  "text",
  "expected_revision",
] as const;
const RENDER_KEYS = ["revision", "sample_first_name", "binding_id"] as const;
const APPROVE_KEYS = ["expected_revision"] as const;
const TEST_PREPARE_KEYS = ["to_email", "binding_id"] as const;
const SEND_PREPARE_KEYS = ["binding_id", "audience_freeze_id"] as const;
const BINDING_SAVE_KEYS = [
  "binding_id",
  "sender_name",
  "sender_address",
  "unsubscribe_base",
  "connection_id",
  "expected_revision",
] as const;
const PROPOSE_KEYS = [
  "campaign_id",
  "proposal_id",
  "subject",
  "preheader",
  "blocks",
] as const;
const APPLY_KEYS = ["expected_revision"] as const;
// CAD-813: the operator's one-time proposal-request mint names only
// the campaign and the chat message the assistant's turn should
// answer. Campaign scope and the source revision are host-stamped by
// the daemon — the body can never carry a token, receipt, turn or
// source_revision claim.
const PROPOSAL_REQUEST_KEYS = ["campaign_id", "message_id", "request_id"] as const;

/** Authority-claiming fields that must never travel from the browser. */
const ALWAYS_FORBIDDEN = [
  "by",
  "actor",
  "workspace",
  "project",
  "project_link",
  "sql",
  "path",
  "secret",
  "install_id",
  "context_id",
] as const;

export const FORBIDDEN_CONTENT_KEYS = [...ALWAYS_FORBIDDEN] as const;

function assertClean(body: Record<string, unknown>, allowed: readonly string[], what: string): void {
  for (const key of Object.keys(body)) {
    if (!allowed.includes(key)) {
      throw new ApiError(`content ${what} carries a forbidden field`, 400, {
        code: "forbidden_field",
      });
    }
  }
}

function assertInputClean(input: Record<string, unknown>, allowed: readonly string[], what: string): void {
  for (const key of Object.keys(input)) {
    if (!allowed.includes(key)) {
      throw new ApiError(`content ${what} carries a forbidden field`, 400, {
        code: "forbidden_field",
      });
    }
  }
}

function scopePath(scope: ContentScope): string {
  return `/api/app-installations/${scope.installId}/contexts/${scope.contextId}/content`;
}

export const contentPaths = {
  savePath: (scope: ContentScope) => `${scopePath(scope)}/campaigns`,
  listPath: (scope: ContentScope) => `${scopePath(scope)}/campaigns/list`,
  showPath: (scope: ContentScope, campaignId: string) =>
    `${scopePath(scope)}/campaigns/${campaignId}`,
  renderPath: (scope: ContentScope, campaignId: string) =>
    `${scopePath(scope)}/campaigns/${campaignId}/render`,
  approvePath: (scope: ContentScope, campaignId: string) =>
    `${scopePath(scope)}/campaigns/${campaignId}/approve`,
  testPreparePath: (scope: ContentScope, campaignId: string) =>
    `${scopePath(scope)}/campaigns/${campaignId}/test-prepare`,
  sendPreparePath: (scope: ContentScope, campaignId: string) =>
    `${scopePath(scope)}/campaigns/${campaignId}/send-prepare`,
  proposePath: (scope: ContentScope) => `${scopePath(scope)}/proposals`,
  proposalRequestPath: (scope: ContentScope) => `${scopePath(scope)}/proposal-requests`,
  proposalListPath: (scope: ContentScope) => `${scopePath(scope)}/proposals/list`,
  proposalPath: (scope: ContentScope, proposalId: string) =>
    `${scopePath(scope)}/proposals/${proposalId}`,
  proposalApplyPath: (scope: ContentScope, proposalId: string) =>
    `${scopePath(scope)}/proposals/${proposalId}/apply`,
  proposalDiscardPath: (scope: ContentScope, proposalId: string) =>
    `${scopePath(scope)}/proposals/${proposalId}/discard`,
  proposalRenderPath: (scope: ContentScope, proposalId: string) =>
    `${scopePath(scope)}/proposals/${proposalId}/render`,
  bindingSavePath: (scope: ContentScope) => `${scopePath(scope)}/sender-bindings`,
  bindingListPath: (scope: ContentScope) => `${scopePath(scope)}/sender-bindings/list`,
  bindingPath: (scope: ContentScope, bindingId: string) =>
    `${scopePath(scope)}/sender-bindings/${bindingId}`,
};

async function post<T>(path: string, body: Record<string, unknown>): Promise<T> {
  const resp = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
    body: JSON.stringify(body),
  });
  // Writes surface the host refusal verbatim: a stale revision or a
  // refused draft must read as the server's reason, never a bare
  // status. The receipt is validated below; a missing body 502s.
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
  if (value === null) throw new ApiError("The server returned an invalid campaign receipt", 502);
  return value as T;
}

async function get<T>(path: string): Promise<T> {
  const resp = await fetch(path, { headers: { ...sessionHeaders() } });
  // Reads surface the host refusal verbatim (an unknown, foreign or
  // stale ID must read as the server's reason, never a bare status).
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
  if (value === null) throw new ApiError("The server returned an invalid campaign receipt", 502);
  return value as T;
}

/**
 * CAD-1056 operator save: exactly one of `blocks` (structured) or
 * `html` (pasted; the host sanitises it and stores only the result),
 * plus an optional plain-text override. The host always appends the
 * unsubscribe footer and any save resets approval.
 */
export type ContentDraftInput = {
  campaignId: string;
  subject: string;
  preheader?: string;
  text?: string;
  expectedRevision?: number;
} & (
  | { blocks: ContentBlock[]; html?: never }
  | { html: string; blocks?: never }
);

export interface ProposalInput {
  campaignId: string;
  proposalId: string;
  subject: string;
  preheader?: string;
  blocks: ContentBlock[];
}

/** Mount contract (CAD-784 owns visible campaign wiring):
 *  `CrmOutlet` renders `<CrmCompose scope campaignId />` inside the
 *  campaign route; this client stays mount-agnostic. */
export const contentClient = {
  paths: contentPaths,
  save(scope: ContentScope, input: ContentDraftInput): Promise<unknown> {
    assertInputClean(input as unknown as Record<string, unknown>, ["campaignId", "subject", "preheader", "blocks", "html", "text", "expectedRevision"], "save");
    if ((input.blocks === undefined) === (input.html === undefined)) {
      throw new ApiError("content save needs exactly one of blocks or html", 400, {
        code: "invalid_body",
      });
    }
    const body: Record<string, unknown> = {
      campaign_id: input.campaignId,
      subject: input.subject,
      preheader: input.preheader ?? "",
    };
    if (input.blocks !== undefined) body.blocks = input.blocks;
    if (input.html !== undefined) body.html = input.html;
    if (input.text !== undefined) body.text = input.text;
    if (input.expectedRevision !== undefined) body.expected_revision = input.expectedRevision;
    assertClean(body, SAVE_KEYS, "save");
    return post(contentPaths.savePath(scope), body);
  },
  show(scope: ContentScope, campaignId: string): Promise<unknown> {
    return get(contentPaths.showPath(scope, campaignId));
  },
  list(scope: ContentScope): Promise<unknown> {
    return get(contentPaths.listPath(scope));
  },
  render(
    scope: ContentScope,
    campaignId: string,
    opts?: { revision?: number; sampleFirstName?: string; bindingId?: string },
  ): Promise<unknown> {
    const body: Record<string, unknown> = {};
    if (opts?.revision !== undefined) body.revision = opts.revision;
    if (opts?.sampleFirstName !== undefined) body.sample_first_name = opts.sampleFirstName;
    if (opts?.bindingId !== undefined) body.binding_id = opts.bindingId;
    assertClean(body, RENDER_KEYS, "render");
    return post(contentPaths.renderPath(scope, campaignId), body);
  },
  approve(scope: ContentScope, campaignId: string, expectedRevision: number): Promise<unknown> {
    const body: Record<string, unknown> = { expected_revision: expectedRevision };
    assertClean(body, APPROVE_KEYS, "approve");
    return post(contentPaths.approvePath(scope, campaignId), body);
  },
  /** `GET …/proposals/<id>/render` — read transport (the proposal id is
   *  the only selector; a body is refused). The host renders the inert
   *  draft's own subject/preheader/blocks — never a save, never a send —
   *  returning proposal_id/source_revision/preview_only:true/send_ready:
   *  false (no saved `revision` — a proposal is unsaved by definition). */
  proposalRender(scope: ContentScope, proposalId: string): Promise<unknown> {
    return get(contentPaths.proposalRenderPath(scope, proposalId));
  },
  testPrepare(
    scope: ContentScope,
    campaignId: string,
    toEmail: string,
    bindingId?: string,
  ): Promise<unknown> {
    const body: Record<string, unknown> = { to_email: toEmail };
    if (bindingId !== undefined) body.binding_id = bindingId;
    assertClean(body, TEST_PREPARE_KEYS, "test-prepare");
    return post(contentPaths.testPreparePath(scope, campaignId), body);
  },
  sendPrepare(
    scope: ContentScope,
    campaignId: string,
    bindingId: string,
    audienceFreezeId?: string,
  ): Promise<unknown> {
    const body: Record<string, unknown> = { binding_id: bindingId };
    if (audienceFreezeId !== undefined) body.audience_freeze_id = audienceFreezeId;
    assertClean(body, SEND_PREPARE_KEYS, "send-prepare");
    return post(contentPaths.sendPreparePath(scope, campaignId), body);
  },
  bindingSave(
    scope: ContentScope,
    input: {
      bindingId: string;
      senderName: string;
      senderAddress: string;
      unsubscribeBase: string;
      connectionId?: string;
      expectedRevision?: number;
    },
  ): Promise<unknown> {
    assertInputClean(input as unknown as Record<string, unknown>, [
      "bindingId",
      "senderName",
      "senderAddress",
      "unsubscribeBase",
      "connectionId",
      "expectedRevision",
    ], "binding-save");
    const body: Record<string, unknown> = {
      binding_id: input.bindingId,
      sender_name: input.senderName,
      sender_address: input.senderAddress,
      unsubscribe_base: input.unsubscribeBase,
    };
    if (input.connectionId !== undefined) body.connection_id = input.connectionId;
    if (input.expectedRevision !== undefined) body.expected_revision = input.expectedRevision;
    assertClean(body, BINDING_SAVE_KEYS, "binding-save");
    return post(contentPaths.bindingSavePath(scope), body);
  },
  bindingShow(scope: ContentScope, bindingId: string): Promise<unknown> {
    return get(contentPaths.bindingPath(scope, bindingId));
  },
  bindingList(scope: ContentScope): Promise<unknown> {
    return get(contentPaths.bindingListPath(scope));
  },
  propose(scope: ContentScope, input: ProposalInput): Promise<unknown> {
    assertInputClean(input as unknown as Record<string, unknown>, ["campaignId", "proposalId", "subject", "preheader", "blocks"], "propose");
    const body: Record<string, unknown> = {
      campaign_id: input.campaignId,
      proposal_id: input.proposalId,
      subject: input.subject,
      preheader: input.preheader ?? "",
      blocks: input.blocks,
    };
    assertClean(body, PROPOSE_KEYS, "propose");
    return post(contentPaths.proposePath(scope), body);
  },
  /**
   * CAD-813: mint a one-time, host-stamped proposal request. The body
   * is exactly `{campaign_id, message_id, request_id}` — the daemon
   * re-proves the message's verified App binding and stamps campaign
   * and live source revision itself. The browser never sends token,
   * receipt, turn or source_revision fields; the transport's
   * `deny_unknown_fields` would refuse them anyway.
   */
  proposalRequest(
    scope: ContentScope,
    input: { campaignId: string; messageId: string; requestId: string },
  ): Promise<unknown> {
    assertInputClean(
      input as unknown as Record<string, unknown>,
      ["campaignId", "messageId", "requestId"],
      "proposal-request",
    );
    const body: Record<string, unknown> = {
      campaign_id: input.campaignId,
      message_id: input.messageId,
      request_id: input.requestId,
    };
    assertClean(body, PROPOSAL_REQUEST_KEYS, "proposal-request");
    return post(contentPaths.proposalRequestPath(scope), body);
  },
  proposalList(scope: ContentScope): Promise<unknown> {
    return get(contentPaths.proposalListPath(scope));
  },
  proposalShow(scope: ContentScope, proposalId: string): Promise<unknown> {
    return get(contentPaths.proposalPath(scope, proposalId));
  },
  proposalApply(
    scope: ContentScope,
    proposalId: string,
    expectedRevision?: number,
  ): Promise<unknown> {
    const body: Record<string, unknown> = {};
    if (expectedRevision !== undefined) body.expected_revision = expectedRevision;
    assertClean(body, APPLY_KEYS, "apply");
    return post(contentPaths.proposalApplyPath(scope, proposalId), body);
  },
  proposalDiscard(scope: ContentScope, proposalId: string): Promise<unknown> {
    return post(contentPaths.proposalDiscardPath(scope, proposalId), {});
  },
};
