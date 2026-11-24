import { createHash } from "node:crypto";
import { ROOT_CONVERSATION_ID } from "@earendil-works/pi-durable";
import {
  BOARD_MESSAGE_KIND,
  type HostedTurnDiagnosticEvent,
  type HostedTurnDiagnosticsPage,
} from "./contract.ts";
import type { TaskSql } from "./identity.ts";
import { requireOperationId } from "./operation-scope.ts";
import { readBoundedDiagnosticWindow } from "./session-summary.ts";
import { modelDiagnosticCode } from "./transcript.ts";

const PAGE_RECORDS = 24;
const MAX_EVENTS = 100;
const MAX_MODEL_MESSAGES = 4;
const MAX_RECORD_BYTES = 128 * 1024;
const IDENTIFIER = /^[A-Za-z0-9_.:-]{1,200}$/;
const MODEL = /^[A-Za-z0-9_-][A-Za-z0-9_./:-]{0,159}$/;
function toolReference(value: unknown): string | null {
  if (typeof value !== "string" || value.length === 0) return null;
  if (IDENTIFIER.test(value)) return value;
  // The provider owns these IDs, not our operation ID grammar. Keep exact
  // correlation for non-canonical IDs without exposing their raw contents.
  return `$sha256:${createHash("sha256").update("cadence.diagnostics.tool-use/v1\u0000").update(JSON.stringify(value)).digest("hex")}`;
}

function gap(code: Extract<HostedTurnDiagnosticEvent, { kind: "evidence_gap" }>["code"]): HostedTurnDiagnosticEvent {
  return { kind: "evidence_gap", at: null, code };
}

function timestamp(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 &&
    value <= 8_640_000_000_000_000 ? value : null;
}

interface Evidence {
  role: string;
  at: unknown;
  model: unknown;
  stop: unknown;
  error: string | null;
  toolUseId: unknown;
  isError: number | null;
  calls: { name: unknown; id: unknown }[];
}

