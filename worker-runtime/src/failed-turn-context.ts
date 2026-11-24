import type { Message } from "@earendil-works/pi-ai";
import { GenerationTask, hook, type Extension } from "@earendil-works/pi-durable";
import { failedTurnReason, terminalError } from "./transcript.ts";

/**
 * CAD-2752: show the model that its previous turn failed.
 *
 * pi-ai drops an assistant message whose `stopReason` is `error` or `aborted`
 * when it replays context, so the model never learns the turn failed and will
 * say nothing went wrong. `GenerationTask.beforeRequest` runs before that drop
 * and its result is never committed, so a terminal failed turn becomes one
 * short note for this request only. The note carries the sanitized reason the
 * user saw, never the raw provider text.
 */

/** Longest stretch of the failed turn's own text the note repeats. */
const PARTIAL_TEXT_CAP = 400;

type Assistant = Extract<Message, { role: "assistant" }>;
type ToolResult = Extract<Message, { role: "toolResult" }>;

function failed(message: Message | undefined): message is Assistant {
  return (
    message?.role === "assistant" &&
    (message.stopReason === "error" || message.stopReason === "aborted")
  );
}

function partialText(message: Assistant): string {
  const text = message.content
    .flatMap((part) => (part.type === "text" ? [part.text] : []))
    .join("\n")
    .trim();
  return text.length <= PARTIAL_TEXT_CAP
    ? text
    : `${text.slice(0, PARTIAL_TEXT_CAP)}...`;
}

function failureNote(message: Assistant, ran: readonly ToolResult[]): string {
  const reason =
    message.stopReason === "aborted"
      ? "The turn was stopped."
      : failedTurnReason(message.errorMessage);
  const partial = partialText(message);
  const names = [...new Set(ran.map((result) => result.toolName))];
  return [
    `[Your previous turn did not finish: ${reason}`,
    partial === "" ? "" : ` Text you had written before the failure: ${JSON.stringify(partial)}.`,
    names.length === 0
      ? " No tool call from that turn completed.]"
      : ` These tool calls from that turn ran and may have taken effect: ${names.join(", ")}.]`,
  ].join("");
}

/**
 * Replace each terminal failed or aborted assistant message, and the tool
 * results that followed it, with one note. A failed message that a later
 * message retried or continued is left alone: pi drops it and the outcome
 * speaks for itself. Messages with no such failure come back unchanged.
 */
export function withFailedTurnNotes(messages: readonly Message[]): readonly Message[] {
  let out: Message[] | undefined;
  let copied = 0;
  for (let index = 0; index < messages.length; index++) {
    const message = messages[index];
    if (!failed(message)) continue;
    const ids = new Set(
      message.content.flatMap((part) => (part.type === "toolCall" ? [part.id] : [])),
    );
    const ran: ToolResult[] = [];
    let after = index + 1;
    for (let next = messages[after]; next?.role === "toolResult" && ids.has(next.toolCallId); next = messages[++after])
      ran.push(next);
    const next = messages[after];
    const terminal =
      next === undefined
        ? message.stopReason === "error" && ran.length === 0 && terminalError(messages, index)
        : next.role === "user";
    if (!terminal) continue;
    out ??= [];
    out.push(...messages.slice(copied, index));
    out.push({
      ...message,
      content: [{ type: "text", text: failureNote(message, ran) }],
      stopReason: "stop",
      errorMessage: undefined,
    });
    copied = after;
    index = after - 1;
  }
  if (out === undefined) return messages;
  out.push(...messages.slice(copied));
  return out;
}

export function failedTurnContextExtension(): Extension {
  return {
    name: "hosted-failed-turn-context",
    hooks: [
      hook(GenerationTask, {
        beforeRequest(request) {
          const messages = withFailedTurnNotes(request.messages);
          return messages === request.messages ? undefined : { messages };
        },
      }),
    ],
  };
}
