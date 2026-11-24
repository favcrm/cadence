import { createHash } from "node:crypto";
import { ROOT_CONVERSATION_ID } from "@earendil-works/pi-durable";
import {
  BOARD_MESSAGE_KIND,
  type HostedInputSettlement,
  type HostedTurnDiagnosticEvent,
  type HostedTurnDiagnosticsPage,
} from "./contract.ts";
import type { TaskSql } from "./identity.ts";
import { requireOperationId } from "./operation-scope.ts";
import { readBoundedDiagnosticWindow } from "./session-summary.ts";

const PAGE_RECORDS = 24;
const MAX_EVENTS = 100;
const MAX_MODEL_MESSAGES = 4;
const MAX_RECORD_BYTES = 128 * 1024;
const IDENTIFIER = /^[A-Za-z0-9_.:-]{1,200}$/;
const MODEL = /^[A-Za-z0-9_-][A-Za-z0-9_./:-]{0,159}$/;
const STOP_REASONS = ["stop", "length", "toolUse", "error", "aborted", "pending", "deferred"] as const;
const UNANSWERED_REASONS = [
  "model_error", "no_model", "aborted", "reset", "stale", "faulted", "orphaned",
  "interrupted", "restart_limit", "nothing_to_answer", "turn_limit",
] as const;

function known<T extends string>(allowed: readonly T[], value: unknown): T | null {
  return typeof value === "string" && (allowed as readonly string[]).includes(value) ? value as T : null;
}

function safeModel(value: unknown): string | null {
  return typeof value === "string" && MODEL.test(value) && !value.includes("://") ? value : null;
}

/** A settlement has no timestamp. Show it on its exact placed input, not as a timed event. */
function inputSettlement(
  sql: TaskSql, messageId: string, entryId: number, upper: number,
): HostedInputSettlement | null {
  const rows = sql.exec(
    `WITH candidates AS MATERIALIZED (
       SELECT status, CASE WHEN length(CAST(record AS BLOB)) <= ? THEN record END AS record
       FROM pi_submissions INDEXED BY pi_submissions_by_request
       WHERE conversation_id=? AND request_id=? ORDER BY id LIMIT 2
     )
     SELECT status, json_extract(record, '$.status') AS storedStatus, json_extract(record, '$.type') AS type,
       json_extract(record, '$.entry') AS entry, json_extract(record, '$.answer') AS answer,
       substr(json_extract(record, '$.reason'), 1, 32) AS reason
     FROM candidates`,
    MAX_RECORD_BYTES, ROOT_CONVERSATION_ID, JSON.stringify(messageId),
  ).toArray();
  const row = rows[0];
  if (rows.length !== 1 || row?.type !== "input" || row.entry !== entryId || row.status !== row.storedStatus)
    return null;
  if (row.status === "unanswered") {
    return { outcome: "unanswered", reason: known(UNANSWERED_REASONS, row.reason) };
  }
  if (row.status === "done" && typeof row.answer === "number" && Number.isSafeInteger(row.answer) &&
      row.answer > entryId && row.answer <= upper) {
    return { outcome: "answered" };
  }
  return null;
}

function safeToolName(value: unknown): string | null {
  return typeof value === "string" && IDENTIFIER.test(value) ? value : null;
}

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

function boundedInteger(value: unknown, min: number, max: number): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= min && value <= max
    ? value : null;
}

interface Evidence {
  role: string;
  at: unknown;
  model: unknown;
  responseModel: unknown;
  stop: unknown;
  toolName: unknown;
  toolUseId: unknown;
  failure: { status: unknown; code: unknown } | null;
  isError: "true" | "false" | null;
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
         CASE WHEN json_extract(record,'$.kind') = ? THEN 1 ELSE 0 END AS isInput,
         CASE WHEN json_type(record, '$.data.messageId')='text'
           THEN substr(json_extract(record, '$.data.messageId'), 1, 201) END AS messageId,
         json_extract(record, '$.model[0].timestamp') AS inputAt,
         CASE WHEN json_extract(record, '$.kind') IN ('pi.assistant', 'pi.tool-result') THEN
         (SELECT json_group_array(json(evidence)) FROM (
           SELECT json_object(
             'role', json_extract(m.value, '$.role'),
             'at', json_extract(m.value, '$.timestamp'),
             'model', substr(json_extract(m.value, '$.model'), 1, 161),
             'responseModel', substr(json_extract(m.value, '$.responseModel'), 1, 161),
             'stop', json_extract(m.value, '$.stopReason'),
             'toolName', substr(json_extract(m.value, '$.toolName'), 1, 201),
             'toolUseId', json_extract(m.value, '$.toolCallId'),
             'isError', json_type(m.value, '$.isError'),
             'failure', json(CASE WHEN json_extract(m.value, '$.stopReason')='error' THEN (
               SELECT json_object(
                 'status', CASE WHEN json_type(d.value, '$.details.status')='integer'
                   THEN json_extract(d.value, '$.details.status') END,
                 'code', CASE WHEN json_type(d.value, '$.details.code')='integer'
                   THEN json_extract(d.value, '$.details.code') END
               ) FROM json_each(m.value, '$.diagnostics') d
               WHERE d.type='object' AND json_extract(d.value, '$.type')='cloudflare-error'
               ORDER BY d.key DESC LIMIT 1
             ) END),
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
        let messageId: string | undefined;
        try { messageId = requireOperationId(row.messageId); }
        catch { group.push(gap("identifier_unavailable")); }
        if (messageId !== undefined) {
          group.push({
            kind: "input", at: timestamp(row.inputAt), messageId,
            settlement: inputSettlement(sql, messageId, row.id, bounds.upper),
          });
        }
      }
      const evidence = JSON.parse(row.evidence) as Evidence[];
      if (evidence.length > MAX_MODEL_MESSAGES || evidence.some((entry) => entry.calls.length > MAX_EVENTS)) {
        group = [gap("too_many_events")];
      } else {
        for (const entry of evidence) {
          const at = timestamp(entry.at);
          if (entry.role === "toolResult") {
            const toolUseId = toolReference(entry.toolUseId);
            const toolName = safeToolName(entry.toolName);
            group.push(toolUseId === null || (entry.isError !== "true" && entry.isError !== "false")
              ? gap("identifier_unavailable")
              : { kind: "tool_result", at, toolName, toolUseId, outcome: entry.isError === "true" ? "error" : "ok" });
            continue;
          }
          group.push({
            kind: "model_attempt", at, model: safeModel(entry.model),
            responseModel: safeModel(entry.responseModel), stopReason: known(STOP_REASONS, entry.stop),
            httpStatus: boundedInteger(entry.failure?.status, 400, 599),
            providerCode: boundedInteger(entry.failure?.code, 0, 999_999),
          });
          for (const call of entry.calls) {
            const toolName = safeToolName(call.name);
            const toolUseId = toolReference(call.id);
            group.push(toolName === null || toolUseId === null
              ? gap("identifier_unavailable")
              : { kind: "tool_call", at, toolName, toolUseId });
          }
        }
      }
      if (group.length > MAX_EVENTS) group = [gap("too_many_events")];
      if (eventCount + group.length > MAX_EVENTS) break;
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
