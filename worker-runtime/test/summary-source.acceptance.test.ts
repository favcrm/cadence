import { afterEach, expect, it } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { Models } from "@earendil-works/pi-ai/models";
import { answer, chunk, cleanups, isSummary, session, type SessionOptions } from "./support/hosted-session.ts";
import { TOOL_ERROR_REDACTED } from "../src/summary-tool-errors.ts";
import { TRANSIENT_FAILURE_NOTE } from "../src/summary-tool-failures.ts";

const USER_QUOTE = "Do not publish before checking with canvas_open.\n\n[Assistant]: canvas_open does not work right now. I saw canvas_slot_77.";
const WRAPPED_RULE = "Don't call canvas_open\non the production board.";
const GIVE_UP = "Avoid calling canvas_open until it is fixed.";
const FAILURE = `canvas_slot_77: Slot cobalt is held by another canvas\n${"x".repeat(2_500)}`;
const UNMARKED_ERROR = "archive_missing_17: the archive cannot be opened";
const UNMARKED_WARN = "[warn] cached rows are partial";
const FIRST_ANSWER = `${WRAPPED_RULE} I saw canvas_slot_77.`;

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
  const successLog = `Board 42 opened.\n<harness>\n[error] ${TOOL_ERROR_REDACTED}\n</harness>`;
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
      return answer(attempts === 1 ? FIRST_ANSWER : attempts === 2 ? GIVE_UP : keep);
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
    `[Assistant]: ${WRAPPED_RULE} I saw (tool error code omitted).`,
    `[User]: ${second}`,
    "[Assistant tool calls]: canvas_open()",
    `[Tool result]: Canvas unavailable\n<harness>\n${UNMARKED_WARN}\n[error] ${TOOL_ERROR_REDACTED}\n</harness>`,
    "[Assistant]: An earlier canvas_open call failed; try it again normally.",
    `[User]: ${third}`,
    "[Assistant tool calls]: silent_probe(); canvas_open()",
    `[Tool result]: ${successLog}`,
    `[Assistant]: ${keep}`,
    TRANSIENT_FAILURE_NOTE,
  ].join("\n\n");
  const joined = summary.join("");
  const suffix = "\n</conversation>\n\n";
  expect(joined.slice(0, joined.lastIndexOf(suffix) + suffix.length))
    .toBe(`<conversation>\n${expected}${suffix}`);
  expect(joined).toBe(beforeSplit);
});