/** Do not serialize raw prompts, answers, reasoning, or tool content out of SQLite. */
export function readSessionTurnDiagnostics(
  sql: TaskSql,
  objectId: string,
  sessionId: string,
  before?: number,
): HostedTurnDiagnosticsPage | null {
  const bounds = readBoundedDiagnosticWindow(sql, objectId, sessionId);
  if (bounds === null) return null;
  try {
    if (before !== undefined) {
      if (!Number.isSafeInteger(before) || before <= bounds.anchor || before > bounds.upper) return null;
      if (!sql.exec(
        "SELECT id FROM pi_entries WHERE conversation_id=? AND id=?",
        ROOT_CONVERSATION_ID, before,
      ).toArray()[0]) return null;
    }

    const rows = sql.exec(
      `WITH candidates AS MATERIALIZED (
         SELECT id, CASE WHEN length(CAST(record AS BLOB)) <= ? THEN record END AS record
         FROM pi_entries INDEXED BY pi_entries_by_conversation
         WHERE conversation_id=? AND id>? AND id<=? ORDER BY id DESC LIMIT ?
       )
       SELECT id, record IS NULL AS oversized,
         CASE WHEN json_extract(record,'$.kind') = ?
           AND json_extract(record,'$.data.transcriptRole') = 'operator' THEN 1 ELSE 0 END AS isInput,
         CASE WHEN json_type(record, '$.data.messageId')='text'
           THEN substr(json_extract(record, '$.data.messageId'), 1, 201) END AS messageId,
         json_extract(record, '$.model[0].timestamp') AS inputAt,
         CASE WHEN json_extract(record, '$.kind') IN ('pi.assistant', 'pi.tool-result') THEN
         (SELECT json_group_array(json(evidence)) FROM (
           SELECT json_object(
             'role', json_extract(m.value, '$.role'),
             'at', json_extract(m.value, '$.timestamp'),
             'model', substr(json_extract(m.value, '$.model'), 1, 161),
             'stop', json_extract(m.value, '$.stopReason'),
             'error', CASE WHEN json_extract(m.value, '$.stopReason')='error'
               THEN substr(json_extract(m.value, '$.errorMessage'), 1, 4096) END,
             'toolUseId', json_extract(m.value, '$.toolCallId'),
             'isError', json_extract(m.value, '$.isError'),
             'calls', json(CASE WHEN json_extract(m.value, '$.role')='assistant' THEN (
               SELECT json_group_array(json(call)) FROM (
                 SELECT json_object(
                   'name', substr(json_extract(p.value, '$.name'), 1, 201),
                   'id', json_extract(p.value, '$.id')
                 ) AS call FROM json_each(m.value, '$.content') p
                 WHERE p.type='object' AND json_extract(p.value, '$.type')='toolCall'
                 ORDER BY p.key LIMIT ?
               )
             ) ELSE '[]' END)
           ) AS evidence FROM json_each(record, '$.model') m
           WHERE m.type='object' AND json_extract(m.value, '$.role') IN ('assistant', 'toolResult')
           ORDER BY m.key LIMIT ?
         )) ELSE '[]' END AS evidence
       FROM candidates ORDER BY id DESC`,
      MAX_RECORD_BYTES, ROOT_CONVERSATION_ID, bounds.anchor,
      before === undefined ? bounds.upper : before - 1, PAGE_RECORDS + 1,
      BOARD_MESSAGE_KIND, MAX_EVENTS + 1, MAX_MODEL_MESSAGES + 1,
    ).toArray();
    if (bounds.anchor === 0 && before === undefined && rows.length === 0) return null;
    const groups: HostedTurnDiagnosticEvent[][] = [];
    let eventCount = 0;
    let oldest: number | null = null;
    for (let i = 0; i < Math.min(rows.length, PAGE_RECORDS); i++) {
      const row = rows[i]!;
      if (typeof row.id !== "number" || !Number.isSafeInteger(row.id) || typeof row.evidence !== "string") return null;
      let group: HostedTurnDiagnosticEvent[] = [];
      if (row.oversized === 1) group.push(gap("record_too_large"));
      if (row.isInput === 1) {
        try {
          group.push({ kind: "input", at: timestamp(row.inputAt), messageId: requireOperationId(row.messageId) });
        } catch {
          group.push(gap("identifier_unavailable"));
        }
      }
      const evidence = JSON.parse(row.evidence) as Evidence[];
      if (evidence.length > MAX_MODEL_MESSAGES || evidence.some((entry) => entry.calls.length > MAX_EVENTS)) {
        group = [gap("too_many_events")];
      } else for (const entry of evidence) {
        const at = timestamp(entry.at);
        if (entry.role === "toolResult") {
          const toolUseId = toolReference(entry.toolUseId);
          group.push(toolUseId === null
            ? gap("identifier_unavailable")
            : { kind: "tool_result", at, toolUseId, outcome: entry.isError === 1 ? "error" : "ok" });
          continue;
        }
        for (const call of entry.calls) {
          const toolName = typeof call.name === "string" && IDENTIFIER.test(call.name) ? call.name : null;
          const toolUseId = toolReference(call.id);
          group.push(toolName === null || toolUseId === null
            ? gap("identifier_unavailable")
            : { kind: "tool_call", at, toolName, toolUseId });
        }
        const model = typeof entry.model === "string" && MODEL.test(entry.model) &&
          !entry.model.includes("://") ? entry.model : null;
        if (entry.stop === "error") {
          group.push({ kind: "model_error", at, model, code: modelDiagnosticCode(entry.error ?? undefined) });
        } else if (entry.stop === "stop" || entry.stop === "length") {
          group.push({ kind: "response_finished", at, model, reason: entry.stop });
        }
      }
      if (group.length > MAX_EVENTS) group = [gap("too_many_events")];
      if (eventCount + group.length > MAX_EVENTS) {
        if (oldest === null) return null;
        break;
      }
      groups.push(group);
      eventCount += group.length;
      oldest = row.id;
    }
    const events: HostedTurnDiagnosticEvent[] = [];
    for (let i = groups.length - 1; i >= 0; i--) events.push(...groups[i]!);
    return { events, nextBefore: rows.length > groups.length ? oldest : null };
  } catch {
    // Missing, unreadable, or incompatible evidence is not a successful empty page.
    return null;
  }
}
