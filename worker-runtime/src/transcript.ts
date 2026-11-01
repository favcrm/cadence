import { isContextOverflow } from "@earendil-works/pi-ai/utils/overflow";
import { isRetryableAssistantError } from "@earendil-works/pi-ai/utils/retry";
import type { EntryRecord } from "@earendil-works/pi-durable";
import { BOARD_MESSAGE_KIND, type ThreadEntry } from "./contract.ts";
import { appPayload, parseEntryAppScope } from "./operation-scope.ts";

/** The private engine's default retry.maxRetries: attempts 1..3 retry. */
const PI_MAX_RETRIES = 3;

const word = (alternatives: string) =>
  new RegExp(`(?<![a-z0-9])(?:${alternatives})(?![a-z0-9])`, "i");
const TIMEOUT = /timed?.?out|timeout|deadline.?exceeded/i;
const UNREACHABLE =
  /connection.?(?:refused|reset|lost|error)|network.?error|(?:temporarily.?)?unavailable|upstream.?(?:connect|request.?failed)/i;
const UNREACHABLE_STATUS = word("502|503|504");
const BUSY = /rate.?limit|too many requests|overloaded|server.?busy/i;
const BUSY_STATUS = word("429");
const REFUSED = /refused|forbidden|unauthori[sz]ed|denied/i;
const REFUSED_STATUS = word("401|402|403");

/** Classify private provider text; never expose its URLs, keys or diagnostics. */
function failedTurnReason(errorMessage: string | undefined): string {
  const raw = errorMessage ?? "";
  if (raw.includes("billing_unavailable")) return "billing_unavailable";
  if (TIMEOUT.test(raw)) return "The model request timed out.";
  if (UNREACHABLE.test(raw) || UNREACHABLE_STATUS.test(raw))
    return "The model service could not be reached.";
  if (BUSY.test(raw) || BUSY_STATUS.test(raw))
    return "The model is busy right now.";
  if (REFUSED.test(raw) || REFUSED_STATUS.test(raw))
    return "The model request was refused.";
  return "The model request failed.";
}

type PiMessage = NonNullable<EntryRecord["model"]>[number];

/**
 * Retry/compaction errors stay hidden until their outcome is known. A stream
 * must never emit a failure entry that a subsequent successful retry removes.
 */
function terminalError(messages: readonly PiMessage[], index: number): boolean {
  const message = messages[index];
  if (message?.role !== "assistant" || message.stopReason !== "error")
    return false;
  const next = messages[index + 1];
  if (next !== undefined) return next.role === "user";
  if (isRetryableAssistantError(message)) {
    let failures = 0;
    for (let i = index; i >= 0; i--) {
      const earlier = messages[i];
      if (earlier?.role !== "assistant" || earlier.stopReason !== "error")
        break;
      failures += 1;
    }
    return failures > PI_MAX_RETRIES;
  }
  return !isContextOverflow(message);
}

/** One line for the trace's tool row; never the full argument JSON. */
function toolCallText(name: string, args: Record<string, unknown>): string {
  for (const key of ["command", "cmd", "path", "query", "url"]) {
    const value = args[key];
    if (typeof value === "string" && value.trim() !== "")
      return `${name}: ${value}`;
  }
  const json = JSON.stringify(args);
  return json === undefined || json === "{}" ? name : `${name} ${json}`;
}

/**
 * Map the engine's committed records, including durable input metadata, into
 * the board's entries.
 *
 * Message identity is stored alongside the input, never inferred from its
 * text. Thinking is an owner view: a caller that does not ask for it gets
 * none, redacted or not.
 */
export function threadEntries(
  records: readonly EntryRecord[],
  includeThinking: boolean,
): ThreadEntry[] {
  const entries: ThreadEntry[] = [];
  const all = records.flatMap((record) => record.model ?? []);
  let position = -1;
  for (const record of records) {
    const data = record.data;
    const messageId =
      record.kind === BOARD_MESSAGE_KIND &&
      data !== null &&
      typeof data === "object" &&
      !Array.isArray(data) &&
      typeof data.messageId === "string"
        ? data.messageId
        : undefined;
    // The app scope travels with the stored entry, so a reconnect, a rebuild
    // and a replayed retry all project the envelope the operation was admitted
    // with, never a scope re-derived from the caller. A stored scope that does
    // not validate fails the read: serving the entry without it would present
    // an operation as Home, which is a silent weakening of its envelope.
    const app = parseEntryAppScope(record.kind, data);
    // The board's own operator-entry payload carries the verified binding as
    // `{install_id, context_id?}`, which is what its retry sends back.
    const payload = app === null ? undefined : { app: appPayload(app) };
    for (const message of record.model ?? []) {
      position += 1;
      // Tool results live in their own message, paired to the assistant tool
      // call by id. The board's tools group consumes both kinds; mapping them
      // here keeps the result out of a standalone system/Details row.
      if (message.role === "toolResult") {
        const text = message.content
          .flatMap((part) => (part.type === "text" ? [part.text] : []))
          .join("\n")
          .trim();
        entries.push({
          seq: entries.length + 1,
          role: "agent",
          kind: "tool_result",
          text,
          payload: {
            tool_use_id: message.toolCallId,
            ...(message.isError ? { is_error: true as const } : {}),
          },
        });
        continue;
      }
      const parts =
        typeof message.content === "string"
          ? [{ type: "text" as const, text: message.content }]
          : message.content;
      const operator = message.role === "user";
      const agent = message.role === "assistant";
      // A completed response is distinct from a tool turn. The board's answer
      // kind is the presentation contract: mark only the last text part of a
      // terminal response, leaving earlier text as commentary.
      const terminal =
        agent &&
        (message.stopReason === "stop" || message.stopReason === "length");
      const textCount = parts.filter(
        (part) => part.type === "text" && part.text.trim().length > 0,
      ).length;
      let textSeen = 0;
      for (const part of parts) {
        if (part.type === "thinking") {
          if (!includeThinking || part.thinking.trim().length === 0) continue;
          entries.push({
            seq: entries.length + 1,
            role: "agent",
            kind: "thinking",
            text: part.thinking.trim(),
            ...(part.redacted === true
              ? { payload: { redacted: true as const } }
              : {}),
          });
          continue;
        }
        if (part.type === "toolCall") {
          entries.push({
            seq: entries.length + 1,
            role: "agent",
            kind: "tool_call",
            text: toolCallText(
              part.name,
              part.arguments as Record<string, unknown>,
            ),
            payload: { tool_use_id: part.id },
          });
          continue;
        }
        if (part.type !== "text" || part.text.trim().length === 0) continue;
        textSeen += 1;
        const final = agent && terminal && textSeen === textCount;
        entries.push({
          seq: entries.length + 1,
          role: operator ? "operator" : agent ? "agent" : "system",
          kind: agent ? (final ? "turn_result" : "assistant_text") : "message",
          text: part.text,
          ...(operator && messageId !== undefined
            ? { message: messageId }
            : {}),
          ...(operator && payload !== undefined ? { payload } : {}),
        });
      }
      if (message.role === "assistant" && terminalError(all, position)) {
        entries.push({
          seq: entries.length + 1,
          role: "agent",
          kind: "turn_result",
          text: "",
          payload: {
            status: "failed",
            error: failedTurnReason(message.errorMessage),
          },
        });
      }
    }
  }
  return entries;
}
