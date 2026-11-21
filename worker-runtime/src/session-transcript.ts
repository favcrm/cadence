import { ROOT_CONVERSATION_ID, type EntryRecord } from "@earendil-works/pi-durable";
import { BOARD_MESSAGE_KIND, type ThreadProjection } from "./contract.ts";
import type { TaskSql } from "./identity.ts";
import { readSessionBounds } from "./session-summary.ts";
import { projectedThreadEntries } from "./transcript.ts";

// The same allowlist governs page rows and accepted cursor anchors.
const VISIBLE = `(json_extract(record, '$.kind') = 'pi.assistant' OR
  (json_extract(record, '$.kind') = ? AND json_extract(record, '$.data.transcriptRole') = 'operator'))`;

type TranscriptMessage = { role: "operator" | "agent"; text: string; created?: string };

// Limit both the SQL transfer and the serialized response. A record is atomic:
// if it cannot fit on an empty page, refuse rather than truncate or skip it.
// Seven records (including lookahead) transfer at most 448 KiB of SQL text.
const PAGE_RECORDS = 6;
const MAX_RECORD_BYTES = 64 * 1024;
const MAX_RESPONSE_BYTES = 48 * 1024;
const MAX_MESSAGES = 100;
const encoder = new TextEncoder();

export function readSessionTranscript(
  sql: TaskSql,
  objectId: string,
  sessionId: string,
  after: number,
  project?: ThreadProjection,
): { messages: TranscriptMessage[]; nextCursor: number | null } | null {
  if (!Number.isSafeInteger(after) || after < 0) return null;
  const bounds = readSessionBounds(sql, objectId, sessionId);
  if (bounds === null) return null;
  try {
    if (after !== 0) {
      if (after <= bounds.anchor || after > bounds.upper) return null;
      const cursor = sql.exec(
        `SELECT id FROM pi_entries WHERE conversation_id = ? AND id = ?
           AND ${VISIBLE} LIMIT 1`,
        ROOT_CONVERSATION_ID, after, BOARD_MESSAGE_KIND,
      ).toArray()[0];
      if (cursor === undefined) return null;
    }
    const rows = sql.exec(
      `SELECT id, CASE WHEN length(CAST(record AS BLOB)) <= ? THEN record END AS record
         FROM pi_entries
        WHERE conversation_id = ? AND id > ? AND id <= ?
          AND ${VISIBLE}
        ORDER BY id ASC LIMIT ?`,
      MAX_RECORD_BYTES, ROOT_CONVERSATION_ID, after || bounds.anchor,
      bounds.upper, BOARD_MESSAGE_KIND, PAGE_RECORDS + 1,
    ).toArray();
    const messages: TranscriptMessage[] = [];
    let processed = after;
    for (const row of rows.slice(0, PAGE_RECORDS)) {
      if (typeof row.id !== "number" || !Number.isSafeInteger(row.id) || typeof row.record !== "string") return null;
      const record = JSON.parse(row.record) as EntryRecord;
      // An ID proves identity, not visibility. Only the host's trusted
      // admission may mark a board input as user-visible.
      const data = record.data;
      const visibleInput = record.kind === BOARD_MESSAGE_KIND &&
        data !== null && typeof data === "object" && !Array.isArray(data) &&
        data.transcriptRole === "operator";
      const entries = visibleInput || record.kind === "pi.assistant"
        ? projectedThreadEntries([record], false, project) : [];
      const additions = entries.flatMap((entry): TranscriptMessage[] => {
        if (visibleInput && entry.role === "operator" && entry.kind === "message") {
          return [{ role: "operator" as const, text: entry.text, ...(entry.created ? { created: entry.created } : {}) }];
        }
        if (record.kind === "pi.assistant" && entry.role === "agent" &&
            (entry.kind === "assistant_text" || entry.kind === "turn_result") && entry.text) {
          return [{ role: "agent" as const, text: entry.text, ...(entry.created ? { created: entry.created } : {}) }];
        }
        return [];
      });
      const candidate = [...messages, ...additions];
      // Reserve the cursor field even for the last page.
      if (candidate.length > MAX_MESSAGES ||
          encoder.encode(JSON.stringify({ messages: candidate, nextCursor: row.id })).byteLength > MAX_RESPONSE_BYTES) {
        if (processed === after) return null;
        return { messages, nextCursor: processed };
      }
      messages.push(...additions);
      processed = row.id;
    }
    return { messages, nextCursor: rows.length > PAGE_RECORDS ? processed : null };
  } catch {
    // Missing or incompatible storage, malformed records and invalid timestamps
    // are not evidence that this exact transcript is empty.
    return null;
  }
}
