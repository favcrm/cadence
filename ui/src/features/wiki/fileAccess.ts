import { withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";

export type AccessStatus = "pending" | "granted" | "declined" | "revoked";
export type AccessRequest = { id: string; version: number; requester: string; requested_at: string; status: AccessStatus; title?: string };
export type Audience = { project: string; visibility: "private" | "workspace"; revision: string | null };
export type SharePreview = { status: "preview"; ticket: string; audience: Audience };
export class FileAccessError extends Error {
  constructor(readonly status: number) { super(`File access request failed (${status}).`); }
}

async function call(path: string, body?: unknown, signal?: AbortSignal): Promise<unknown> {
  return withPrivateOperation(async (operation) => {
    const response = await operation.fetch(path, {
      method: body === undefined ? "GET" : "POST",
      headers: { ...sessionHeaders(), ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }), cache: "no-store",
    });
    const data: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw new FileAccessError(response.status);
    return data;
  }, signal);
}
const endpoint = (id: string, version: number, action: string) => `/api/wiki/file-access/${encodeURIComponent(id)}/${version}/${action}`;
const statuses = ["pending", "granted", "declined", "revoked"];
export async function listAccessRequests(signal?: AbortSignal): Promise<AccessRequest[]> {
  const data = await call("/api/wiki/file-access/requests", undefined, signal) as { requests?: unknown } | null;
  if (!Array.isArray(data?.requests) || !data.requests.every((r: AccessRequest) => r && typeof r.id === "string" && Number.isSafeInteger(r.version) && r.version > 0 && typeof r.requester === "string" && typeof r.requested_at === "string" && statuses.includes(r.status) && (r.title === undefined || typeof r.title === "string"))) throw new Error("Invalid file access response.");
  return data.requests as AccessRequest[];
}
export async function requestAccess(id: string, version: number): Promise<AccessStatus | "available"> {
  // The request endpoint deliberately has no body.
  return withPrivateOperation(async (operation) => {
    const response = await operation.fetch(endpoint(id, version, "request"), { method: "POST", headers: sessionHeaders(), cache: "no-store" });
    const data = await response.json().catch(() => null) as { status?: string } | null;
    operation.assertCurrent();
    if (!response.ok) throw new FileAccessError(response.status);
    if (!data?.status || ![...statuses, "available"].includes(data.status)) throw new Error("Invalid file access response.");
    return data.status as AccessStatus | "available";
  });
}
export async function decideAccess(row: AccessRequest, decision: "grant" | "decline" | "revoke"): Promise<void> {
  const data = await call(endpoint(row.id, row.version, "decision"), { requester: row.requester, decision }) as { status?: unknown } | null;
  const expected = decision === "grant" ? "granted" : decision === "decline" ? "declined" : "revoked";
  if (!data || data.status !== expected) throw new Error("Invalid file access decision response.");
}
function sameJson(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (!left || !right || typeof left !== "object" || typeof right !== "object" || Array.isArray(left) !== Array.isArray(right)) return false;
  const a = left as Record<string, unknown>;
  const b = right as Record<string, unknown>;
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((key) => Object.prototype.hasOwnProperty.call(b, key) && sameJson(a[key], b[key]));
}

export async function shareFile(id: string, version: number, ticket: string, audience?: Audience): Promise<SharePreview | { status: "shared"; ticket: string; audience: Audience }> {
  const data = await call(endpoint(id, version, "share"), { ticket, ...(audience ? { audience } : {}) }) as SharePreview | { status: "shared"; ticket: string; audience: Audience } | null;
  if (!data || data.status !== (audience ? "shared" : "preview") || data.ticket !== ticket || !data.audience || typeof data.audience.project !== "string" || !["private", "workspace"].includes(data.audience.visibility) || !(data.audience.revision === null || typeof data.audience.revision === "string") || (audience && !sameJson(data.audience, audience))) throw new Error("Invalid share response.");
  return data;
}
