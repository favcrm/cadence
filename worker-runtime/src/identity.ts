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
  if (!hasTable(sql, "pi_submissions")) return false;
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
  if (!hasTable(sql, "pi_submissions")) return null;
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

/** One submission the engine has settled, as its own record says. */
export interface SettledSubmission {
  /** The engine's own submission id. */
  readonly id: number;
  /** The conversation whose records hold it. */
  readonly conversation: number;
  /** The operation id it was admitted under. */
  readonly operationId: string;
  /**
   * The terminal lifecycle status the engine stored. `done` ran to an answer;
   * `unanswered` settled without one. Both are terminal for the engine's
   * admission path, which returns this record for a repeated operation id
   * instead of creating a submission or starting a run.
   */
  readonly status: "done" | "unanswered";
}

/**
 * What the session's durable records prove about one conversation's operation
 * id.
 *
 * - `settled`: exactly one terminal submission holds the id, and the session is
 *   at rest — no queued or placed submission and no task short of terminal in
 *   any conversation of this object. The engine's admission path can only
 *   return that submission for it, and nothing else here is in flight.
 * - `absent`: no durable submission holds the id, and the session is at rest.
 *   No evidence of the operation exists in this session. This is not a fence:
 *   an admission another instance is still making is not visible here, so a
 *   caller must separately fence or await its own dispatch.
 * - `unknown`: the records prove neither. The id is held by work the engine
 *   still has, the id is absent while the session holds other work, or the
 *   record or schema is ambiguous, unreadable or inconsistent.
 */
export type SubmissionSettlement =
  | { readonly state: "settled"; readonly settled: SettledSubmission }
  | { readonly state: "absent" }
  | { readonly state: "unknown" };

/**
 * What the engine's own durable records prove about one operation id.
 *
 * The pair — conversation and request id — is the engine's deduplication key
 * (`pi_submissions.conversation_id` and its JSON-encoded `request_id`), and its
 * admission path reads exactly that pair before creating anything: a repeated
 * id returns the stored submission, whatever its lifecycle status, and
 * `done`/`unanswered` are the states it never rewrites. One terminal record
 * therefore proves the id can never be submitted as new work.
 *
 * Both `settled` and `absent` also require the session to be at rest: the whole
 * object holds no queued or placed submission and no task short of `terminal`,
 * in its root or any nested conversation — the same two tables the engine's
 * optionless pending view scans. A terminal record alone proves the id cannot be
 * resubmitted, but a caller that must reconcile a finished dispatch also has to
 * know nothing else in this session is still in flight, so any outstanding work
 * answers `unknown` rather than `settled`.
 *
 * `absent` means only that no durable evidence exists here. A submission an
 * instance is still admitting is not visible in these tables, so absence is not
 * a fence and the caller must fence or await that dispatch itself.
 *
 * Ambiguity is not resolved: two rows for one key, an unreadable record,
 * another submission type, an unknown status, a body that disagrees with the
 * row it was read by, a schema with only one of the engine's two tables, or a
 * status outside the engine's terminal set anywhere in the session all answer
 * `unknown`, never `settled` and never `absent`.
 *
 * This reads durable rows only: it opens no engine session and wakes no task,
 * so asking the question cannot resume, restart or replay the conversation.
 * A failed read throws; callers must not read one as "no record".
 */
export function readSubmissionSettlement(
  sql: TaskSql,
  conversationId: number,
  operationId: string,
): SubmissionSettlement {
  if (!Number.isSafeInteger(conversationId) || conversationId <= 0)
    throw new Error("The settlement's conversation is malformed.");
  if (typeof operationId !== "string" || operationId.length === 0)
    throw new Error("The settlement's operation id is malformed.");
  // Without the submissions table nothing can hold the id, but the session
  // still has to be readable as at rest; a partial schema or any work in
  // flight answers unknown below.
  if (!hasTable(sql, "pi_submissions"))
    return outstandingWork(sql) === "none"
      ? { state: "absent" }
      : { state: "unknown" };
  const rows = sql
    .exec(
      `SELECT id, conversation_id, status, record FROM pi_submissions
        WHERE conversation_id=? AND request_id=? ORDER BY id LIMIT 2`,
      conversationId,
      JSON.stringify(operationId),
    )
    .toArray();
  if (rows.length > 1) return { state: "unknown" };
  const row = rows[0];
  if (row === undefined)
    return outstandingWork(sql) === "none"
      ? { state: "absent" }
      : { state: "unknown" };
  const id = Number(row.id);
  if (!Number.isSafeInteger(id) || id <= 0) return { state: "unknown" };
  const submission = parseRecord(row.record);
  if (submission === null) return { state: "unknown" };
  // The stored body must agree with the row it was read by and with the
  // question, or this row proves nothing about this operation. The engine
  // writes the key and the lifecycle status to both the columns its pending
  // scans read and the record its admission path returns; a disagreement means
  // one of the two views is stale or corrupt.
  if (
    submission.requestId !== operationId ||
    submission.conversationId !== conversationId ||
    submission.type !== "input" ||
    Number(row.conversation_id) !== conversationId ||
    row.status !== submission.status
  )
    return { state: "unknown" };
  const status = submission.status;
  if (status !== "done" && status !== "unanswered") return { state: "unknown" };
  // The id cannot be resubmitted, but the gate also requires nothing else in
  // this session to be in flight.
  if (outstandingWork(sql) !== "none")
    return { state: "unknown" };
  return {
    state: "settled",
    settled: { id, conversation: conversationId, operationId, status },
  };
}

