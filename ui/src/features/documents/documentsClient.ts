import { errorMessage } from "../../lib/apiError";
import { privateFetch, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import { objectField } from "../../lib/wire";

/**
 * CAD-2286: the board's client for the hosted document lifecycle routes
 * (`/api/document-types`, `/api/documents`). Feature-local on purpose, as
 * `Members.tsx` is: `privateFetch` with `sessionHeaders`, never `lib/api.ts`.
 *
 * Nothing here throws for an answer. Every call resolves to a typed result,
 * and a refusal keeps the server's `error.code`. Parsing is strict about each
 * required field and tolerant of fields it does not know, so a board that adds
 * a field keeps working and a board that drops or mistypes one is an
 * `invalid` answer, never an invented state. A list with one malformed row is
 * invalid as a whole: a dropped row would hide a document from its approver.
 *
 * Only an abort is rethrown: the caller's signal (an AbortError, or the
 * signal's own reason when it lands mid-request) or the private session
 * changing, so a superseded read never renders as a failure.
 */

/** The capability string the probe must answer for this client to speak to it. */
export const DOCUMENT_CAPABILITY = "cad2286.document-lifecycle.v1";

export const DOCUMENT_STAGES = ["draft", "in_review", "approved", "delivered", "archived"] as const;
export type DocumentStage = (typeof DOCUMENT_STAGES)[number];

export const DOCUMENT_KINDS = ["md", "html", "csv", "pdf", "image", "docx", "xlsx", "pptx"] as const;
export type DocumentKind = (typeof DOCUMENT_KINDS)[number];
export const DOCUMENT_MAX_BYTES = 10 * 1024 * 1024;

/**
 * The lifecycle's own codes; the shared gate may also answer others
 * (board_session_required, membership_changed, tenant_placement_changed…),
 * which are kept as unknown codes.
 */
export const DOCUMENT_REFUSAL_CODES = [
  "document_not_approved",
  "document_archived",
  "not_approver",
  "self_approval",
  "approver_unavailable",
  "owner_only",
  "owner_required",
  "not_creator",
  "not_in_review",
  "already_shared",
  "unlabelled",
  "payload_too_large",
  "conflict",
  "not_found",
  "origin_refused",
  "invalid_request",
  "capability_unavailable",
  "internal",
] as const;
export type DocumentRefusalCode = (typeof DOCUMENT_REFUSAL_CODES)[number];

/** What every type gets until the owner confirms a preference. */
export interface DocumentBaseline {
  stages: string[];
  requireApproval: boolean;
  /** `null` is the workspace owner role. */
  approver: string | null;
  archiveAfterDays: number;
  recipientNote: string;
}

/** One type preference. `label` is the normalized key (trimmed, lower-case). */
export interface DocumentType {
  label: string;
  requireApproval: boolean;
  /** A member's subject id, or `null` for the owner role. */
  approver: string | null;
  recipientNote: string;
  archiveAfterDays: number;
  /** Only a `confirmed` row is in force; a `pending` one waits for the owner. */
  state: "pending" | "confirmed";
  rev: number;
}

/** The fields a member may propose and the owner may set. */
export interface DocumentTypeInput {
  requireApproval: boolean;
  /** A member's subject id, or `null` for the owner role. */
  approver: string | null;
  recipientNote: string;
  archiveAfterDays: number;
}

/** The artifact a document is, as the board's file view carries it. */
export interface DocumentFile {
  id: string;
  version: number;
  kind: string;
  title: string;
  /** The artifact's own label, as written; the type key is `DocumentState.type`. */
  label: string;
  customer: string;
  bytes: number;
  digest: string;
  /** ISO timestamp of this version. */
  created: string;
  issue: string | null;
  project: string | null;
  shared: boolean;
}

/** A document's lifecycle state. */
export interface DocumentState {
  /** The type key the document was submitted under. */
  type: string;
  stage: DocumentStage;
  submittedVersion: number | null;
  approvedVersion: number | null;
  selfApproved: boolean;
  deliveredVersion: number | null;
  /** Epoch milliseconds. */
  deliveredAt: number | null;
  /** Epoch milliseconds. */
  archivedAt: number | null;
  rev: number;
}

export interface DocumentRow {
  file: DocumentFile;
  document: DocumentState;
}

export interface DocumentTypes {
  capability: string;
  baseline: DocumentBaseline;
  types: DocumentType[];
}

export interface ApproveReceipt {
  stage: DocumentStage;
  version: number;
  selfApproved: boolean;
}

export interface StageReceipt {
  stage: DocumentStage;
}

export interface CreateDocumentInput {
  title: string;
  label: string;
  kind: DocumentKind;
  contentBase64: string;
  project?: string;
  issue?: string;
  customer?: string;
  submit?: boolean;
  requestKey?: string;
}

export interface SubmitReceipt {
  stage: DocumentStage;
  type: string;
  typeConfirmed: boolean;
}

/**
 * Why a call did not succeed.
 * - `refused`: the board answered an error with a code (`code` is kept even
 *   when this client does not know it).
 * - `invalid`: the answer was not the contract's shape.
 * - `failed`: no answer (network) or an error status with no code.
 */
export type DocumentsFailure =
  | { kind: "refused"; status: number; code: DocumentRefusalCode | (string & {}); message: string }
  | { kind: "invalid"; status: number; message: string }
  | { kind: "failed"; status: number | null; message: string };

export type DocumentsResult<T> = { ok: true; value: T } | { ok: false; failure: DocumentsFailure };

/** The refusal code of a failed result, or null for a success or an uncoded failure. */
export function refusalCode(result: DocumentsResult<unknown>): string | null {
  return !result.ok && result.failure.kind === "refused" ? result.failure.code : null;
}

/**
 * The capability probe. `unavailable` means the board hides the lifecycle:
 * - `not_served`: a 404, or a 501 `not_implemented` (flag off, older board);
 * - `unsupported_capability`: a 200 naming a capability other than v1, which
 *   this client does not speak (a newer contract ships a new capability).
 * Anything else that is not a well-formed v1 answer is an `error`, never
 * `unavailable`.
 */
export type DocumentsProbe =
  | ({ kind: "available" } & DocumentTypes)
  | { kind: "unavailable"; reason: "not_served" | "unsupported_capability" }
  | { kind: "error"; failure: DocumentsFailure };

// ---- parsing ----

/** A parse failure: the answer is not the contract's shape. */
class Malformed extends Error {}

function fail(what: string): never {
  throw new Malformed(what);
}

function record(value: unknown, what: string): Record<string, unknown> {
  return objectField(value) ?? fail(`${what} is not an object`);
}

function str(row: Record<string, unknown>, key: string): string {
  const value = row[key];
  return typeof value === "string" ? value : fail(`${key} is not a string`);
}

function strOrNull(row: Record<string, unknown>, key: string): string | null {
  const value = row[key];
  return value === null || typeof value === "string" ? value : fail(`${key} is not a string or null`);
}

function bool(row: Record<string, unknown>, key: string): boolean {
  const value = row[key];
  return typeof value === "boolean" ? value : fail(`${key} is not a boolean`);
}

function int(row: Record<string, unknown>, key: string): number {
  const value = row[key];
  return typeof value === "number" && Number.isSafeInteger(value) ? value : fail(`${key} is not an integer`);
}

function intOrNull(row: Record<string, unknown>, key: string): number | null {
  return row[key] === null ? null : int(row, key);
}

function stage(row: Record<string, unknown>): DocumentStage {
  const value = row.stage;
  return typeof value === "string" && (DOCUMENT_STAGES as readonly string[]).includes(value)
    ? (value as DocumentStage)
    : fail("stage is not a known stage");
}

function array(row: Record<string, unknown>, key: string): unknown[] {
  const value = row[key];
  return Array.isArray(value) ? value : fail(`${key} is not a list`);
}

function parseBaseline(value: unknown): DocumentBaseline {
  const row = record(value, "baseline");
  return {
    stages: array(row, "stages").map((item) => (typeof item === "string" ? item : fail("a stage is not a string"))),
    requireApproval: bool(row, "requireApproval"),
    approver: strOrNull(row, "approver"),
    archiveAfterDays: int(row, "archiveAfterDays"),
    recipientNote: str(row, "recipientNote"),
  };
}

function parseType(value: unknown): DocumentType {
  const row = record(value, "type");
  const state = row.state;
  if (state !== "pending" && state !== "confirmed") fail("type state is not pending or confirmed");
  return {
    label: str(row, "label"),
    requireApproval: bool(row, "requireApproval"),
    approver: strOrNull(row, "approver"),
    recipientNote: str(row, "recipientNote"),
    archiveAfterDays: int(row, "archiveAfterDays"),
    state,
    rev: int(row, "rev"),
  };
}

/** The probe's body: v1 parsed strictly, or another capability left unread,
 *  since its shape is not this client's contract. A missing capability is malformed. */
function parseProbe(value: unknown): { supported: true; types: DocumentTypes } | { supported: false } {
  const body = record(value, "type list");
  const capability = str(body, "capability");
  if (capability !== DOCUMENT_CAPABILITY) return { supported: false };
  return {
    supported: true,
    types: { capability, baseline: parseBaseline(body.baseline), types: array(body, "types").map(parseType) },
  };
}

function parseFile(value: unknown): DocumentFile {
  const row = record(value, "file");
  return {
    id: str(row, "id"),
    version: int(row, "version"),
    kind: str(row, "kind"),
    title: str(row, "title"),
    label: str(row, "label"),
    customer: str(row, "customer"),
    bytes: int(row, "bytes"),
    digest: str(row, "digest"),
    created: str(row, "created"),
    issue: strOrNull(row, "issue"),
    project: row.project === undefined ? null : strOrNull(row, "project"),
    shared: bool(row, "shared"),
  };
}

function parseState(value: unknown): DocumentState {
  const row = record(value, "document");
  return {
    type: str(row, "type"),
    stage: stage(row),
    submittedVersion: intOrNull(row, "submittedVersion"),
    approvedVersion: intOrNull(row, "approvedVersion"),
    selfApproved: bool(row, "selfApproved"),
    deliveredVersion: intOrNull(row, "deliveredVersion"),
    deliveredAt: intOrNull(row, "deliveredAt"),
    archivedAt: intOrNull(row, "archivedAt"),
    rev: int(row, "rev"),
  };
}

function parseRow(value: unknown): DocumentRow {
  const row = record(value, "document row");
  return { file: parseFile(row.file), document: parseState(row.document) };
}

function parseRows(value: unknown): DocumentRow[] {
  return array(record(value, "document list"), "documents").map(parseRow);
}

function parseSubmit(value: unknown): SubmitReceipt {
  const row = record(value, "submit receipt");
  return {
    stage: stage(row),
    type: str(row, "type"),
    typeConfirmed: bool(row, "typeConfirmed"),
  };
}

function parseApprove(value: unknown): ApproveReceipt {
  const row = record(value, "approval");
  return { stage: stage(row), version: int(row, "version"), selfApproved: bool(row, "selfApproved") };
}

function parseStage(value: unknown): StageReceipt {
  return { stage: stage(record(value, "receipt")) };
}

function parseTypeReceipt(value: unknown): DocumentType {
  return parseType(record(value, "receipt").type);
}

// ---- transport ----

/** The refusal's code, whether the board nested it under `error` or not. */
function codeOf(body: unknown): string | undefined {
  const row = objectField(body);
  if (!row) return undefined;
  if (typeof row.code === "string") return row.code;
  const error = objectField(row.error);
  return error && typeof error.code === "string" ? error.code : undefined;
}

function isAbort(error: unknown): boolean {
  return error instanceof Error && error.name === "AbortError";
}

type Answer = { status: number; ok: boolean; body: unknown; readable: boolean };

async function send(path: string, init: RequestInit, signal?: AbortSignal): Promise<Answer> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(path, init, operation);
    let body: unknown = null;
    let readable = true;
    try {
      body = await response.json();
    } catch {
      readable = false;
    }
    operation.assertCurrent();
    return { status: response.status, ok: response.ok, body, readable };
  }, signal);
}

