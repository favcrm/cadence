import { ApiError } from "../../lib/api";
import { hostErrorText } from "./shared/hostErrors";

/**
 * Campaign content grammar mirrors (CAD-782 bounds, CAD-784 screens).
 * Subject/preheader plus bounded heading/paragraph/button blocks, the
 * one approved `{{first_name|Fallback}}` token, and https button URLs.
 * Client checks refuse malformed drafts before the wire; the host
 * stays the source of truth and its refusals surface verbatim.
 */

export type CampaignBlock =
  | { type: "heading"; text: string }
  | { type: "paragraph"; text: string }
  | { type: "button"; label: string; url: string };

export const BLOCK_TYPES = [
  { value: "heading", label: "Heading" },
  { value: "paragraph", label: "Paragraph" },
  { value: "button", label: "Button" },
] as const;

export function isBlockType(value: string): value is CampaignBlock["type"] {
  return value === "heading" || value === "paragraph" || value === "button";
}

export function checkCampaignId(id: string): void {
  if (id.length < 1 || id.length > 128 || !/^[A-Za-z0-9_-]+$/.test(id)) {
    throw new ApiError("campaign ID uses letters, digits, - and _ (1–128)", 400);
  }
}

function checkTokens(text: string): void {
  let rest = text;
  for (;;) {
    const open = rest.indexOf("{{");
    if (open === -1) {
      if (rest.includes("}}") || rest.includes("{%") || rest.includes("{#")) {
        throw new ApiError("email personalization uses only {{first_name|Fallback}}", 400);
      }
      return;
    }
    const after = rest.slice(open + 2);
    const close = after.indexOf("}}");
    if (close === -1) {
      throw new ApiError("email personalization uses only {{first_name|Fallback}}", 400);
    }
    const token = after.slice(0, close);
    const parts = token.split("|");
    if (parts.length !== 2 || parts[0] !== "first_name" || !fallbackShape(parts[1])) {
      throw new ApiError("email personalization uses only {{first_name|Fallback}}", 400);
    }
    rest = after.slice(close + 2);
  }
}

