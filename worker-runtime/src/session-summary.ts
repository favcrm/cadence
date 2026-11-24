import { ROOT_CONVERSATION_ID } from "@earendil-works/pi-durable";
import { BOARD_MESSAGE_KIND, type HostedRunFailureCode, type HostedSessionInspection } from "./contract.ts";
import type { TaskSql } from "./identity.ts";
import { RESET_KIND } from "./transcript.ts";

const MAX_ID = Number.MAX_SAFE_INTEGER;
const TIMESTAMP = "json_extract(record, '$.model[0].timestamp')";

function recordedTime(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 ? value : null;
}

function runFailureCode(reason: unknown): HostedRunFailureCode | null {
  switch (reason) {
    case "model_error":
    case "no_model":
    case "faulted":
    case "orphaned":
    case "interrupted":
    case "restart_limit":
    case "turn_limit":
      return reason;
    default:
      return null;
  }
}

/** The pinned Pi schema indexes context heads separately from ordinary entries. */
function resetBetween(sql: TaskSql, min: number, max: number, order: "ASC" | "DESC") {
  return sql.exec(
    `SELECT id, ${TIMESTAMP} AS timestamp FROM pi_entries
      WHERE conversation_id = ? AND id BETWEEN ? AND ? AND head IS NOT NULL AND head = id
        AND json_extract(record, '$.kind') = ?
      ORDER BY id ${order} LIMIT 1`,
    ROOT_CONVERSATION_ID, min, max, RESET_KIND,
  ).toArray()[0];
}

function activityBetween(sql: TaskSql, min: number, max: number, order: "ASC" | "DESC") {
  const row = sql.exec(
    `SELECT ${TIMESTAMP} AS timestamp FROM pi_entries
      WHERE conversation_id = ? AND id BETWEEN ? AND ?
        AND json_extract(record, '$.kind') IN (?, 'pi.user', 'pi.assistant', 'pi.tool-result')
        AND json_type(record, '$.model[0].timestamp') = 'integer'
        AND ${TIMESTAMP} BETWEEN 1 AND ?
      ORDER BY id ${order} LIMIT 1`,
    ROOT_CONVERSATION_ID, min, max, BOARD_MESSAGE_KIND, MAX_ID,
  ).toArray()[0];
  return recordedTime(row?.timestamp);
}

/**
 * Read committed evidence in one synchronous DO turn; do not start a harness,
 * store a second log, or copy engine record bodies through the SQL boundary.
 */
export function readSessionSummary(sql: TaskSql, objectId: string, sessionId?: string): HostedSessionInspection {
  const prefix = `${objectId}:`;
  try {
    let anchor = 0;
    let openedAt: number | null = null;
    let boundary: Record<string, unknown> | undefined;
    if (sessionId === undefined) {
      const reset = resetBetween(sql, 1, MAX_ID, "DESC");
      if (reset !== undefined) {
        if (typeof reset.id !== "number" || !Number.isSafeInteger(reset.id) || reset.id <= 0) return { state: "unknown" };
        anchor = reset.id;
        openedAt = recordedTime(reset.timestamp);
      }
    } else {
      if (!sessionId.startsWith(prefix)) return { state: "unobserved" };
      const suffix = sessionId.slice(prefix.length);
      anchor = Number(suffix);
      if (!Number.isSafeInteger(anchor) || anchor < 0 || String(anchor) !== suffix) return { state: "unobserved" };
      if (anchor !== 0) {
        const reset = resetBetween(sql, anchor, anchor, "ASC");
        if (reset === undefined) return { state: "unobserved" };
        openedAt = recordedTime(reset.timestamp);
      }
      boundary = resetBetween(sql, anchor + 1, MAX_ID, "ASC");
    }

    const upper = typeof boundary?.id === "number" ? boundary.id - 1 : MAX_ID;
    const run = sql.exec(
      `SELECT status, json_extract(record, '$.type') AS type,
              json_extract(record, '$.entry') AS entry,
              json_extract(record, '$.reason') AS reason
         FROM pi_submissions
        WHERE conversation_id = ? AND id BETWEEN ? AND ?
          AND json_extract(record, '$.type') IN ('input', 'continue')
        ORDER BY id DESC LIMIT 1`,
      ROOT_CONVERSATION_ID, anchor + 1, upper,
    ).toArray()[0];
    const startedAt = openedAt ?? activityBetween(sql, anchor + 1, upper, "ASC");
    if (anchor === 0 && run === undefined && startedAt === null) return { state: "unobserved" };
    const latest = activityBetween(sql, anchor + 1, upper, "DESC") ?? startedAt;
    const placedHere = typeof run?.entry === "number" && run.entry > anchor && run.entry <= upper;
    return {
      state: "observed",
      session: {
        sessionId: `${prefix}${anchor}`,
        startedAt,
        lastActivityAt: latest,
        status: boundary === undefined ? "open" : "reset",
        endedAt: recordedTime(boundary?.timestamp),
        lastRunFailureCode: run?.status === "unanswered" && (placedHere || run.type === "continue")
          ? runFailureCode(run.reason)
          : null,
      },
    };
  } catch {
    return { state: "unknown" };
  }
}