function failureOf(answer: Answer): DocumentsFailure {
  const code = codeOf(answer.body);
  const message = errorMessage(answer.body, `The board answered ${answer.status}.`);
  return code === undefined
    ? { kind: "failed", status: answer.status, message }
    : { kind: "refused", status: answer.status, code, message };
}

/** One request, classified: a parsed success, a refusal, an invalid answer or
 *  no answer. `status` is null when nothing answered. */
async function call<T>(
  path: string,
  init: RequestInit,
  parse: (body: unknown) => T,
  signal?: AbortSignal,
): Promise<{ status: number | null; result: DocumentsResult<T> }> {
  let answer: Answer;
  try {
    answer = await send(path, init, signal);
  } catch (error) {
    // Any rejection once the caller's signal aborted is the abort, whatever its
    // reason (`controller.abort("unmount")` rejects with that string).
    if (signal?.aborted || isAbort(error)) throw error;
    const message = error instanceof Error ? error.message : String(error);
    return { status: null, result: { ok: false, failure: { kind: "failed", status: null, message } } };
  }
  const { status } = answer;
  if (!answer.ok) return { status, result: { ok: false, failure: failureOf(answer) } };
  // An unreadable success proves nothing happened, so no caller may report one.
  if (!answer.readable)
    return { status, result: { ok: false, failure: { kind: "invalid", status, message: "The board's answer is not readable." } } };
  try {
    return { status, result: { ok: true, value: parse(answer.body) } };
  } catch (error) {
    if (!(error instanceof Malformed)) throw error;
    const message = `The board's answer is not the expected shape: ${error.message}.`;
    return { status, result: { ok: false, failure: { kind: "invalid", status, message } } };
  }
}

