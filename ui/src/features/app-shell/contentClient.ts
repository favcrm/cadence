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
 * `deny_unknown_fields` refuses it again. No actor claims, HTML,
 * SMTP secrets or project links ever enter this client. Authority
 * stays server-checked; the assistant's proposal is inert until the
 * operator explicitly applies it, and the sender/unsubscribe/footer
 * material is host-locked — never an editable field here.
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
  "expected_revision",
] as const;
const RENDER_KEYS = ["revision", "sample_first_name"] as const;
const APPROVE_KEYS = ["expected_revision"] as const;
const TEST_PREPARE_KEYS = ["to_email"] as const;
const SEND_PREPARE_KEYS = ["audience_freeze_id"] as const;
const PROPOSE_KEYS = [
  "campaign_id",
  "proposal_id",
  "subject",
  "preheader",
  "blocks",
] as const;
const APPLY_KEYS = ["expected_revision"] as const;

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
  proposalListPath: (scope: ContentScope) => `${scopePath(scope)}/proposals/list`,
  proposalPath: (scope: ContentScope, proposalId: string) =>
    `${scopePath(scope)}/proposals/${proposalId}`,
  proposalApplyPath: (scope: ContentScope, proposalId: string) =>
    `${scopePath(scope)}/proposals/${proposalId}/apply`,
  proposalDiscardPath: (scope: ContentScope, proposalId: string) =>
    `${scopePath(scope)}/proposals/${proposalId}/discard`,
};

async function post<T>(path: string, body: Record<string, unknown>): Promise<T> {
  const resp = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json", ...sessionHeaders() },
    body: JSON.stringify(body),
  });
  if (!resp.ok) throw new ApiError(`${resp.status} ${resp.statusText}`, resp.status);
  return (await resp.json()) as T;
}

async function get<T>(path: string): Promise<T> {
  const resp = await fetch(path, { headers: { ...sessionHeaders() } });
  if (!resp.ok) throw new ApiError(`${resp.status} ${resp.statusText}`, resp.status);
  return (await resp.json()) as T;
}

export interface ContentDraftInput {
  campaignId: string;
  subject: string;
  preheader?: string;
  blocks: ContentBlock[];
  expectedRevision?: number;
}

export interface ProposalInput {
  campaignId: string;
  proposalId: string;
  subject: string;
  preheader?: string;
  blocks: ContentBlock[];
}

/** Mount contract for CAD-781's outlet (deferred until its head merges):
 *  `CrmOutlet` renders `<CrmCompose scope campaignId />` inside the
 *  campaign route; this client stays mount-agnostic. */
export const contentClient = {
  paths: contentPaths,
  save(scope: ContentScope, input: ContentDraftInput): Promise<unknown> {
    assertInputClean(input as unknown as Record<string, unknown>, ["campaignId", "subject", "preheader", "blocks", "expectedRevision"], "save");
    const body: Record<string, unknown> = {
      campaign_id: input.campaignId,
      subject: input.subject,
      preheader: input.preheader ?? "",
      blocks: input.blocks,
    };
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
    opts?: { revision?: number; sampleFirstName?: string },
  ): Promise<unknown> {
    const body: Record<string, unknown> = {};
    if (opts?.revision !== undefined) body.revision = opts.revision;
    if (opts?.sampleFirstName !== undefined) body.sample_first_name = opts.sampleFirstName;
    assertClean(body, RENDER_KEYS, "render");
    return post(contentPaths.renderPath(scope, campaignId), body);
  },
  approve(scope: ContentScope, campaignId: string, expectedRevision: number): Promise<unknown> {
    const body: Record<string, unknown> = { expected_revision: expectedRevision };
    assertClean(body, APPROVE_KEYS, "approve");
    return post(contentPaths.approvePath(scope, campaignId), body);
  },
  testPrepare(scope: ContentScope, campaignId: string, toEmail: string): Promise<unknown> {
    const body: Record<string, unknown> = { to_email: toEmail };
    assertClean(body, TEST_PREPARE_KEYS, "test-prepare");
    return post(contentPaths.testPreparePath(scope, campaignId), body);
  },
  sendPrepare(
    scope: ContentScope,
    campaignId: string,
    audienceFreezeId?: string,
  ): Promise<unknown> {
    const body: Record<string, unknown> = {};
    if (audienceFreezeId !== undefined) body.audience_freeze_id = audienceFreezeId;
    assertClean(body, SEND_PREPARE_KEYS, "send-prepare");
    return post(contentPaths.sendPreparePath(scope, campaignId), body);
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
