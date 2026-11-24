import { ROOT_CONVERSATION_ID } from "@earendil-works/pi-durable";
import {
  BOARD_MESSAGE_KIND,
  type HostedTurnDiagnosticEvent,
  type HostedTurnDiagnosticsPage,
} from "./contract.ts";
import type { TaskSql } from "./identity.ts";
import { readSessionBounds } from "./session-summary.ts";
import { modelDiagnosticCode } from "./transcript.ts";

const PAGE_RECORDS = 24;
const MAX_EVENTS = 100;
const MAX_MODEL_MESSAGES = 4;
const IDENTIFIER = /^[A-Za-z0-9_.-]{1,160}$/;
const MODEL = /^[A-Za-z0-9_-][A-Za-z0-9_./:-]{0,159}$/;
const SOURCE = `(json_extract(record, '$.kind') IN ('pi.assistant', 'pi.tool-result') OR
  (json_extract(record, '$.kind') = ? AND json_extract(record, '$.data.transcriptRole') = 'operator'))`;

function identifier(value: unknown): string | null {
  return typeof value === "string" && IDENTIFIER.test(value) ? value : null;
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
  const bounds = readSessionBounds(sql, objectId, sessionId);
  if (bounds === null) return null;
  try {
    if (before !== undefined) {
      if (!Number.isSafeInteger(before) || before <= bounds.anchor || before > bounds.upper) return null;
      if (!sql.exec(
        `SELECT id FROM pi_entries WHERE conversation_id=? AND id=? AND ${SOURCE} LIMIT 1`,
        ROOT_CONVERSATION_ID, before, BOARD_MESSAGE_KIND,
      ).toArray()[0]) return null;
    }

    const rows = sql.exec(
      `SELECT id, json_extract(record, '$.kind') AS kind,
         substr(json_extract(record, '$.data.messageId'), 1, 161) AS messageId,
         json_extract(record, '$.model[0].timestamp') AS inputAt,
         (SELECT json_group_array(json(evidence)) FROM (
           SELECT json_object(
             'role', json_extract(m.value, '$.role'),
             'at', json_extract(m.value, '$.timestamp'),
             'model', substr(json_extract(m.value, '$.model'), 1, 161),
             'stop', json_extract(m.value, '$.stopReason'),
             'error', CASE WHEN json_extract(m.value, '$.stopReason')='error'
               THEN substr(json_extract(m.value, '$.errorMessage'), 1, 4096) END,
             'toolUseId', substr(json_extract(m.value, '$.toolCallId'), 1, 161),
             'isError', json_extract(m.value, '$.isError'),
             'calls', json(CASE WHEN json_extract(m.value, '$.role')='assistant' THEN (
               SELECT json_group_array(json(call)) FROM (
                 SELECT json_object(
                   'name', substr(json_extract(p.value, '$.name'), 1, 161),
                   'id', substr(json_extract(p.value, '$.id'), 1, 161)
                 ) AS call FROM json_each(m.value, '$.content') p
                 WHERE p.type='object' AND json_extract(p.value, '$.type')='toolCall'
                 ORDER BY p.key LIMIT ?
               )
             ) ELSE '[]' END)
           ) AS evidence FROM json_each(record, '$.model') m
           WHERE m.type='object' AND json_extract(m.value, '$.role') IN ('assistant', 'toolResult')
           ORDER BY m.key LIMIT ?
         )) AS evidence
       FROM (
         SELECT id, record FROM pi_entries WHERE conversation_id=? AND id>? AND id<=?
           AND ${SOURCE} ORDER BY id DESC LIMIT ?
       ) ORDER BY id DESC`,
      MAX_EVENTS + 1, MAX_MODEL_MESSAGES + 1, ROOT_CONVERSATION_ID, bounds.anchor,
      before === undefined ? bounds.upper : before - 1, BOARD_MESSAGE_KIND, PAGE_RECORDS + 1,
    ).toArray();
    const groups: HostedTurnDiagnosticEvent[][] = [];
    let eventCount = 0;
    let oldest: number | null = null;
    for (let i = 0; i < Math.min(rows.length, PAGE_RECORDS); i++) {
      const row = rows[i]!;
      if (typeof row.id !== "number" || !Number.isSafeInteger(row.id) || typeof row.evidence !== "string") return null;
      const group: HostedTurnDiagnosticEvent[] = [];
      const messageId = row.kind === BOARD_MESSAGE_KIND ? identifier(row.messageId) : null;
      if (messageId !== null) group.push({ kind: "input", at: timestamp(row.inputAt), messageId });
      const evidence = JSON.parse(row.evidence) as Evidence[];
      if (evidence.length > MAX_MODEL_MESSAGES) return null;
      for (const entry of evidence) {
        const at = timestamp(entry.at);
        if (entry.role === "toolResult") {
          const toolUseId = identifier(entry.toolUseId);
          if (toolUseId !== null) group.push({ kind: "tool_result", at, toolUseId, outcome: entry.isError === 1 ? "error" : "ok" });
          continue;
        }
        if (entry.calls.length > MAX_EVENTS) return null;
        for (const call of entry.calls) {
          const toolName = identifier(call.name);
          const toolUseId = identifier(call.id);
          if (toolName !== null && toolUseId !== null) group.push({ kind: "tool_call", at, toolName, toolUseId });
        }
        const model = typeof entry.model === "string" && MODEL.test(entry.model) &&
          !entry.model.includes("://") ? entry.model : null;
        if (entry.stop === "error") {
          group.push({ kind: "model_error", at, model, code: modelDiagnosticCode(entry.error ?? undefined) });
        } else if (entry.stop === "stop" || entry.stop === "length") {
          group.push({ kind: "response_finished", at, model, reason: entry.stop });
        }
      }
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
