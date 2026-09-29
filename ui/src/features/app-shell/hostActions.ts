import { ApiError } from "../../lib/api";
import { sessionHeaders } from "../../lib/sessionKey";

/**
 * Typed host action client for installed-App record screens (CAD-802).
 *
 * The installation, context and record IDs always come from the trusted
 * route/scope — never from a form field, a chat message or a stored
 * draft. Request bodies carry only the operator-checked grammar the
 * HTTP peer accepts (`record_id` + `profile` on create,
 * `expected_revision` + `profile` on update); anything else throws
 * client-side before a byte is sent, and the server's
 * `deny_unknown_fields` refuses it again. No actor claims, SQL, App
 * database paths, provider secrets or project links ever enter this
 * client. Authority stays server-checked (CAD-768); the selected record
 * is context for a turn, never proof of access.
 */

/** One record row as the daemon returns it: identity plus profile. */
export interface HostRecord {
  id: string;
  install_id: string;
  context_id: string;
  kind: string;
  revision: number;
  digest: string;
  profile: unknown;
  history?: unknown;
}

/** URL-bound scope: the only identity this client will use. */
export interface HostScope {
  installId: string;
  contextId: string;
}

/** Body keys the HTTP peer accepts. Everything else is forged. */
const CREATE_KEYS = ["record_id", "profile"] as const;
const UPDATE_KEYS = ["expected_revision", "profile"] as const;

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
] as const;

/** URL-identity fields: only the operation's own grammar may carry them
 *  (`record_id` on create); anywhere else they are forged. */
const URL_IDENTITY_KEYS = ["install_id", "context_id", "record_id"] as const;

/** Every field this client refuses outside the operation's grammar. */
export const FORBIDDEN_BODY_KEYS = [...ALWAYS_FORBIDDEN, ...URL_IDENTITY_KEYS] as const;

function segment(id: string): boolean {
  return id.length >= 1 && id.length <= 128 && /^[A-Za-z0-9_-]+$/.test(id);
}

/** The same segment grammar the HTTP peer enforces — refuse early. */
export function assertScope(scope: HostScope): void {
  if (!segment(scope.installId) || !segment(scope.contextId)) {
    throw new ApiError("invalid installation or context scope", 400);
  }
}

export function assertRecordId(recordId: string): void {
  if (!segment(recordId)) throw new ApiError("invalid record id", 400);
}

/** Defense in depth: unknown or authority-claiming keys never serialize. */
export function assertCleanBody(body: Record<string, unknown>, allowed: readonly string[]): void {
  for (const key of Object.keys(body)) {
    if (
      !allowed.includes(key) ||
      (ALWAYS_FORBIDDEN as readonly string[]).includes(key)
    ) {
      throw new ApiError(`refused app record field: ${key}`, 400);
    }
  }
}

const part = encodeURIComponent;
export function listPath(scope: HostScope): string {
  assertScope(scope);
  return `/api/app-installations/${part(scope.installId)}/contexts/${part(scope.contextId)}/records`;
}

export function recordPath(scope: HostScope, recordId: string): string {
  assertScope(scope);
  assertRecordId(recordId);
  return `${listPath(scope)}/${part(recordId)}`;
}

export function updatePath(scope: HostScope, recordId: string): string {
  return `${recordPath(scope, recordId)}/update`;
}

async function request<T>(path: string, body?: Record<string, unknown>): Promise<T> {
  const response = await fetch(path, {
    method: body === undefined ? "GET" : "POST",
    credentials: "same-origin",
    cache: "no-store",
    headers:
      body === undefined
        ? sessionHeaders()
        : { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  const value = await response.json().catch(() => null);
  if (!response.ok) throw new ApiError(value?.error ?? `${response.status} ${response.statusText}`, response.status);
  if (value === null) throw new ApiError("The server returned an invalid app receipt", 502);
  return value as T;
}

function asRecord(value: unknown): HostRecord {
  const record = (value as { record?: unknown } | null)?.record;
  if (!record || typeof record !== "object") throw new ApiError("The server returned an invalid app receipt", 502);
  const row = record as Record<string, unknown>;
  if (typeof row.id !== "string" || typeof row.revision !== "number" || typeof row.digest !== "string") {
    throw new ApiError("The server returned an invalid app receipt", 502);
  }
  return {
    id: row.id,
    install_id: typeof row.install_id === "string" ? row.install_id : "",
    context_id: typeof row.context_id === "string" ? row.context_id : "",
    kind: typeof row.kind === "string" ? row.kind : "",
    revision: row.revision,
    digest: row.digest,
    profile: row.profile,
    history: row.history,
  };
}

export const hostActions = {
  paths: { listPath, recordPath, updatePath },
  guards: { assertScope, assertRecordId, assertCleanBody },
  /** `GET …/records` — the server scopes rows to the URL's install/context. */
  async list(scope: HostScope): Promise<HostRecord[]> {
    const value = await request<{ records: unknown }>(listPath(scope));
    if (!Array.isArray(value.records)) throw new ApiError("The server returned an invalid app receipt", 502);
    return value.records.map((row) => asRecord({ record: row }));
  },
  /** `GET …/records/:id` — one row under the same URL scope. */
  async show(scope: HostScope, recordId: string): Promise<HostRecord> {
    return asRecord(await request<unknown>(recordPath(scope, recordId)));
  },
  /** `POST …/records` — body is exactly `{record_id, profile}`. */
  async create(scope: HostScope, recordId: string, profile: unknown): Promise<HostRecord> {
    assertRecordId(recordId);
    const body: Record<string, unknown> = { record_id: recordId, profile };
    assertCleanBody(body, CREATE_KEYS);
    return asRecord(await request<unknown>(listPath(scope), body));
  },
  /** `POST …/records/:id/update` — body is exactly `{expected_revision, profile}`. */
  async update(scope: HostScope, recordId: string, expectedRevision: number, profile: unknown): Promise<HostRecord> {
    if (!Number.isInteger(expectedRevision) || expectedRevision <= 0) {
      throw new ApiError("expected record revision must be a positive integer", 400);
    }
    const body: Record<string, unknown> = { expected_revision: expectedRevision, profile };
    assertCleanBody(body, UPDATE_KEYS);
    return asRecord(await request<unknown>(updatePath(scope, recordId), body));
  },
};
