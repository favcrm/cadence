import type { GenerationIdentity } from "./contract.ts";

/**
 * The one place that knows the engine's durable SQLite layout.
 *
 * The engine stores one row per task in `pi_tasks`; the harness configures the
 * `pi_` prefix. A generation task keeps a `{ phase, attempt, cutoff, ... }`
 * checkpoint, and its task id is stable across provider retries and crash
 * recovery while a tool round advances `cutoff`. That pair — task id + cutoff
 * — is the operation-scoped identity the funding ledger keys on, so this
 * adapter exposes nothing else and fails closed on a missing or ambiguous
 * running generation.
 *
 * A run's inputs (`pi.live.run.inputs`) are submission ids. `pi_submissions`
 * records the entry each placed input became, and `pi_entries` holds that
 * entry's own `kind`, `data` and user message — where the runtime's admission
 * envelope lives. `readSubmissionEntry` exposes that for one submission root,
 * and `readHeldSubmission` reads the same records from the other end: the
 * submission, and the text and entry it was submitted with, for one operation
 * id. A queued input has no entry yet; its draft lives in the engine's own
 * inbox document, which the session reads through the engine's document API.
 *
 * A future engine release that changes these tables or shapes only needs this
 * module updated; session and funding code consume the stable shapes.
 */
export interface TaskSql {
  exec(
    query: string,
    ...bindings: (string | number | null)[]
  ): { toArray(): Record<string, unknown>[] };
}

interface TaskRow {
  id: number;
  record: string;
}

const IDENTITY_PART = /^[A-Za-z0-9_-]{1,40}$/;

interface GenerationCheckpoint {
  phase?: unknown;
  cutoff?: unknown;
}

interface GenerationRecord {
  conversationId?: unknown;
  kind?: unknown;
  state?: { status?: unknown; checkpoint?: GenerationCheckpoint };
}

/** The one running request's identity, or `null` when it is absent or ambiguous. */
export function readGenerationIdentity(
  sql: TaskSql,
): GenerationIdentity | null {
  let rows: TaskRow[];
  try {
    rows = sql
      .exec(
        `SELECT id, record FROM pi_tasks
          WHERE kind = '"pi.generation"' AND status = 'running'
          ORDER BY id DESC LIMIT 2`,
      )
      .toArray()
      .map((row) => ({ id: Number(row.id), record: String(row.record) }));
  } catch {
    return null;
  }
  // Two running generation tasks in one conversation object is ambiguous;
  // choosing either could bill or replay the wrong operation.
  if (rows.length !== 1) return null;
  const row = rows[0]!;
  let record: GenerationRecord;
  try {
    record = JSON.parse(row.record) as GenerationRecord;
  } catch {
    return null;
  }
  const checkpoint = record.state?.checkpoint;
  if (
    record.kind !== "pi.generation" ||
    record.state?.status !== "running" ||
    !checkpoint ||
    checkpoint.phase !== "request"
  ) {
    return null;
  }
  const conversationId = record.conversationId;
  const cutoff = checkpoint.cutoff;
  const cutoffValue =
    typeof cutoff === "number" && Number.isSafeInteger(cutoff) && cutoff >= 0
      ? String(cutoff)
      : typeof cutoff === "string"
        ? cutoff
        : null;
  const conversation =
    typeof conversationId === "number" && Number.isSafeInteger(conversationId)
      ? String(conversationId)
      : typeof conversationId === "string"
        ? conversationId
        : null;
  const task = String(row.id);
  if (
    conversation === null ||
    cutoffValue === null ||
    !IDENTITY_PART.test(conversation) ||
    !IDENTITY_PART.test(task) ||
    !IDENTITY_PART.test(cutoffValue)
  ) {
    return null;
  }
  return { conversation, task, cutoff: cutoffValue };
}

/**
 * The summarization request of the exact compaction task attested by the
 * engine's current phase, or nothing. A task's pinned tail does not change on
 * a retry or after recovery, and its task id is distinct from the generation
 * waiting for it.
 */
export function readCompactionIdentity(sql: TaskSql, taskId: number): GenerationIdentity | null {
  if (!Number.isSafeInteger(taskId) || taskId <= 0) return null;
  try {
    const rows = sql
      .exec(
        `SELECT id, conversation_id, record FROM pi_tasks
          WHERE id = ? AND kind = '"pi.compaction"' AND status = 'running'`,
        taskId,
      )
      .toArray();
    const row = rows[0];
    if (rows.length !== 1 || row?.id !== taskId) return null;
    const record = parseRecord(row.record);
    const state = record?.state;
    if (typeof state !== "object" || state === null || Array.isArray(state)) return null;
    const checkpoint = (state as Record<string, unknown>).checkpoint;
    if (
      record?.id !== taskId ||
      record.conversationId !== row.conversation_id ||
      record.kind !== "pi.compaction" ||
      (state as Record<string, unknown>).status !== "running" ||
      typeof checkpoint !== "object" ||
      checkpoint === null ||
      Array.isArray(checkpoint)
    )
      return null;
    const request = checkpoint as Record<string, unknown>;
    if (request.phase !== "summarize") return null;
    const part = (value: unknown): string | null =>
      typeof value === "number" && Number.isSafeInteger(value) && value > 0
        ? String(value)
        : null;
    const conversation = part(record.conversationId);
    const cutoff = part(request.tail);
    if (conversation === null || cutoff === null) return null;
    return { conversation, task: String(taskId), cutoff };
  } catch {
    return null;
  }
}

