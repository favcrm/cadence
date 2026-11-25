import { ApiError, errorMessage } from "../../lib/apiError";
import { sessionHeaders } from "../../lib/sessionKey";
import {
  privateFetch,
  privateReadsAllowed,
  usingPrivateOperation,
  type PrivateOperation,
} from "../../lib/privateSession";
import type {
  AgentEndpoint,
  EndpointCreateResult,
  EndpointDeliveriesPayload,
  EndpointDetailPayload,
  EndpointRotateResult,
  EndpointsPayload,
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

export const endpointsApi = {
  list: (agentUid?: string) =>
    read<EndpointsPayload>(
      agentUid ? `/api/endpoints?agent=${encodeURIComponent(agentUid)}` : "/api/endpoints",
    ),
  show: (id: string) => read<EndpointDetailPayload>(`/api/endpoints/${encodeURIComponent(id)}`),
  deliveries: (id: string, limit = 50) =>
    read<EndpointDeliveriesPayload>(
      `/api/endpoints/${encodeURIComponent(id)}/deliveries?limit=${encodeURIComponent(String(limit))}`,
    ),
  create: (body: Record<string, unknown>) => write<EndpointCreateResult>("/api/endpoints", body),
  update: (id: string, body: Record<string, unknown>) =>
    write<{ ok: boolean; endpoint: AgentEndpoint }>(
      `/api/endpoints/${encodeURIComponent(id)}/update`,
      body,
    ),
  rotateSecret: (id: string) =>
    write<EndpointRotateResult>(`/api/endpoints/${encodeURIComponent(id)}/rotate-secret`, {}),
  verify: (id: string) =>
    write<{ ok: boolean; endpoint: AgentEndpoint }>(
      `/api/endpoints/${encodeURIComponent(id)}/verify`,
      {},
    ),
  enable: (id: string) =>
    write<{ ok: boolean; endpoint: AgentEndpoint }>(
      `/api/endpoints/${encodeURIComponent(id)}/enable`,
      {},
    ),
  pause: (id: string) =>
    write<{ ok: boolean; endpoint: AgentEndpoint }>(
      `/api/endpoints/${encodeURIComponent(id)}/pause`,
      {},
    ),
  disable: (id: string) =>
    write<{ ok: boolean; endpoint: AgentEndpoint }>(
      `/api/endpoints/${encodeURIComponent(id)}/disable`,
      {},
    ),
  remove: (id: string) =>
    write<{ ok: boolean; removed: string }>(
      `/api/endpoints/${encodeURIComponent(id)}/delete`,
      {},
    ),
};