const read = (): RequestInit => ({ headers: sessionHeaders(), cache: "no-store" });

/** A same-origin board write. The hosted routes take an exact `application/json`
 *  body; archive and restore read none. */
function write(body?: object): RequestInit {
  return {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
    cache: "no-store",
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  };
}

const documentPath = (id: string, action: string) => `/api/documents/${encodeURIComponent(id)}/${action}`;
const typePath = (label: string) => `/api/document-types/${encodeURIComponent(label)}`;

// ---- routes ----

/** `GET /api/document-types`: whether this board has the lifecycle, and its types. */
export async function probe(signal?: AbortSignal): Promise<DocumentsProbe> {
  const { status, result } = await call("/api/document-types", read(), parseProbe, signal);
  if (result.ok)
    return result.value.supported
      ? { kind: "available", ...result.value.types }
      : { kind: "unavailable", reason: "unsupported_capability" };
  const { failure } = result;
  if (status === 404) return { kind: "unavailable", reason: "not_served" };
  if (status === 501 && failure.kind === "refused" && failure.code === "not_implemented")
    return { kind: "unavailable", reason: "not_served" };
  return { kind: "error", failure };
}

/** `GET /api/documents?archived=0|1` (and optional `project=<key>`): the documents this viewer may see. */
export async function list(
  archived: boolean,
  project?: string | null,
  signal?: AbortSignal,
): Promise<DocumentsResult<DocumentRow[]>> {
  const query = project
    ? `project=${encodeURIComponent(project)}&archived=${archived ? 1 : 0}`
    : `archived=${archived ? 1 : 0}`;
  return (await call(`/api/documents?${query}`, read(), parseRows, signal)).result;
}