function fallbackShape(fallback: string): boolean {
  return (
    fallback.length > 0 &&
    fallback.length <= 40 &&
    /^[A-Za-z'’ \-]+$/.test(fallback)
  );
}

function checkText(text: string, bound: number, allowNewline: boolean, what: string): void {
  if (text.length === 0 || text.length > bound) {
    throw new ApiError(`${what} exceeds its supported shape or bounds`, 400);
  }
  for (const ch of text) {
    if (ch === "\n" && allowNewline) continue;
    if (ch.charCodeAt(0) < 32 || ch.charCodeAt(0) === 127) {
      throw new ApiError(`${what} exceeds its supported shape or bounds`, 400);
    }
  }
  if (text.includes("<") || text.includes(">") || text.includes("`")) {
    throw new ApiError(`${what} carries markup outside the email grammar`, 400);
  }
  if (text.includes("{") || text.includes("}")) checkTokens(text);
  const lower = text.toLowerCase();
  for (const marker of [
    "javascript:",
    "data:",
    "vbscript:",
    "onerror=",
    "onload=",
    "onclick=",
    "<script",
    "expression(",
  ]) {
    if (lower.includes(marker)) {
      throw new ApiError(`${what} carries markup outside the email grammar`, 400);
    }
  }
}

function checkUrl(url: string): void {
  if (
    url.length === 0 ||
    url.length > 500 ||
    [...url].some((ch) => ch.charCodeAt(0) < 33 || ch.charCodeAt(0) === 127) ||
    /\s/.test(url)
  ) {
    throw new ApiError("button URL exceeds its supported shape or bounds", 400);
  }
  const rest = url.startsWith("https://") ? url.slice("https://".length) : null;
  if (rest === null) {
    throw new ApiError("button URLs are https only", 400);
  }
  const host = rest.split(/[\/?#]/, 1)[0];
  if (!host.includes(".") || host.includes("@") || host.includes(":")) {
    throw new ApiError("button URL needs a dotted https host", 400);
  }
  if (!/^[A-Za-z0-9.-]+$/.test(host)) {
    throw new ApiError("button URL needs a dotted https host", 400);
  }
  if (/[<>'"`\\{}]/.test(url)) {
    throw new ApiError("button URL exceeds its supported shape or bounds", 400);
  }
}

/** Client mirror of the host draft grammar. */
export function checkContent(subject: string, preheader: string, blocks: CampaignBlock[]): void {
  if (subject.length === 0 || subject.length > 150 || subject.trim() !== subject) {
    throw new ApiError("subject is required (150 characters, no padding)", 400);
  }
  checkText(subject, 150, false, "subject");
  if (preheader.length > 200) {
    throw new ApiError("preheader exceeds its supported shape or bounds", 400);
  }
  if (preheader.length > 0) checkText(preheader, 200, false, "preheader");
  if (blocks.length === 0 || blocks.length > 12) {
    throw new ApiError("content needs 1 to 12 blocks", 400);
  }
  for (const block of blocks) {
    if (!isBlockType(block.type)) {
      throw new ApiError("content blocks are heading, paragraph or button", 400);
    }
    if (block.type === "heading") checkText(block.text, 120, false, "heading");
    else if (block.type === "paragraph") checkText(block.text, 2000, true, "paragraph");
    else {
      checkText(block.label, 60, false, "button label");
      checkUrl(block.url);
    }
  }
}

/** Append the approved token with a fallback to a text field. */
export function withToken(text: string, fallback: string): string {
  const clean = fallback.trim() === "" ? "Friend" : fallback.trim();
  return `${text}{{first_name|${clean}}}`;
}

export interface ContentDoc {
  campaignId: string;
  revision: number;
  subject: string;
  preheader: string;
  blocks: CampaignBlock[];
  contentDigest: string;
  approval: { revision: number | null; digest: string | null; valid: boolean; scope: string };
}

export function parseContentDoc(value: unknown): ContentDoc {
  const doc = (value as { content?: unknown } | null)?.content;
  if (!doc || typeof doc !== "object") {
    throw new ApiError("The server returned an invalid campaign receipt", 502);
  }
  const row = doc as Record<string, unknown>;
  const approval = (row.approval as Record<string, unknown> | null) ?? {};
  if (
    typeof row.campaign_id !== "string" ||
    typeof row.revision !== "number" ||
    typeof row.subject !== "string" ||
    typeof row.preheader !== "string" ||
    !Array.isArray(row.blocks) ||
    typeof row.content_digest !== "string"
  ) {
    throw new ApiError("The server returned an invalid campaign receipt", 502);
  }
  return {
    campaignId: row.campaign_id,
    revision: row.revision,
    subject: row.subject,
    preheader: row.preheader,
    blocks: row.blocks as CampaignBlock[],
    contentDigest: row.content_digest,
    approval: {
      revision: typeof approval.revision === "number" ? approval.revision : null,
      digest: typeof approval.digest === "string" ? approval.digest : null,
      valid: approval.valid === true,
      scope: typeof approval.scope === "string" ? approval.scope : "content-only",
    },
  };
}

export function parseContentList(value: unknown): ContentDoc[] {
  const rows = (value as { contents?: unknown } | null)?.contents;
  if (!Array.isArray(rows)) {
    throw new ApiError("The server returned an invalid campaign receipt", 502);
  }
  return rows.map((row) => parseContentDoc({ content: row }));
}

export interface ContentRender {
  revision: number;
  contentDigest: string;
  html: string;
  text: string;
  previewOnly: boolean;
  sender: { name: string; address: string };
  unsubscribeUrl: string;
  bindingId: string;
}

export function parseRender(value: unknown): ContentRender {
  const render = (value as { render?: unknown } | null)?.render;
  if (!render || typeof render !== "object") {
    throw new ApiError("The server returned an invalid render receipt", 502);
  }
  const row = render as Record<string, unknown>;
  const sender = (row.sender as Record<string, unknown> | null) ?? {};
  if (
    typeof row.revision !== "number" ||
    typeof row.content_digest !== "string" ||
    typeof row.html !== "string" ||
    typeof row.text !== "string" ||
    typeof sender.name !== "string" ||
    typeof sender.address !== "string" ||
    typeof row.unsubscribe_url !== "string"
  ) {
    throw new ApiError("The server returned an invalid render receipt", 502);
  }
  const binding = (row.binding as Record<string, unknown> | null) ?? {};
  return {
    revision: row.revision,
    contentDigest: row.content_digest,
    html: row.html,
    text: row.text,
    previewOnly: row.preview_only === true,
    sender: { name: sender.name, address: sender.address },
    unsubscribeUrl: row.unsubscribe_url,
    bindingId: typeof binding.binding_id === "string" ? binding.binding_id : "preview",
  };
}

/** CAD-1016: the before-Apply preview of a pending assistant draft —
 *  `app_content_proposal_render` renders the inert proposal's own
 *  subject/preheader/blocks through the same safe renderer, bound to
 *  `proposal_id`/`source_revision` (the draft's stamp, never a saved
 *  revision), `preview_only:true`/`send_ready:false` always. Carries
 *  no `revision` — a proposal is unsaved by definition. */
export interface ProposalRenderDoc {
  proposalId: string;
  sourceRevision: number;
  state: string;
  html: string;
  text: string;
  previewOnly: boolean;
  sendReady: boolean;
  sender: { name: string; address: string };
  unsubscribeUrl: string;
  bindingId: string;
  contentDigest: string;
}

export function parseProposalRender(value: unknown): ProposalRenderDoc {
  const render = (value as { render?: unknown } | null)?.render;
  if (!render || typeof render !== "object") {
    throw new ApiError("The server returned an invalid proposal render receipt", 502);
  }
  const row = render as Record<string, unknown>;
  const sender = (row.sender as Record<string, unknown> | null) ?? {};
  if (
    typeof row.proposal_id !== "string" ||
    typeof row.source_revision !== "number" ||
    typeof row.state !== "string" ||
    typeof row.html !== "string" ||
    typeof row.text !== "string" ||
    typeof sender.name !== "string" ||
    typeof sender.address !== "string" ||
    typeof row.unsubscribe_url !== "string"
  ) {
    throw new ApiError("The server returned an invalid proposal render receipt", 502);
  }
  const binding = (row.binding as Record<string, unknown> | null) ?? {};
  return {
    proposalId: row.proposal_id,
    sourceRevision: row.source_revision,
    state: row.state,
    html: row.html,
    text: row.text,
    previewOnly: row.preview_only === true,
    sendReady: row.send_ready === true,
    sender: { name: sender.name, address: sender.address },
    unsubscribeUrl: row.unsubscribe_url,
    bindingId: typeof binding.binding_id === "string" ? binding.binding_id : "preview",
    contentDigest: typeof row.content_digest === "string" ? row.content_digest : "",
  };
}

/** CAD-813: the host-stamped assistant provenance on a proposal.
 *  Every field is server-typed from the durable receipt — the
 *  assistant's claim is never the authority. */
export interface AssistantReceipt {
  messageId: string;
  agent: string;
  requestId: string;
  installId: string;
  contextId: string;
  campaignId: string;
  sourceRevision: number;
}

export function parseAssistantReceipt(value: unknown): AssistantReceipt | null {
  if (!value || typeof value !== "object") return null;
  const row = value as Record<string, unknown>;
  if (
    typeof row.message_id !== "string" ||
    typeof row.agent !== "string" ||
    typeof row.request_id !== "string" ||
    typeof row.install_id !== "string" ||
    typeof row.context_id !== "string" ||
    typeof row.campaign_id !== "string" ||
    typeof row.source_revision !== "number"
  ) {
    return null;
  }
  return {
    messageId: row.message_id,
    agent: row.agent,
    requestId: row.request_id,
    installId: row.install_id,
    contextId: row.context_id,
    campaignId: row.campaign_id,
    sourceRevision: row.source_revision,
  };
}

export interface ProposalDoc {
  proposalId: string;
  campaignId: string;
  sourceRevision: number;
  subject: string;
  preheader: string;
  /** The inert draft's bounded blocks — the proposal body the host
   *  renders for the pre-Apply preview (CAD-1016). */
  blocks: CampaignBlock[];
  state: string;
  actor: string;
  origin: string;
  /** Non-null only on host-verified assistant proposals (CAD-813). */
  assistantReceipt: AssistantReceipt | null;
}

export function parseProposal(value: unknown): ProposalDoc {
  const doc = (value as { proposal?: unknown } | null)?.proposal;
  if (!doc || typeof doc !== "object") {
    throw new ApiError("The server returned an invalid proposal receipt", 502);
  }
  const row = doc as Record<string, unknown>;
  if (
    typeof row.proposal_id !== "string" ||
    typeof row.campaign_id !== "string" ||
    typeof row.source_revision !== "number" ||
    typeof row.subject !== "string" ||
    typeof row.state !== "string" ||
    !Array.isArray(row.blocks)
  ) {
    throw new ApiError("The server returned an invalid proposal receipt", 502);
  }
  return {
    proposalId: row.proposal_id,
    campaignId: row.campaign_id,
    sourceRevision: row.source_revision,
    subject: row.subject,
    preheader: typeof row.preheader === "string" ? row.preheader : "",
    blocks: row.blocks as CampaignBlock[],
    state: row.state,
    actor: typeof row.actor === "string" ? row.actor : "operator",
    origin: typeof row.origin === "string" ? row.origin : "operator-direct",
    assistantReceipt: parseAssistantReceipt(row.assistant_receipt),
  };
}

/** CAD-813: the minted request's host stamp — campaign and source
 *  revision are read back exactly as the daemon recorded them. */
export interface ProposalRequestDoc {
  requestId: string;
  campaignId: string;
  sourceRevision: number;
  messageId: string;
  state: string;
  usedBy: string | null;
}

export function parseProposalRequest(value: unknown): ProposalRequestDoc {
  const doc = (value as { request?: unknown } | null)?.request;
  if (!doc || typeof doc !== "object") {
    throw new ApiError("The server returned an invalid proposal request receipt", 502);
  }
  const row = doc as Record<string, unknown>;
  if (
    typeof row.request_id !== "string" ||
    typeof row.campaign_id !== "string" ||
    typeof row.source_revision !== "number" ||
    typeof row.message_id !== "string" ||
    typeof row.state !== "string"
  ) {
    throw new ApiError("The server returned an invalid proposal request receipt", 502);
  }
  return {
    requestId: row.request_id,
    campaignId: row.campaign_id,
    sourceRevision: row.source_revision,
    messageId: row.message_id,
    state: row.state,
    usedBy: typeof row.used_by === "string" ? row.used_by : null,
  };
}

/** A fresh client-chosen request id — identifier-safe, never a
 *  provenance claim (the daemon stamps scope and revision). */
export function newRequestId(): string {
  try {
    return `req-${crypto.randomUUID().replaceAll("-", "").slice(0, 24)}`;
  } catch {
    // No crypto.getRandomValues fallback needed beyond this — a
    // request id only needs collision-freedom, not entropy claims.
    return `req-${Date.now().toString(36)}${Math.floor(Math.random() * 36 ** 8).toString(36)}`;
  }
}

export function parseProposalList(value: unknown): ProposalDoc[] {
  const rows = (value as { proposals?: unknown } | null)?.proposals;
  if (!Array.isArray(rows)) {
    throw new ApiError("The server returned an invalid proposal receipt", 502);
  }
  return rows.map((row) => parseProposal({ proposal: row }));
}

/** Server refusals stay generic; transport failures name the retry. */
export function friendlyCampaignError(error: unknown): string {
  return hostErrorText(error, "The campaign request was refused — retry.");
}
