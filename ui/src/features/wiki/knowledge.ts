import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, usingPrivateOperation, type PrivateOperation } from "../../lib/privateSession";
import { wikiErrorFrom, WikiError, type WikiPage } from "./api";
import { normalizePage } from "./wire";

/** A shared collection root. Private user, profile and memory trees are not collections. */
export function collectionRoot(path: string): string | null {
  const parts = path.split("/").filter(Boolean);
  if (parts.length === 0 || parts.some((part) => part === "." || part === "..")) return null;
  if (parts[0] === "global" && parts.length >= 2) return `global/${parts[1]}`;
  if (parts[0] === "projects" && parts.length >= 3) return `projects/${parts[1]}/${parts[2]}`;
  return null;
}

export function isCollectionRoot(path: string): boolean {
  return collectionRoot(path) === path;
}

export interface DocumentMetadata {
  path: string;
  source_type: string | null;
  version: string | null;
  applicability: string | null;
  supersedes: string | null;
  approved_revision: string | null;
}

export interface CollectionMetadata {
  publisher: string | null;
  reviewer: string | null;
  review_project: string | null;
  documents: DocumentMetadata[];
}

export type ProcessingState = "covered" | "pending" | "empty" | "failed" | "unsupported";

export interface KnowledgeDocument {
  path: string;
  kind: string;
  rev: string | null;
  sha256?: string;
  processing: ProcessingState | null;
  metadata: DocumentMetadata;
  /** Null when the server did not say whether this revision is the approved one. */
  approval_current: boolean | null;
}

export interface CollectionPermissions {
  publish: boolean;
  review: boolean;
}

export interface CollectionState {
  path: string;
  rev: string | null;
  metadata: CollectionMetadata;
  documents: KnowledgeDocument[];
  permissions: CollectionPermissions;
}

export interface CorrectionCitation {
  path: string;
  revision: string;
  section?: string;
  page?: number;
}

export interface CorrectionActor {
  id: string;
  display: string;
}

export interface KnowledgeCorrection {
  id: string;
  project: string;
  path: string;
  status: "pending" | "accepted" | "rejected";
  question: string;
  answer: string;
  expected_answer: string;
  reason: string;
  citations: CorrectionCitation[];
  submitted_by: CorrectionActor;
  reviewed_by: CorrectionActor | null;
  review_reason: string | null;
  /** Null when the server did not say whether the cited context is still current. */
  current: boolean | null;
}

export interface CorrectionList {
  path: string;
  requests: KnowledgeCorrection[];
  permissions: { submit: boolean; review: boolean };
}

const PROCESSING: Record<string, ProcessingState> = {
  covered: "covered",
  pending: "pending",
  empty: "empty",
  failed: "failed",
  unsupported: "unsupported",
};

function record(value: unknown): Record<string, unknown> | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function nullableText(value: unknown): string | null {
  if (value === null) return null;
  return text(value);
}

function metadata(value: unknown, fallbackPath: string): DocumentMetadata {
  const row = record(value);
  return {
    path: text(row?.path) ?? fallbackPath,
    source_type: nullableText(row?.source_type),
    version: nullableText(row?.version),
    applicability: nullableText(row?.applicability),
    supersedes: nullableText(row?.supersedes),
    approved_revision: nullableText(row?.approved_revision),
  };
}

function document(value: unknown): KnowledgeDocument | null {
  const row = record(value);
  const path = text(row?.path);
  if (!row || !path) return null;
  const processing = typeof row.processing === "string" ? PROCESSING[row.processing] ?? null : null;
  const sha = text(row.sha256);
  return {
    path,
    kind: text(row.kind) ?? "file",
    rev: nullableText(row.rev),
    ...(sha ? { sha256: sha } : {}),
    processing,
    metadata: metadata(row.metadata, path),
    approval_current: typeof row.approval_current === "boolean" ? row.approval_current : null,
  };
}

function actor(value: unknown): CorrectionActor | null {
  const row = record(value);
  const id = text(row?.id);
  const display = text(row?.display);
  if (!id || !display) return null;
  return { id, display };
}

function citation(value: unknown): CorrectionCitation | null {
  const row = record(value);
  const path = text(row?.path);
  const revision = text(row?.revision);
  if (!path || !revision) return null;
  const section = text(row?.section) ?? undefined;
  const page = typeof row?.page === "number" && Number.isInteger(row.page) ? row.page : undefined;
  return { path, revision, ...(section ? { section } : {}), ...(page !== undefined ? { page } : {}) };
}

export function parseCollection(value: unknown): CollectionState {
  const row = record(value);
  const path = text(row?.path);
  const meta = record(row?.metadata);
  const permissions = record(row?.permissions);
  if (!row || !path || !meta || !permissions) throw new WikiError("Collection metadata was incomplete", 502);
  const documents = Array.isArray(row.documents) ? row.documents.map(document).filter((item): item is KnowledgeDocument => item !== null) : [];
  const listed = Array.isArray(meta.documents) ? meta.documents.map((item) => metadata(item, "")).filter((item) => item.path) : [];
  return {
    path,
    rev: nullableText(row.rev),
    metadata: {
      publisher: nullableText(meta.publisher),
      reviewer: nullableText(meta.reviewer),
      review_project: nullableText(meta.review_project),
      documents: listed,
    },
    documents,
    permissions: {
      publish: permissions.publish === true,
      review: permissions.review === true,
    },
  };
}