/** Whether the session is at rest, still working, or unreadable. */
type OutstandingWork = "none" | "some" | "unknown";

/**
 * Whether the whole session object is at rest, still working, or unreadable.
 *
 * The Durable Object is one session: its root conversation and any nested or
 * forked conversation it creates share these tables. The scan therefore covers
 * every conversation, exactly as the engine's own optionless pending view does,
 * so work in a child conversation cannot be missed. Any submission status other
 * than the engine's two terminal states counts as outstanding — including NULL
 * or a value this engine does not write — so a legacy or corrupt schema blocks
 * instead of reading as at rest. The engine's migrations create both tables
 * together, so one present without the other is likewise reported as unreadable.
 */
function outstandingWork(sql: TaskSql): OutstandingWork {
  const submissions = hasTable(sql, "pi_submissions");
  if (submissions !== hasTable(sql, "pi_tasks")) return "unknown";
  if (!submissions) return "none";
  // Anything that is not one of the engine's terminal states counts as
  // outstanding, including NULL or a value this engine does not write: a
  // legacy or corrupt schema must block, never read as at rest. `NOT IN` and
  // `<>` are NULL-unsafe, so the NULL test is explicit.
  if (
    sql
      .exec(
        `SELECT 1 AS held FROM pi_submissions
          WHERE status IS NULL OR status NOT IN ('done','unanswered') LIMIT 1`,
      )
      .toArray().length > 0
  )
    return "some";
  return sql
    .exec(
      `SELECT 1 AS held FROM pi_tasks
        WHERE status IS NULL OR status<>'terminal' LIMIT 1`,
    )
    .toArray().length > 0
    ? "some"
    : "none";
}

/** Whether the engine's schema has this table; a migrated engine has both. */
function hasTable(sql: TaskSql, name: "pi_submissions" | "pi_tasks"): boolean {
  const table = sql
    .exec(
      "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
      name,
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

/**
 * Whether a blocking compaction the run itself waited on ended in a terminal
 * failure.
 *
 * Only a compaction the run waited on counts: the engine creates a blocking
 * threshold compaction as a child of the generation that needs it, while a
 * background one is conversation-owned and may fail while its concurrent
 * generation still succeeds. The owning generation must also have settled —
 * the child commits its outcome before the generation reads it and ends the
 * run — so a read between those commits reports nothing yet.
 *
 * The message is the caller's own sentence: a failed compaction's stored error
 * may carry provider prose, and this adapter never hands that out.
 */
export function readCompactionFailure(sql: TaskSql, conversationId: number): boolean {
  if (!Number.isSafeInteger(conversationId) || conversationId <= 0) return false;
  try {
    const rows = sql
      .exec(
        `SELECT id, record FROM pi_tasks
          WHERE conversation_id = ? AND kind = '"pi.compaction"' AND status = 'terminal'
          ORDER BY id DESC LIMIT 1`,
        conversationId,
      )
      .toArray();
    const row = rows[0];
    if (row === undefined || !Number.isSafeInteger(row.id)) return false;
    const id = Number(row.id);
    // A record written after the failure — a placed summary, an answer, or the
    // next input — means the failure belongs to history, not to the run on
    // screen.
    const latest = sql
      .exec(`SELECT MAX(id) AS latest FROM pi_entries WHERE conversation_id = ?`, conversationId)
      .toArray()[0]?.latest;
    if (typeof latest === "number" && id <= latest) return false;
    const record = parseRecord(row.record);
    const owner = record?.owner;
    if (!Number.isSafeInteger(owner)) return false;
    const state = record?.state;
    if (typeof state !== "object" || state === null || Array.isArray(state)) return false;
    const terminal = state as Record<string, unknown>;
    if (terminal.status !== "terminal") return false;
    const outcome = terminal.outcome;
    if (typeof outcome !== "object" || outcome === null || Array.isArray(outcome)) return false;
    const status = (outcome as Record<string, unknown>).status;
    if (status !== "failed" && status !== "faulted" && status !== "orphaned") return false;
    const parent = sql
      .exec(`SELECT status FROM pi_tasks WHERE id = ?`, owner as number)
      .toArray()[0];
    return parent === undefined || parent.status === "terminal";
  } catch {
    return false;
  }
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
