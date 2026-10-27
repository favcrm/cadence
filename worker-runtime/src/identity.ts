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
 * A future engine release that changes the table or checkpoint shape only
 * needs this module updated; session and funding code consume the stable
 * shape.
 */
export interface TaskSql {
  exec(query: string): { toArray(): Record<string, unknown>[] };
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