export function parseCorrections(value: unknown): CorrectionList {
  const row = record(value);
  const path = text(row?.path);
  const permissions = record(row?.permissions);
  if (!row || !path || !permissions || !Array.isArray(row.requests)) throw new WikiError("Correction records were incomplete", 502);
  const requests: KnowledgeCorrection[] = [];
  for (const item of row.requests) {
    const body = record(item);
    const id = text(body?.id);
    const project = text(body?.project);
    const itemPath = text(body?.path);
    const status = body?.status === "pending" || body?.status === "accepted" || body?.status === "rejected" ? body.status : null;
    const submitted = actor(body?.submitted_by);
    if (!body || !id || !project || !itemPath || !status || !submitted) continue;
    const reviewed = body.reviewed_by === null ? null : actor(body.reviewed_by);
    requests.push({
      id,
      project,
      path: itemPath,
      status,
      question: typeof body.question === "string" ? body.question : "",
      answer: typeof body.answer === "string" ? body.answer : "",
      expected_answer: typeof body.expected_answer === "string" ? body.expected_answer : "",
      reason: typeof body.reason === "string" ? body.reason : "",
      citations: Array.isArray(body.citations) ? body.citations.map(citation).filter((cite): cite is CorrectionCitation => cite !== null) : [],
      submitted_by: submitted,
      reviewed_by: reviewed,
      review_reason: body.review_reason === null ? null : text(body.review_reason),
      current: typeof body.current === "boolean" ? body.current : null,
    });
  }
  return {
    path,
    requests,
    permissions: { submit: permissions.submit === true, review: permissions.review === true },
  };
}

async function readJson<T>(url: string, parse: (value: unknown) => T, operation?: PrivateOperation): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(url, { headers: sessionHeaders() }, op);
    const body: unknown = await resp.json().catch(() => null);
    op.assertCurrent();
    if (!resp.ok) throw wikiErrorFrom(resp.status, body);
    return parse(body);
  });
}

async function sendJson<T>(method: "PUT" | "POST", url: string, payload: object, parse: (value: unknown) => T, operation?: PrivateOperation): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(url, {
      method,
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify(payload),
    }, op);
    const body: unknown = await resp.json().catch(() => null);
    op.assertCurrent();
    // A refused write arrives as a successful envelope carrying `conflict`
    // and no `error` field, so read it before the generic status check or the
    // reader only ever sees "409 response".
    const conflict = record(body)?.conflict;
    if (conflict === "if_rev") throw wikiErrorFrom(409, { ...(record(body) ?? {}), error: "This collection changed since you opened it", code: "conflict" });
    if (conflict === "stale") throw wikiErrorFrom(409, { ...(record(body) ?? {}), error: text(record(body)?.reason) ?? "The cited source changed since this report was made", code: "stale" });
    if (!resp.ok) throw wikiErrorFrom(resp.status, body);
    return parse(body);
  });
}

export function readCollection(path: string, operation?: PrivateOperation): Promise<CollectionState> {
  return readJson(`/api/wiki/collection?path=${encodeURIComponent(path)}`, parseCollection, operation);
}

export function saveCollection(path: string, ifRev: string, meta: CollectionMetadata, operation?: PrivateOperation): Promise<CollectionState> {
  return sendJson("PUT", "/api/wiki/collection", { path, if_rev: ifRev, metadata: meta }, parseCollection, operation);
}

export function readCorrections(path: string, operation?: PrivateOperation): Promise<CorrectionList> {
  return readJson(`/api/wiki/corrections?path=${encodeURIComponent(path)}`, parseCorrections, operation);
}

export interface CorrectionReportBody {
  path: string;
  question: string;
  answer: string;
  expected_answer: string;
  reason: string;
  citations: CorrectionCitation[];
}

function oneCorrection(value: unknown): KnowledgeCorrection {
  const list = parseCorrections({ path: "report", requests: [value], permissions: { submit: false, review: false } });
  const request = list.requests[0];
  if (!request) throw new WikiError("The correction record was incomplete", 502);
  return request;
}

export function submitCorrection(body: CorrectionReportBody, operation?: PrivateOperation): Promise<KnowledgeCorrection> {
  return sendJson("POST", "/api/wiki/corrections", body, oneCorrection, operation);
}

export function reviewCorrection(
  body: { path: string; id: string; decision: "accept" | "reject"; reason: string; source_checked: boolean },
  operation?: PrivateOperation,
): Promise<KnowledgeCorrection> {
  return sendJson("POST", "/api/wiki/corrections/review", body, oneCorrection, operation);
}

/** Historical file JSON. A missing or different `rev` is unavailable and is not reread as latest. */
export async function readRevision(path: string, at: string, operation?: PrivateOperation): Promise<WikiPage> {
  if (!at) throw new WikiError("This revision is unavailable", 404);
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(`/api/wiki/file?path=${encodeURIComponent(path)}&at=${encodeURIComponent(at)}`, { headers: { Accept: "application/json", ...sessionHeaders() } }, op);
    const type = resp.headers.get("content-type") ?? "";
    if (!type.includes("json")) {
      await resp.body?.cancel();
      op.assertCurrent();
      if (!resp.ok) throw new WikiError("This revision is unavailable", resp.status);
      throw new WikiError("This revision did not return page text", 502);
    }
    const body: unknown = await resp.json().catch(() => null);
    op.assertCurrent();
    if (!resp.ok) throw wikiErrorFrom(resp.status, body);
    const page = normalizePage(body as Parameters<typeof normalizePage>[0]);
    if (page.rev !== at) throw new WikiError("This revision is unavailable", 404);
    return page;
  });
}
