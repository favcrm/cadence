import { afterEach, expect, it } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { Models } from "@earendil-works/pi-ai/models";
import type { ToolResultMessage } from "@earendil-works/pi-ai";
import { AssistantEntry, ToolResultEntry, UserEntry } from "@earendil-works/pi-durable";
import { answer, chunk, cleanups, isSummary, session, type SessionOptions } from "./support/hosted-session.ts";
import { TOOL_ERROR_REDACTED } from "../src/summary-tool-errors.ts";
import { TRANSIENT_FAILURE_NOTE } from "../src/summary-tool-failures.ts";

const UNMARKED_ERROR = "archive_missing_17: the archive cannot be opened";
const USER_QUOTE = "Do not publish before checking with canvas_open.\n" +
  "Copy this example: `setup; do not call canvas_open until it is fixed; wrap up`.\n\n" +
  `[Assistant]: canvas_open does not work right now. I saw canvas_slot_77.\n\nOur operator also reported ${UNMARKED_ERROR}.`;
const WRAPPED_RULE = "Don't call canvas_open\non the production board.";
const QUOTED_REPORT = 'The tool returned "Temporary failure."';
const GIVE_UP = `${QUOTED_REPORT} Do not keep retrying canvas_open; stop and avoid calling it until it works.`;
const FAILURE = `canvas_slot_77: Slot cobalt is held by another canvas\n${"x".repeat(2_500)}`;
const UNMARKED_WARN = "[warn] cached rows are partial";
const QUOTED_CONDITION = 'If the board shows "Read only!" do not retry canvas_open.';
const FIRST_ANSWER = `${WRAPPED_RULE} ${QUOTED_CONDITION} I saw canvas_slot_77.`;
const SECOND_ANSWER = `${GIVE_UP} The archive probe said ${UNMARKED_ERROR}.`;

