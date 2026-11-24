import type { AssistantMessage, Message } from "@earendil-works/pi-ai";
import type { EntryRecord } from "@earendil-works/pi-durable";
import { describe, expect, it } from "bun:test";
import { withFailedTurnNotes } from "../src/failed-turn-context.ts";
import { threadEntries } from "../src/transcript.ts";

const user = (text: string): Message => ({ role: "user", content: text, timestamp: 1 });

const assistant = (
  content: AssistantMessage["content"],
  stopReason: AssistantMessage["stopReason"],
  errorMessage?: string,
): AssistantMessage => ({
  role: "assistant",
  api: "openai-completions",
  provider: "openai",
  model: "synthetic",
  timestamp: 1,
  stopReason,
  ...(errorMessage === undefined ? {} : { errorMessage }),
  usage: {
    input: 0,
    output: 0,
    cacheRead: 0,
    cacheWrite: 0,
    totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
  },
  content,
});

const text = (message: Message | undefined): string => {
  if (message?.role !== "assistant") throw new Error("not an assistant message");
  return message.content.map((part) => (part.type === "text" ? part.text : "")).join("");
};

describe("failed turn context note", () => {
  it("replaces a failed turn followed by a user message with a note holding the sanitized reason", () => {
    const secret = "upstream https://llm.internal/v1 key sk-test timed out";
    const messages: Message[] = [
      user("hi"),
      assistant([{ type: "text", text: "Working on it" }], "error", secret),
      user("what went wrong?"),
    ];
    const next = withFailedTurnNotes(messages);
    expect(next).toHaveLength(3);
    expect(next[2]).toBe(messages[2]!);
    const note = text(next[1]);
    expect(note).toBe(
      '[Your previous turn did not finish: The model request timed out. Text you had written before the failure: "Working on it". No tool call from that turn completed.]',
    );
    expect(note).not.toContain("llm.internal");
    expect(messages[1]!.role === "assistant" && messages[1]!.stopReason).toBe("error");
  });

  it("names the tool whose call completed", () => {
    const messages: Message[] = [
      user("run it"),
      assistant([{ type: "toolCall", id: "c1", name: "bash", arguments: {} }], "error", "boom"),
      {
        role: "toolResult",
        toolCallId: "c1",
        toolName: "bash",
        content: [{ type: "text", text: "done" }],
        isError: false,
        timestamp: 1,
      },
      user("what happened?"),
    ];
    const next = withFailedTurnNotes(messages);
    expect(next).toHaveLength(3);
    expect(text(next[1])).toBe(
      "[Your previous turn did not finish: The model request failed. These tool calls from that turn ran and may have taken effect: bash.]",
    );
  });

  it("leaves a context with no failure exactly as it was", () => {
    const messages: Message[] = [
      user("hi"),
      assistant([{ type: "text", text: "hello" }], "stop"),
      user("again"),
    ];
    expect(withFailedTurnNotes(messages)).toBe(messages);
  });

  it("leaves a failed message that a later message retried", () => {
    const messages: Message[] = [
      user("hi"),
      assistant([], "error", "overloaded"),
      assistant([{ type: "text", text: "hello" }], "stop"),
    ];
    expect(withFailedTurnNotes(messages)).toBe(messages);
  });
});

describe("failed turn reasons", () => {
  const reasonOf = (errorMessage: string) => {
    const record = {
      id: 1,
      conversationId: 1,
      kind: "pi.assistant",
      model: [assistant([], "error", errorMessage)],
    } as unknown as EntryRecord;
    const entry = threadEntries([record, { ...record, id: 2, kind: "pi.user", model: [user("next")] } as EntryRecord], false).find(
      (item) => item.kind === "turn_result",
    );
    return (entry?.payload as { error: string }).error;
  };

  it("maps each gateway message to its reason", () => {
    expect(reasonOf("The model reached its output limit while writing a tool call.")).toBe(
      "The model ran out of output space while writing a tool call. Ask for the content in smaller parts.",
    );
    expect(reasonOf("The model wrote a tool call larger than the supported limit.")).toBe(
      "The model wrote a tool call that was too large. Ask for the content in smaller parts.",
    );
    expect(reasonOf("The model provider timed out while streaming.")).toBe(
      "The model request timed out.",
    );
    expect(reasonOf("something unrelated broke")).toBe("The model request failed.");
  });
});