/** The engine's stored pieces of one entry: its kind, its data and its messages. */
export interface StoredEntryData {
  readonly kind: unknown;
  readonly data: unknown;
  /** The messages the entry contributes to the model context, when it has any. */
  readonly model: unknown;
}

/**
 * The entry one placed input submission became, as the engine stored it.
 *
 * Each of a run's inputs is a submission id. Its record names the placed
 * entry, and that entry is immutable, so the runtime reads the envelope it
 * actually admitted instead of re-deriving a scope from a caller. This never
 * guesses and never answers "nothing": a missing row, a failed read, a
 * non-input submission, one with no placed entry or an unusable record all
 * throw, so a caller can tell "proven Home" (a readable entry without an app)
 * from "unavailable" and refuse the latter.
 */
export function readSubmissionEntry(
  sql: TaskSql,
  submissionId: string,
): StoredEntryData {
  const unavailable = () =>
    new Error(`Submission "${submissionId}" has no readable placed entry.`);
  const id = Number(submissionId);
  if (!Number.isSafeInteger(id) || id <= 0) throw unavailable();
  const submission = record(sql, "pi_submissions", id);
  if (submission === null || submission.type !== "input") throw unavailable();
  const entryId = submission.entry;
  if (
    typeof entryId !== "number" ||
    !Number.isSafeInteger(entryId) ||
    entryId <= 0
  )
    throw unavailable();
  const entry = entryData(sql, entryId);
  if (entry === null) throw unavailable();
  return entry;
}

/** The engine's request id for a placed input, never a value inferred from its text. */
export function readSubmissionRequestId(sql: TaskSql, submissionId: string): string {
  const unavailable = () =>
    new Error(`Submission "${submissionId}" has no readable request identity.`);
  const id = Number(submissionId);
  if (!Number.isSafeInteger(id) || id <= 0) throw unavailable();
  const submission = record(sql, "pi_submissions", id);
  if (
    submission === null ||
    submission.type !== "input" ||
    typeof submission.requestId !== "string"
  )
    throw unavailable();
  return submission.requestId;
}

/**
 * Whether the engine holds any submission — queued, placed or settled — for one
 * operation id.
 *
 * The engine writes the id verbatim into the submission's own `record` (its
 * `request_id` column stores the JSON-encoded index form), so this reads the
 * authoritative record. A database without the engine's submissions table
 * provably holds no submissions: pi's own migrations create that table before
 * anything can be stored in it. Anything else — a failed read, an unreadable
 * schema — propagates, so a caller that cannot prove the id is unused refuses
 * it instead of assuming.
 */
export function submissionHeld(sql: TaskSql, operationId: string): boolean {
  if (!hasSubmissions(sql)) return false;
  const held = sql
    .exec(
      "SELECT 1 AS held FROM pi_submissions WHERE json_extract(record,'$.requestId')=? LIMIT 1",
      operationId,
    )
    .toArray()[0];
  return held !== undefined;
}

/** One submission the engine holds for an operation id, as its own records say. */
export interface HeldSubmission {
  /** The engine's own submission id. */
  readonly id: number;
  /** The submission type the engine stored. */
  readonly type: "input" | "write";
  /** The lifecycle status the engine stored. */
  readonly status: "queued" | "placed" | "done" | "unanswered";
  /**
   * The exact text the input was submitted with, when the engine stored it as
   * one plain user message: the message a submission appends for its input.
   * `null` when the engine placed no entry yet, or stored a content shape that
   * cannot be read back as that text.
   */
  readonly text: string | null;
  /** The entry the input was placed as, when the engine placed one. */
  readonly entry: StoredEntryData | null;
}