/** `POST /api/documents`: create a new versioned document in `draft` or direct `in_review`. */
export async function create(input: CreateDocumentInput): Promise<DocumentsResult<DocumentRow>> {
  const body = {
    title: input.title,
    label: input.label,
    kind: input.kind,
    contentBase64: input.contentBase64,
    ...(input.project === undefined ? {} : { project: input.project }),
    ...(input.issue === undefined ? {} : { issue: input.issue }),
    ...(input.customer === undefined ? {} : { customer: input.customer }),
    ...(input.submit === undefined ? {} : { submit: input.submit }),
    ...(input.requestKey === undefined ? {} : { requestKey: input.requestKey }),
  };
  return (await call("/api/documents", write(body), parseRow)).result;
}

/** `POST /api/documents/:id/submit`: creator submits a draft document for review. */
export async function submit(id: string): Promise<DocumentsResult<SubmitReceipt>> {
  return (await call(documentPath(id, "submit"), write(), parseSubmit)).result;
}

/** `POST /api/documents/:id/approve` for the version the approver reviewed.
 *  It can reject with AbortError if the private session pauses after the server
 *  applied the change, so callers catch it and refresh. */
export async function approve(id: string, version: number): Promise<DocumentsResult<ApproveReceipt>> {
  return (await call(documentPath(id, "approve"), write({ version }), parseApprove)).result;
}

/** `POST /api/documents/:id/archive`.
 *  It can reject with AbortError if the private session pauses after the server
 *  applied the change, so callers catch it and refresh. */
export async function archive(id: string): Promise<DocumentsResult<StageReceipt>> {
  return (await call(documentPath(id, "archive"), write(), parseStage)).result;
}

/** `POST /api/documents/:id/restore`.
 *  It can reject with AbortError if the private session pauses after the server
 *  applied the change, so callers catch it and refresh. */
export async function restore(id: string): Promise<DocumentsResult<StageReceipt>> {
  return (await call(documentPath(id, "restore"), write(), parseStage)).result;
}

/** `POST /api/document-types/:label`: the owner's answer is confirmed at once,
 *  a member's is pending. The body carries exactly the four preference fields.
 *  It can reject with AbortError if the private session pauses after the server
 *  applied the change, so callers catch it and refresh. */
export async function setType(label: string, input: DocumentTypeInput): Promise<DocumentsResult<DocumentType>> {
  const body = {
    requireApproval: input.requireApproval,
    approver: input.approver,
    recipientNote: input.recipientNote,
    archiveAfterDays: input.archiveAfterDays,
  };
  return (await call(typePath(label), write(body), parseTypeReceipt)).result;
}

/** `POST /api/document-types/:label/confirm`: the owner confirms the row at `rev`.
 *  It can reject with AbortError if the private session pauses after the server
 *  applied the change, so callers catch it and refresh. */
export async function confirmType(label: string, rev: number): Promise<DocumentsResult<DocumentType>> {
  return (await call(`${typePath(label)}/confirm`, write({ rev }), parseTypeReceipt)).result;
}
