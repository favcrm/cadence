import { ApiError, errorMessage } from "../../lib/apiError";
import { sessionHeaders } from "../../lib/sessionKey";
import {
  privateFetch,
  privateReadsAllowed,
  usingPrivateOperation,
  type PrivateOperation,
} from "../../lib/privateSession";
import type {
  ConnectionPolicy,
  Routine,
  RoutinePayload,
  RoutineRun,
  RoutinesPayload,
  StagedRoutineProposal,
} from "../../lib/types";

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

export const routinesApi = {
  list: (agent?: string) =>
    read<RoutinesPayload>(
      agent ? `/api/routines?agent=${encodeURIComponent(agent)}` : "/api/routines",
    ),
  show: (id: string) => read<RoutinePayload>(`/api/routines/${encodeURIComponent(id)}`),
  create: (body: Record<string, unknown>) =>
    write<{ ok: boolean; routine: Routine }>("/api/routines", body),
  update: (id: string, body: Record<string, unknown>) =>
    write<{ ok: boolean; routine: Routine }>(
      `/api/routines/${encodeURIComponent(id)}/update`,
      body,
    ),
  approve: (id: string, body: { expect_rev?: string | null; digest?: string | null }) =>
    write<{ ok: boolean; routine: Routine }>(
      `/api/routines/${encodeURIComponent(id)}/approve`,
      body,
    ),
  run: (id: string, body?: { trigger_source?: string; due_instant?: number }) =>
    write<{ ok: boolean; run: RoutineRun }>(
      `/api/routines/${encodeURIComponent(id)}/run`,
      body ?? {},
    ),
  runDecide: (runId: string, decision: "approve" | "dismiss") =>
    write<{ ok: boolean; run: RoutineRun }>(
      `/api/routine-runs/${encodeURIComponent(runId)}/decide`,
      { decision },
    ),
  proposalDecide: (proposalId: string, decision: "approve" | "dismiss") =>
    write<{ ok: boolean; proposal: StagedRoutineProposal; routine?: Routine }>(
      `/api/routine-proposals/${encodeURIComponent(proposalId)}/decide`,
      { decision },
    ),
  pause: (id: string, reason?: string) =>
    write<{ ok: boolean; routine: Routine }>(
      `/api/routines/${encodeURIComponent(id)}/pause`,
      reason ? { reason } : {},
    ),
  resume: (id: string) =>
    write<{ ok: boolean; routine: Routine }>(
      `/api/routines/${encodeURIComponent(id)}/resume`,
      {},
    ),
  remove: (id: string) =>
    write<{ ok: boolean; removed: string }>(
      `/api/routines/${encodeURIComponent(id)}/remove`,
      {},
    ),
  connectionPolicy: (id: string) =>
    read<{ policy: ConnectionPolicy }>(`/api/connections/${encodeURIComponent(id)}/policy`),
  connectionPolicySet: (id: string, body: Record<string, unknown>) =>
    write<{ ok: boolean; policy: ConnectionPolicy; shared_credential_cleared: boolean }>(
      `/api/connections/${encodeURIComponent(id)}/policy`,
      body,
    ),
  credentialRequestContinue: (id: string, body?: { connection_id?: string }) =>
    write<{
      ok: boolean;
      request_id: string;
      connection_id: string;
      resumed: boolean;
      message_id: string | null;
    }>(`/api/connection-requests/${encodeURIComponent(id)}/continue`, body ?? {}),
};