/** Resolve the same proven generation and reset bounds used by the summary. */
export function readSessionBounds(
  sql: TaskSql,
  objectId: string,
  sessionId: string,
): { anchor: number; upper: number } | null {
  const summary = readSessionSummary(sql, objectId, sessionId);
  if (summary.state !== "observed" || summary.session.sessionId !== sessionId) return null;
  try {
    const anchor = Number(sessionId.slice(objectId.length + 1));
    const boundary = resetBetween(sql, anchor + 1, MAX_ID, "ASC");
    if (boundary === undefined) return { anchor, upper: MAX_ID };
    if (typeof boundary.id !== "number" || !Number.isSafeInteger(boundary.id) || boundary.id <= anchor) return null;
    return { anchor, upper: boundary.id - 1 };
  } catch {
    return null;
  }
}

/**
 * Install during session storage initialization, not during a diagnostics read.
 * Pi's ordinary head index also contains compaction anchors, which are not resets.
 */
export function ensureDiagnosticResetIndex(sql: TaskSql): void {
  try {
    if (!sql.exec("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='pi_entries'").toArray()[0])
      return;
    sql.exec(`CREATE INDEX IF NOT EXISTS cadence_diagnostic_reset_heads
      ON pi_entries (conversation_id, id) WHERE head=id`);
  } catch {
    // The indexed diagnostics read will refuse. An unavailable support index
    // must not take down ordinary chat or transcript reads.
    console.warn("Hosted conversation diagnostics index unavailable.");
  }
}

/** Diagnostics needs only a proven reset window, not the summary's activity or run scans. */
export function readBoundedDiagnosticWindow(
  sql: TaskSql,
  objectId: string,
  sessionId: string,
): { anchor: number; upper: number } | null {
  const prefix = `${objectId}:`;
  if (!sessionId.startsWith(prefix)) return null;
  const suffix = sessionId.slice(prefix.length);
  const anchor = Number(suffix);
  if (!Number.isSafeInteger(anchor) || anchor < 0 || String(anchor) !== suffix) return null;
  const maxHeadBytes = 128 * 1024;
  try {
    if (anchor !== 0 && !sql.exec(
      `SELECT id FROM pi_entries WHERE conversation_id=? AND id=? AND head=id
       AND CASE WHEN length(CAST(record AS BLOB)) <= ? THEN json_extract(record, '$.kind') END = ?`,
      ROOT_CONVERSATION_ID, anchor, maxHeadBytes, RESET_KIND,
    ).toArray()[0]) return null;

    const next = sql.exec(
      `SELECT id, CASE WHEN length(CAST(record AS BLOB)) <= ?
                      THEN json_extract(record, '$.kind') END AS kind
       FROM pi_entries INDEXED BY cadence_diagnostic_reset_heads
       WHERE conversation_id=? AND id>? AND head=id ORDER BY id LIMIT 1`,
      maxHeadBytes, ROOT_CONVERSATION_ID, anchor,
    ).toArray()[0];
    if (!next) return { anchor, upper: MAX_ID };
    if (next.kind !== RESET_KIND || typeof next.id !== "number" ||
        !Number.isSafeInteger(next.id) || next.id <= anchor) return null;
    return { anchor, upper: next.id - 1 };
  } catch {
    return null;
  }
}
