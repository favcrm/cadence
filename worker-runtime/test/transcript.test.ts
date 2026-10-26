import type { AssistantMessage } from "@earendil-works/pi-ai";
import type { EntryRecord } from "@earendil-works/pi-durable";
import { describe, expect, it } from "bun:test";
import { BOARD_MESSAGE_KIND } from "../src/contract.ts";
import { threadEntries } from "../src/transcript.ts";

const input = (
  id: string,
  text: string,
  data?: EntryRecord["data"],
): EntryRecord => ({
  id: Number(id) as EntryRecord["id"],
  conversationId: 1 as EntryRecord["conversationId"],
  kind: data === undefined ? "pi.user" : BOARD_MESSAGE_KIND,
  model: [{ role: "user", content: text, timestamp: 1000 }],
  ...(data === undefined ? {} : { data }),
});

const assistant = (
  id: string,
  content: AssistantMessage["content"],
  stopReason: AssistantMessage["stopReason"],
): EntryRecord => ({
  id: Number(id) as EntryRecord["id"],
  conversationId: 1 as EntryRecord["conversationId"],
  kind: "pi.assistant",
  model: [
    {
      role: "assistant",
      api: "openai-completions",
      provider: "openai",
      model: "synthetic",
      timestamp: 1000,
      stopReason,
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
});

const toolResult = (
  id: string,
  toolCallId: string,
  text: string,
): EntryRecord => ({
  id: Number(id) as EntryRecord["id"],
  conversationId: 1 as EntryRecord["conversationId"],
  kind: "pi.tool-result",
  model: [
    {
      role: "toolResult",
      toolCallId,
      toolName: "bash",
      content: [{ type: "text", text }],
      isError: false,
      timestamp: 1000,
    },
  ],
});

describe("board transcript identity", () => {
  it("keeps repeated and differently punctuated inputs bound to their recorded message ids", () => {
    const records = [
      input("1", "Hello.", { messageId: "first" }),
      input("2", "Hello.", { messageId: "second" }),
      input("3", "hello", { messageId: "third" }),
    ];
    expect(threadEntries(records, true)).toEqual([
      {
        seq: 1,
        role: "operator",
        kind: "message",
        text: "Hello.",
        message: "first",
      },
      {
        seq: 2,
        role: "operator",
        kind: "message",
        text: "Hello.",
        message: "second",
      },
      {
        seq: 3,
        role: "operator",
        kind: "message",
        text: "hello",
        message: "third",
      },
    ]);
  });

  it("retains legacy history without inventing an id from matching text or untrusted entry data", () => {
    const legacy = input("1", "Same text");
    const other = {
      ...input("2", "Same text", { messageId: "not-a-board-send" }),
      kind: "app.webhook",
    };
    const current = input("3", "Same text", { messageId: "actual-send" });
    expect(
      threadEntries([legacy, other, current], true).map(
        (entry) => entry.message,
      ),
    ).toEqual([undefined, undefined, "actual-send"]);
  });

  it("does not attach malformed or non-operator message metadata", () => {
    const answer: EntryRecord = {
      ...input("2", "", { messageId: "not-the-reply-id" }),
      model: [
        { role: "system", content: "Provider diagnostic", timestamp: 1000 },
      ],
    };
    expect(
      threadEntries([input("1", "Question", { messageId: 42 }), answer], true),
    ).toEqual([
      { seq: 1, role: "operator", kind: "message", text: "Question" },
      { seq: 2, role: "system", kind: "message", text: "Provider diagnostic" },
    ]);
  });

  it("keeps provider thinking and answer parts separate while preserving input identity", () => {
    const answer: EntryRecord = {
      id: 2 as EntryRecord["id"],
      conversationId: 1 as EntryRecord["conversationId"],
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
            cost: {
              input: 0,
              output: 0,
              cacheRead: 0,
              cacheWrite: 0,
              total: 0,
            },
          },
          content: [
            { type: "thinking", thinking: "Provider trace", redacted: true },
            { type: "text", text: "Final answer" },
          ],
        },
      ],
    };
    expect(
      threadEntries(
        [input("1", "Question", { messageId: "input-id" }), answer],
        true,
      ),
    ).toEqual([
      {
        seq: 1,
        role: "operator",
        kind: "message",
        text: "Question",
        message: "input-id",
      },
      {
        seq: 2,
        role: "agent",
        kind: "thinking",
        text: "Provider trace",
        payload: { redacted: true },
      },
      { seq: 3, role: "agent", kind: "turn_result", text: "Final answer" },
    ]);
  });

  it("withholds thinking, redacted or not, from a reader that is not the owner", () => {
    const answer = assistant(
      "1",
      [
        { type: "thinking", thinking: "Owner-only trace" },
        { type: "thinking", thinking: "Redacted trace", redacted: true },
        { type: "text", text: "Visible answer" },
      ],
      "stop",
    );
    expect(threadEntries([answer], false)).toEqual([
      { seq: 1, role: "agent", kind: "turn_result", text: "Visible answer" },
    ]);
    expect(threadEntries([answer], true).map((entry) => entry.kind)).toEqual([
      "thinking",
      "thinking",
      "turn_result",
    ]);
  });

  it("keeps a tool-turn text as commentary and marks only a completed answer", () => {
    expect(
      threadEntries(
        [
          input("1", "Question", { messageId: "m" }),
          assistant(
            "2",
            [{ type: "text", text: "Checking the board." }],
            "toolUse",
          ),
          assistant(
            "3",
            [{ type: "text", text: "Here is the answer." }],
            "stop",
          ),
        ],
        true,
      ),
    ).toEqual([
      {
        seq: 1,
        role: "operator",
        kind: "message",
        text: "Question",
        message: "m",
      },
      {
        seq: 2,
        role: "agent",
        kind: "assistant_text",
        text: "Checking the board.",
      },
      {
        seq: 3,
        role: "agent",
        kind: "turn_result",
        text: "Here is the answer.",
      },
    ]);
  });

  it("marks only the final text part of a terminal response", () => {
    expect(
      threadEntries(
        [
          assistant(
            "1",
            [
              { type: "text", text: "First note." },
              { type: "text", text: "The answer." },
            ],
            "stop",
          ),
        ],
        true,
      ),
    ).toEqual([
      { seq: 1, role: "agent", kind: "assistant_text", text: "First note." },
      { seq: 2, role: "agent", kind: "turn_result", text: "The answer." },
    ]);
  });

  it("maps tool calls and results into the board's tools group", () => {
    expect(
      threadEntries(
        [
          assistant(
            "1",
            [
              { type: "thinking", thinking: "Check the board." },
              { type: "text", text: "Checking the board." },
              {
                type: "toolCall",
                id: "t1",
                name: "bash",
                arguments: { command: "cadence board read" },
              },
            ],
            "toolUse",
          ),
          toolResult("2", "t1", "2 issues: PILOT-1, PILOT-2"),
          assistant("3", [{ type: "text", text: "Two issues." }], "stop"),
        ],
        true,
      ),
    ).toEqual([
      { seq: 1, role: "agent", kind: "thinking", text: "Check the board." },
      {
        seq: 2,
        role: "agent",
        kind: "assistant_text",
        text: "Checking the board.",
      },
      {
        seq: 3,
        role: "agent",
        kind: "tool_call",
        text: "bash: cadence board read",
        payload: { tool_use_id: "t1" },
      },
      {
        seq: 4,
        role: "agent",
        kind: "tool_result",
        text: "2 issues: PILOT-1, PILOT-2",
        payload: { tool_use_id: "t1" },
      },
      { seq: 5, role: "agent", kind: "turn_result", text: "Two issues." },
    ]);
  });

  it("treats a length-limited response as completed but not an errored one", () => {
    expect(
      threadEntries(
        [
          assistant(
            "1",
            [{ type: "text", text: "Truncated answer" }],
            "length",
          ),
          assistant("2", [{ type: "text", text: "Provider error" }], "error"),
        ],
        true,
      ),
    ).toEqual([
      { seq: 1, role: "agent", kind: "turn_result", text: "Truncated answer" },
      { seq: 2, role: "agent", kind: "assistant_text", text: "Provider error" },
      {
        seq: 3,
        role: "agent",
        kind: "turn_result",
        text: "",
        payload: { status: "failed", error: "The model request failed." },
      },
    ]);
  });

  // Preserved from AgenticOS AOS-389; only test-runner/module imports move.
  describe("failed assistant turns", () => {
    const failed = (id: string, errorMessage?: string): EntryRecord => {
      const record = assistant(id, [], "error");
      const [message] = record.model as [AssistantMessage];
      return {
        ...record,
        model: [{ ...message, ...(errorMessage ? { errorMessage } : {}) }],
      };
    };
    const failures = (records: EntryRecord[]) =>
      threadEntries(records, false).filter(
        (entry) => entry.payload?.status === "failed",
      );
    const reason = (message?: string) =>
      failures([failed("2", message), input("3", "again")])[0]?.payload?.error;

    it("projects a terminal error as a failed turn_result with a safe reason", () => {
      const entries = threadEntries(
        [
          input("1", "hi"),
          failed("2", "400 from https://gw.internal/v1?key=sk-secret refused"),
        ],
        false,
      );
      expect(entries[1]).toEqual({
        seq: 2,
        role: "agent",
        kind: "turn_result",
        text: "",
        payload: { status: "failed", error: "The model request was refused." },
      });
      expect(JSON.stringify(entries)).not.toContain("sk-secret");
      expect(JSON.stringify(entries)).not.toContain("gw.internal");
    });

    it("shows nothing for a retryable error that Pi then retried successfully", () => {
      const entries = threadEntries(
        [
          input("1", "hi"),
          failed("2", "429 too many requests"),
          assistant("3", [{ type: "text", text: "Hello" }], "stop"),
        ],
        false,
      );
      expect(entries.map((entry) => [entry.seq, entry.kind])).toEqual([
        [1, "message"],
        [2, "turn_result"],
      ]);
      expect(entries[1]?.text).toBe("Hello");
    });

    it("holds a last-message retryable error until retries are exhausted", () => {
      const retrying = [
        input("1", "hi"),
        failed("2", "503"),
        failed("3", "503"),
        failed("4", "503"),
      ];
      expect(failures(retrying)).toEqual([]);
      const exhausted = [...retrying, failed("5", "503")];
      expect(failures(exhausted)).toHaveLength(1);
      expect(failures(exhausted)[0]?.seq).toBe(2);
    });

    it("keeps earlier seqs stable while a held error resolves (live stream)", () => {
      const held = [input("1", "hi"), failed("2", "529 overloaded")];
      const before = threadEntries(held, false);
      const after = threadEntries(
        [...held, assistant("3", [{ type: "text", text: "ok" }], "stop")],
        false,
      );
      expect(after.slice(0, before.length)).toEqual(before);
      expect(
        after.filter((entry) => entry.payload?.status === "failed"),
      ).toEqual([]);
    });

    it("treats a retryable error followed by a new user message as final", () => {
      expect(
        failures([input("1", "hi"), failed("2", "503"), input("3", "again")]),
      ).toHaveLength(1);
    });

    it("passes our billing code through and classifies the rest", () => {
      expect(reason("403 billing_unavailable")).toBe("billing_unavailable");
      expect(reason("request timed out")).toBe("The model request timed out.");
      expect(reason("429 too many requests")).toBe(
        "The model is busy right now.",
      );
      expect(reason("403 forbidden")).toBe("The model request was refused.");
      expect(reason(undefined)).toBe("The model request failed.");
    });

    it("does not misclassify on look-alikes", () => {
      expect(reason("connection refused")).toBe(
        "The model service could not be reached.",
      );
      expect(reason("request id req-4290-x403y failed")).toBe(
        "The model request failed.",
      );
      expect(reason("read 14031 bytes then failed")).toBe(
        "The model request failed.",
      );
    });

    it("does not surface an aborted turn", () => {
      expect(threadEntries([assistant("2", [], "aborted")], false)).toEqual([]);
    });
  });
});
