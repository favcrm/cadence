import { authorizeTool, authorizeTools } from "./tool-policy.ts";
import type { ToolAuthority } from "./tool-policy.ts";

/**
 * `@cadence/hosted-runtime/testing`: the real guards an application test needs
 * and opaque record fixtures for the engine's stored and live shapes.
 *
 * This entry imports no engine SDK and no Workers platform module. The fixture
 * builders return `unknown`, so a consumer never handles a private record type
 * and the engine's own shapes stay inside this package.
 */

export { authorizeTool, authorizeTools };
export type { ToolAuthority };

/** The assistant parts a fixture record may carry. */
export type AssistantFixtureContent = readonly (
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string; redacted?: boolean }
)[];

/** One stored operator input, as the engine persists it. */
export function userRecord(id: number, text: string): unknown {
  return {
    id,
    conversationId: 1,
    kind: "pi.user",
    model: [{ role: "user", content: text, timestamp: 1000 }],
  };
}

/** One stored completed assistant turn, as the engine persists it. */
export function assistantRecord(
  id: number,
  content: AssistantFixtureContent,
): unknown {
  return {
    id,
    conversationId: 1,
    kind: "pi.assistant",
    model: [
      {
        role: "assistant",
        api: "openai-completions",
        provider: "openai",
        model: "synthetic",
        timestamp: 1000,
        stopReason: "stop",
        usage: {
          input: 0,
          output: 0,
          cacheRead: 0,
          cacheWrite: 0,
          totalTokens: 0,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
        },
        content,
      },
    ],
  };
}

/**
 * The engine's committed batch for one live assistant turn: a thinking delta
 * and block per thinking part, then the committed message. A test hands this
 * to a stream listener so the real transcript mapping runs over live records.
 */
export function liveBatch(entry: unknown): readonly unknown[] {
  // The consumer treats fixture records as opaque; these guards only name the
  // one stored message shape `assistantRecord` builds.
  if (typeof entry !== "object" || entry === null || !("model" in entry)) {
    throw new Error("liveBatch requires a record from assistantRecord");
  }
  const model = entry.model;
  if (!Array.isArray(model) || model.length === 0) {
    throw new Error("liveBatch requires a record with one stored message");
  }
  const message: unknown = model[0];
  if (
    typeof message !== "object" ||
    message === null ||
    !("content" in message) ||
    !Array.isArray(message.content)
  ) {
    throw new Error("liveBatch requires an assistant message with content");
  }
  const changes: unknown[] = [];
  message.content.forEach((block: unknown, contentIndex: number) => {
    if (
      typeof block === "object" &&
      block !== null &&
      "type" in block &&
      block.type === "thinking" &&
      "thinking" in block &&
      typeof block.thinking === "string"
    ) {
      changes.push({
        type: "thinking_delta",
        contentIndex,
        delta: block.thinking,
      });
    }
    changes.push({ type: "block", contentIndex, block });
  });
  changes.push({ type: "message", message });
  return [
    {
      type: "message_update",
      usage: "usage" in message ? message.usage : undefined,
      changes,
    },
    { type: "message_end", entry },
  ];
}
