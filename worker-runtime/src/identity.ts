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
 * entry's own `kind` and `data` — where the runtime's admission envelope lives.
 * `readSubmissionEntry` exposes exactly that pair for one submission root.
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

/** The engine's stored `kind`/`data` for one placed input entry. */
export interface StoredEntryData {
  readonly kind: unknown;
  readonly data: unknown;
}

/**
 * The entry one placed input submission became, as the engine stored it.
 *
 * Each of a run's inputs is a submission id. Its record names the placed
 * entry, and that entry is immutable, so the runtime reads the envelope it
 * actually admitted instead of re-deriving a scope from a caller. A missing
 * row, an unplaced or non-input submission, or an unparsable record yields
 * `null`; this never guesses.
 */
export function readSubmissionEntry(
  sql: TaskSql,
  submissionId: string,
): StoredEntryData | null {
  const id = Number(submissionId);
  if (!Number.isSafeInteger(id) || id <= 0) return null;
  const submission = record(sql, "pi_submissions", id);
  if (submission === null || !("type" in submission)) return null;
  if (submission.type !== "input") return null;
  if (!("entry" in submission)) return null;
  const entryId = submission.entry;
  if (
    typeof entryId !== "number" ||
    !Number.isSafeInteger(entryId) ||
    entryId <= 0
  )
    return null;
  const entry = record(sql, "pi_entries", entryId);
  if (entry === null || !("kind" in entry)) return null;
  return {
    kind: entry.kind,
    data: "data" in entry ? entry.data : undefined,
  };
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
  const table = sql
    .exec(
      "SELECT name FROM sqlite_master WHERE type='table' AND name='pi_submissions'",
    )
    .toArray()[0];
  if (table === undefined) return false;
  const held = sql
    .exec(
      "SELECT 1 AS held FROM pi_submissions WHERE json_extract(record,'$.requestId')=? LIMIT 1",
      operationId,
    )
    .toArray()[0];
  return held !== undefined;
}

/** One stored record's parsed JSON body, or `null` when it is absent or unparsable. */
function record(
  sql: TaskSql,
  table: "pi_submissions" | "pi_entries",
  id: number,
): object | null {
  let row: Record<string, unknown> | undefined;
  try {
    row = sql.exec(`SELECT record FROM ${table} WHERE id=?`, id).toArray()[0];
  } catch {
    return null;
  }
  if (row === undefined) return null;
  const stored = row.record;
  if (typeof stored !== "string") return null;
  try {
    const parsed: unknown = JSON.parse(stored);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
      return null;
    return parsed;
  } catch {
    return null;
  }
}