/**
 * The submission the engine holds for one operation id, or `null` when its
 * records prove it holds none.
 *
 * A runtime build that predates the admission guard submits without one, so the
 * engine can hold an operation id this session's SQLite never bound. Recovering
 * what such a submission actually was is the only way to tell an identical
 * retry from a different operation reusing the id, and the engine's own records
 * are the only authority for it: the submission's type, the entry it was placed
 * as, and the user message that entry carries.
 *
 * The lookup is the engine's own deduplication key — the conversation and the
 * JSON-encoded request id it indexes — so this answers for exactly the
 * submission the engine would replay, and not one another conversation holds
 * under the same id.
 *
 * Unlike the rest of this adapter, an unreadable or ambiguous record throws.
 * Its caller acts on "the engine holds nothing", so that answer must be a proof
 * rather than a failed read: two rows for one request id, a record that is not
 * a submission, or an entry the record names but the engine cannot produce are
 * states no correct engine writes, and a caller that cannot tell which row is
 * the operation refuses instead of guessing.
 */
export function readHeldSubmission(
  sql: TaskSql,
  conversationId: number,
  operationId: string,
): HeldSubmission | null {
  if (!hasSubmissions(sql)) return null;
  const rows = sql
    .exec(
      `SELECT id, record FROM pi_submissions
        WHERE conversation_id=? AND request_id=? ORDER BY id LIMIT 2`,
      conversationId,
      JSON.stringify(operationId),
    )
    .toArray();
  if (rows.length === 0) return null;
  if (rows.length > 1)
    throw new Error(
      `Operation "${operationId}" names more than one engine submission.`,
    );
  const row = rows[0];
  if (row === undefined) return null;
  const id = Number(row.id);
  if (!Number.isSafeInteger(id) || id <= 0)
    throw new Error(
      `Operation "${operationId}" has an unreadable engine submission.`,
    );
  const submission = parseRecord(row.record);
  if (submission === null)
    throw new Error(
      `Operation "${operationId}" has an unreadable engine submission.`,
    );
  const { type, status } = submission;
  if (type !== "input" && type !== "write")
    throw new Error(
      `Operation "${operationId}" has an unreadable engine submission.`,
    );
  if (
    status !== "queued" &&
    status !== "placed" &&
    status !== "done" &&
    status !== "unanswered"
  )
    throw new Error(
      `Operation "${operationId}" has an unreadable engine submission.`,
    );
  const entryId = submission.entry;
  if (entryId === undefined || entryId === null)
    return { id, type, status, text: null, entry: null };
  if (
    typeof entryId !== "number" ||
    !Number.isSafeInteger(entryId) ||
    entryId <= 0
  )
    throw new Error(
      `Operation "${operationId}" has an unreadable engine submission.`,
    );
  const entry = entryData(sql, entryId);
  if (entry === null)
    throw new Error(
      `Operation "${operationId}" has an engine submission whose entry is unavailable.`,
    );
  return { id, type, status, text: entryText(entry.model), entry };
}

/** Whether the engine's submissions table exists; without it, none is held. */
function hasSubmissions(sql: TaskSql): boolean {
  const table = sql
    .exec(
      "SELECT name FROM sqlite_master WHERE type='table' AND name='pi_submissions'",
    )
    .toArray()[0];
  return table !== undefined;
}

/** One stored entry's pieces, or `null` when the row is absent or unreadable. */
function entryData(sql: TaskSql, entryId: number): StoredEntryData | null {
  const entry = record(sql, "pi_entries", entryId);
  if (entry === null || !("kind" in entry)) return null;
  return {
    kind: entry.kind,
    data: "data" in entry ? entry.data : undefined,
    model: "model" in entry ? entry.model : undefined,
  };
}

/**
 * The text one input entry carries, when the engine stored exactly the user
 * message a submission appends for its input: a single message of role `user`
 * whose content is a plain string. A content list, no messages, or any other
 * message shape is not that text, so this answers `null` and its caller refuses
 * rather than reconstructing one.
 */
function entryText(model: unknown): string | null {
  if (!Array.isArray(model) || model.length !== 1) return null;
  const message: unknown = model[0];
  if (typeof message !== "object" || message === null || Array.isArray(message))
    return null;
  if (!("role" in message) || !("content" in message)) return null;
  if (message.role !== "user" || typeof message.content !== "string")
    return null;
  return message.content;
}

/** One stored `record` column's parsed JSON body, or `null` when it is not one. */
function parseRecord(stored: unknown): Record<string, unknown> | null {
  if (typeof stored !== "string") return null;
  try {
    const parsed: unknown = JSON.parse(stored);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
      return null;
    return parsed as Record<string, unknown>;
  } catch {
    return null;
  }
}

/**
 * One row's parsed `record`, or `null` when the row is absent or not a JSON
 * object. A failed SQL read throws. The row's id is already known by the caller, so only the
 * body is read.
 */
function record(
  sql: TaskSql,
  table: "pi_submissions" | "pi_entries",
  id: number,
): Record<string, unknown> | null {
  // A failed read propagates: callers must not read it as an absent row.
  const row = sql
    .exec(`SELECT record FROM ${table} WHERE id=?`, id)
    .toArray()[0];
  if (row === undefined) return null;
  return parseRecord(row.record);
}
