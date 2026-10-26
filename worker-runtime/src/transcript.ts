import type { EntryRecord } from "@earendil-works/pi-durable";
import { BOARD_MESSAGE_KIND, type ThreadEntry } from "./contract.ts";

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
    for (const message of record.model ?? []) {
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
        });
      }
    }
  }
  return entries;
}
