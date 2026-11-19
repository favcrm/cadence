import { ApiError, errorMessage } from "../../lib/apiError";
import { sessionHeaders } from "../../lib/sessionKey";
import {
  privateFetch,
  privateReadsAllowed,
  usingPrivateOperation,
  type PrivateOperation,
} from "../../lib/privateSession";
import type { AgentInboxFeed, AgentInboxPromoteResult } from "../../lib/types";

async function answer<T>(resp: Response): Promise<T> {
  const body = await resp.json().catch(() => null);
  if (!resp.ok) {
    const record =
      body !== null && typeof body === "object" && !Array.isArray(body)
        ? (body as Record<string, unknown>)
        : undefined;
    throw new ApiError(errorMessage(body, `${resp.status} ${resp.statusText}`), resp.status, record);
  }
  return body as T;
}

async function read<T>(url: string, operation?: PrivateOperation): Promise<T> {
  if (!operation && !privateReadsAllowed()) throw new Error("Session verification required");
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(url, { headers: sessionHeaders() }, op);
    const value = await answer<T>(resp);
    op.assertCurrent();
    return value;
  });
}

async function write<T>(url: string, body: object, operation?: PrivateOperation): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(
      url,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-Cadence-Board": "1",
          ...sessionHeaders(),
        },
        body: JSON.stringify(body),
      },
      op,
    );
    const value = await answer<T>(resp);
    op.assertCurrent();
    return value;
  });
}

/** CAD-2476: Agent Inbox & assistant-to-assistant relay HTTP client. */
export const inboxApi = {
  feed: (opts?: { alias?: string; unreadOnly?: boolean; after?: number; limit?: number }) => {
    const qs = new URLSearchParams();
    if (opts?.alias) qs.set("alias", opts.alias);
    if (opts?.unreadOnly) qs.set("unread_only", "true");
    if (opts?.after !== undefined) qs.set("after", String(opts.after));
    if (opts?.limit !== undefined) qs.set("limit", String(opts.limit));
    const q = qs.toString();
    return read<AgentInboxFeed>(q ? `/api/inbox?${q}` : "/api/inbox");
  },
  send: (body: {
    to: string;
    text: string;
    kind?: "checkin" | "handoff" | "decision" | "reply";
    reply_to?: string;
    artifact?: string;
  }) => write<Record<string, unknown>>("/api/inbox/send", body),
  ack: (body: { alias?: string; through: number; reader?: string }) =>
    write<Record<string, unknown>>("/api/inbox/ack", body),
  promote: (body: { alias?: string; message: string; project?: string; title?: string }) =>
    write<AgentInboxPromoteResult>("/api/inbox/promote", body),
  decide: (body: {
    alias?: string;
    message: string;
    decision: "approved" | "declined";
    reason?: string;
  }) => write<Record<string, unknown>>("/api/inbox/decide", body),
};
