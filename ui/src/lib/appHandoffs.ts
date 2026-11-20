import { ApiError, errorMessage } from "./apiError";
import { usingPrivateOperation, type PrivateOperation } from "./privateSession";
import { sessionHeaders } from "./sessionKey";
import type { ThreadRef } from "./types";

/** AgenticOS host contract DEMOCOMPANY-26. Not an authorization credential. */
export const APP_HANDOFF_HEADERS = { "X-Cadence-App-Handoff": "app-handoff-decision-v1" };
const REVIEW_HEADER = { "X-Cadence-App-Handoff-Review": "1" };
const ACTION_ID = /^[a-f0-9]{32}$/;
const APP_KEY = /^[a-z][a-z0-9_-]{0,63}$/;
const states = ["pending", "approved", "denied", "running", "unconfirmed", "unanswered", "refused"] as const;
export type AppHandoffState = (typeof states)[number] | "answered";
export type AppHandoffMutation = "approve" | "deny" | "run";
export type AppHandoffDecision = {
  id: string;
  app: string;
  recipient: string;
  question: string;
} & ({ state: Exclude<AppHandoffState, "answered"> } | { state: "answered"; answer: string });

function object(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function parseDecision(value: unknown): AppHandoffDecision {
  const row = object(value);
  const app = object(row?.app);
  const recipient = object(row?.recipient);
  if (
    !row || row.v !== 1 || row.kind !== "app_handoff" ||
    typeof row.action_id !== "string" || !ACTION_ID.test(row.action_id) ||
    !app || typeof app.key !== "string" || !APP_KEY.test(app.key) ||
    typeof app.title !== "string" || app.title.length < 1 || app.title.length > 200 ||
    !recipient || typeof recipient.name !== "string" || recipient.name.length < 1 || recipient.name.length > 200 ||
    typeof row.question !== "string" || !row.question.trim() || new TextEncoder().encode(row.question).length > 4096 ||
    Object.keys(app).length !== 2 || Object.keys(recipient).length !== 1 ||
    Object.keys(row).length !== (row.state === "answered" ? 8 : 7)
  ) throw new Error("Invalid private app handoff decision.");
  const base = { id: row.action_id, app: app.title, recipient: recipient.name, question: row.question };
  if (row.state === "answered" && typeof row.answer === "string" && row.answer.length > 0)
    return { ...base, state: "answered", answer: row.answer };
  const state = states.find((item) => item === row.state);
  if (state && !Object.hasOwn(row, "answer")) return { ...base, state };
  throw new Error("Invalid private app handoff result.");
}

async function request(
  conversation: string,
  id?: string,
  mutation?: AppHandoffMutation,
  signal?: AbortSignal,
  review?: "read" | "send",
): Promise<Record<string, unknown> | null> {
  if (id !== undefined && !ACTION_ID.test(id)) throw new Error("Invalid private app handoff ID.");
  const suffix = review ? "/review" : mutation ? `/${mutation}` : "";
  const path = `/api/assist/app-handoffs${id ? `/${id}` : ""}${suffix}`;
  return usingPrivateOperation(undefined, async (op) => {
    const response = await op.fetch(`${path}?${new URLSearchParams({ conversation })}`, {
      method: mutation || review === "send" ? "POST" : "GET",
      headers: {
        ...sessionHeaders(), ...APP_HANDOFF_HEADERS, "X-Cadence-Board": "1",
        ...((mutation === "run" || review) ? REVIEW_HEADER : {}),
      },
    });
    if ((id === undefined || review === "read") && (response.status === 404 || response.status === 412)) {
      op.assertCurrent();
      return null;
    }
    const body: unknown = await response.json().catch(() => null);
    op.assertCurrent();
    if (!response.ok) throw new ApiError(errorMessage(body, response.statusText), response.status);
    const result = object(body);
    if (!result) throw new Error("Invalid private app handoff response.");
    return result;
  }, signal);
}

export async function listAppHandoffs(conversation: string, signal: AbortSignal): Promise<AppHandoffDecision[]> {
  const body = await request(conversation, undefined, undefined, signal);
  if (!body) return [];
  if (!Array.isArray(body.decisions) || body.decisions.length > 50)
    throw new Error("Invalid private app handoff list.");
  const seen = new Set<string>();
  return body.decisions.map((value) => {
    const row = parseDecision(value);
    if (seen.has(row.id)) throw new Error("Duplicate private app handoff decision.");
    seen.add(row.id);
    return row;
  });
}

export async function appHandoffDecision(
  conversation: string,
  id: string,
  mutation?: AppHandoffMutation,
  signal?: AbortSignal,
): Promise<AppHandoffDecision> {
  const body = await request(conversation, id, mutation, signal);
  const row = parseDecision(body?.decision);
  if (row.id !== id) throw new Error("The response belongs to a different handoff.");
  return row;
}

export type SupplierBillDraftReview = {
  supplier: string;
  sourceExcerpt: string;
  contactId: string;
  reference: string;
  issueDate: string;
  dueDate: string;
  currency: string;
  lines: { description: string; amountMinor: number }[];
  totalMinor: number;
  minorDigits: 0 | 2 | 3;
  actionId: string | null;
};

export async function appHandoffReview(
  conversation: string,
  id: string,
  send: boolean,
  signal: AbortSignal,
): Promise<SupplierBillDraftReview | null> {
  const body = await request(conversation, id, undefined, signal, send ? "send" : "read");
  if (!body) return null;
  const review = object(body.review);
  const bill = object(review?.proposal);
  if (review?.kind !== "supplier_bill_draft" || review.handoff_id !== id || review.source !== "unverified" ||
    (review.minor_digits !== 0 && review.minor_digits !== 2 && review.minor_digits !== 3) ||
    !bill || typeof bill.supplier !== "string" || !bill.supplier || bill.supplier.length > 120 ||
    typeof bill.source_excerpt !== "string" || !bill.source_excerpt.trim() || bill.source_excerpt.length > 2048 ||
    typeof bill.contact_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(bill.contact_id) ||
    typeof bill.supplier_ref !== "string" || !/^[A-Z0-9 #/._-]{1,40}$/.test(bill.supplier_ref) ||
    typeof bill.issue_date !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(bill.issue_date) ||
    typeof bill.due_date !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(bill.due_date) || bill.due_date < bill.issue_date ||
    typeof bill.currency !== "string" || !/^[A-Z]{3}$/.test(bill.currency) ||
    !Array.isArray(bill.lines) || bill.lines.length === 0 || bill.lines.length > 25 ||
    typeof bill.total_minor !== "number" || !Number.isSafeInteger(bill.total_minor) || bill.total_minor <= 0 ||
    (review.action_id !== null && (typeof review.action_id !== "string" || !ACTION_ID.test(review.action_id))) ||
    (send && review.action_id === null)
  ) throw new Error("Invalid private draft review receipt.");
  const lines = (bill.lines as unknown[]).map((line) => {
    const item = object(line);
    if (typeof item?.description !== "string" || !item.description || item.description.length > 200 ||
      typeof item.amount_minor !== "number" || !Number.isSafeInteger(item.amount_minor) || item.amount_minor < 0)
      throw new Error("Invalid private draft review line.");
    return { description: item.description, amountMinor: item.amount_minor };
  });
  if (lines.reduce((sum, line) => sum + line.amountMinor, 0) !== bill.total_minor)
    throw new Error("Invalid private draft review total.");
  return {
    supplier: bill.supplier, contactId: bill.contact_id, reference: bill.supplier_ref,
    sourceExcerpt: bill.source_excerpt,
    issueDate: bill.issue_date, dueDate: bill.due_date, currency: bill.currency,
    lines, totalMinor: bill.total_minor, minorDigits: review.minor_digits, actionId: review.action_id,
  };
}

/** This opt-in is HTTP-only; the shared socket path stays unchanged. */
export async function sendHandoffCompatibleMaster(
  input: {
    conversation: string;
    message: string;
    text: string;
    refs?: ThreadRef[];
    attachments?: { id: string }[];
    mediaRefs?: { asset_id: string; revision: number; sha256: string }[];
  },
  operation: PrivateOperation,
): Promise<void> {
  const { conversation, message, text, refs, attachments, mediaRefs } = input;
  const response = await operation.fetch("/api/threads/master/messages", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Cadence-Board": "1",
      ...sessionHeaders(),
      ...APP_HANDOFF_HEADERS,
    },
    body: JSON.stringify({
      conversation,
      message,
      text,
      ...(refs?.length ? { refs } : {}),
      ...(attachments?.length ? { attachments: attachments.map(({ id }) => ({ id })) } : {}),
      ...(mediaRefs?.length ? { media_refs: mediaRefs } : {}),
    }),
  });
  const body: unknown = await response.json().catch(() => null);
  operation.assertCurrent();
  if (!response.ok) throw new ApiError(errorMessage(body, response.statusText), response.status);
}