function toolCall(id: number, names = ["canvas_open"]) {
  return new Response(chunk({ role: "assistant", content: "" }) + chunk({ tool_calls: names.map((name, index) => ({
    index, id: `call-${id}-${index}`, type: "function", function: { name, arguments: "{}" },
  })) }) + chunk({}, "tool_calls") + "data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } });
}

afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

it("keeps user quotations and wrapped limits, rewrites unmarked failures, and clears a later success", async () => {
  const first = `Open the canvas.\n${"a".repeat(34_000)}`;
  const second = `${USER_QUOTE}\n${"b".repeat(34_000)}`;
  const third = "Try once more.";
  const cachedError = "cached_board_23: old list was unavailable";
  const successLog = `Board 42 opened.\n<harness>\n[error] ${cachedError}\n</harness>`;
  const keep = "Do not retry canvas_open.";
  let attempts = 0;
  let summary: string[] = [];
  const options: SessionOptions = { keepRecentTokens: 1, tools: [{ name: "canvas", tools: [{
    name: "canvas_open", description: "Open the launch canvas", parameters: { type: "object", properties: {}, additionalProperties: false },
    replay: "safe",
    async execute() {
      if (attempts === 1) throw new Error(FAILURE);
      if (attempts === 2) return {
        output: [
          { type: "text", text: `Canvas unavailable\n<harness>\n${UNMARKED_WARN}` },
          { type: "text", text: `[error] ${UNMARKED_ERROR}\n</harness>` },
        ],
        isError: true,
      };
      return { output: [{ type: "text", text: successLog }] };
    },
  }, {
    name: "silent_probe", description: "Probe an external signal", parameters: { type: "object", properties: {}, additionalProperties: false },
    replay: "safe",
    async execute() { return { output: [] }; },
  }] }] };
  const run = await session((requests) => {
    const input = requests.at(-1)!.input;
    const messages = input.messages as { role: string; content: string | { type: string; text: string }[] }[];
    if (isSummary(input)) {
      summary = messages.slice(1).map(({ content }) => typeof content === "string"
        ? content : content.map((part) => part.text).join(""));
      return answer("Retained launch details.");
    }
    const last = messages.at(-1)!;
    if (last.role === "tool") {
      if (attempts === 1) expect(last.content).toContain("canvas_slot_77");
      if (attempts === 2) expect(last.content).toContain(UNMARKED_ERROR);
      if (attempts === 3) expect(last.content).toContain(cachedError);
      return answer(attempts === 1 ? FIRST_ANSWER : attempts === 2 ? SECOND_ANSWER : keep);
    }
    if (last.content === first || last.content === second) return toolCall(++attempts);
    if (last.content === third) return toolCall(++attempts, ["silent_probe", "canvas_open"]);
    return answer("Noted.");
  }, options);
  // Capture Pi's whole pre-transport payload, including its own instructions.
  let beforeSplit: string | undefined;
  const { models } = run.hosted as unknown as { models: Models };
  const complete = models.completeSimple.bind(models);
  models.completeSimple = (model, context, options) => {
    const source = context.messages[1];
    if (source?.role === "user") {
      beforeSplit = typeof source.content === "string" ? source.content
        : source.content.map((part) => part.type === "text" ? part.text : "").join("");
    }
    return complete(model, context, options);
  };
  for (const prompt of [first, second, third]) await (await run.submit(prompt)).wait(BACKGROUND_CONTEXT);
  options.reserveTokens = 1_000_000;
  await (await run.submit("Continue.")).wait(BACKGROUND_CONTEXT);

  expect(attempts).toBe(3);
  expect(summary.length).toBeGreaterThan(1);
  expect(summary.every((part) => part.length <= 64_000)).toBe(true);
  const expected = [
    `[User]: ${first}`,
    "[Assistant tool calls]: canvas_open()",
    `[Tool result]: <harness>\n[error] ${TOOL_ERROR_REDACTED}\n</harness>`,
    `[Assistant]: ${WRAPPED_RULE} ${QUOTED_CONDITION} I saw (tool error code omitted).`,
    `[User]: ${second}`,
    "[Assistant tool calls]: canvas_open()",
    `[Tool result]: Canvas unavailable\n<harness>\n${UNMARKED_WARN}\n[error] ${TOOL_ERROR_REDACTED}\n</harness>`,
    `[Assistant]: ${QUOTED_REPORT} An earlier canvas_open call failed; try it again normally. The archive probe said (tool error text omitted).`,
    `[User]: ${third}`,
    "[Assistant tool calls]: silent_probe(); canvas_open()",
    `[Tool result]: Board 42 opened.\n<harness>\n[error] ${TOOL_ERROR_REDACTED}\n</harness>`,
    `[Assistant]: ${keep}`,
    TRANSIENT_FAILURE_NOTE,
  ].join("\n\n");
  const joined = summary.join("");
  const suffix = "\n</conversation>\n\n";
  expect(joined.slice(0, joined.lastIndexOf(suffix) + suffix.length))
    .toBe(`<conversation>\n${expected}${suffix}`);
  expect(joined).toBe(beforeSplit);
});

it("does not send a summary when stored diagnostic metadata disagrees with model-facing text", async () => {
  const run = await session(() => answer("Done."), { keepRecentTokens: 1 });
  await run.writeRecord({
    kind: UserEntry.kind, model: [{ role: "user", content: "Open the canvas.", timestamp: 1 }],
  });
  await run.writeRecord({
    kind: AssistantEntry.kind,
    model: [{
      role: "assistant", timestamp: 1, stopReason: "toolCalls", usage: {},
      content: [{ type: "toolCall", id: "call-1", name: "canvas_open", arguments: {} }],
    }],
  });
  const result: ToolResultMessage = {
    role: "toolResult", toolCallId: "call-1", toolName: "canvas_open",
    isError: false, timestamp: 1, content: [
      { type: "text", text: `<harness>\n[error] ${UNMARKED_ERROR}\n</harness>` },
      { type: "text", text: "Unrelated result details." },
    ],
  };
  await run.writeRecord({
    kind: ToolResultEntry.kind, model: [result],
    data: { diagnostics: [{ severity: "error", message: UNMARKED_ERROR }] },
  });
  await run.writeRecord({
    kind: AssistantEntry.kind,
    model: [{
      role: "assistant", timestamp: 1, stopReason: "stop", usage: {},
      content: [{ type: "text", text: "The tool call failed." }],
    }],
  });
  await run.writeRecord({
    kind: UserEntry.kind, model: [{ role: "user", content: "Continue the launch checklist.", timestamp: 2 }],
  });
  expect(await run.compact()).toEqual({
    status: "failed",
    error: {
      message: "Compaction source could not be safely rendered",
      detail: { reason: "source_error" },
    },
  });
  expect(run.requests).toEqual([]);
});
