import { describe, expect, it } from "bun:test";
import { createAI } from "agents/models/pi-ai";
import { defaultModel, meteredBinding } from "../src/model.ts";

const chunk = (delta: Record<string, unknown>, finish: string | null = null) =>
  `data: ${JSON.stringify({
    id: "synthetic-response",
    object: "chat.completion.chunk",
    created: 1,
    model: "synthetic-serving-model",
    choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`;
const usage = `data: ${JSON.stringify({
  id: "synthetic-response",
  object: "chat.completion.chunk",
  created: 1,
  model: "synthetic-serving-model",
  choices: [],
  usage: { prompt_tokens: 7, completion_tokens: 3, total_tokens: 10 },
})}\n\n`;

// Exercise the actual private adapter parser, not a mocked model SDK.
async function parse(body: string) {
  const binding = meteredBinding(
    {
      requestModel: async () =>
        new Response(body, {
          headers: { "content-type": "text/event-stream" },
        }),
    },
    () => ({ conversation: "1", task: "2", cutoff: "3" }),
  );
  const ai = createAI({ binding: binding as Ai });
  const stream = ai.streamSimple(
    defaultModel("default"),
    {
      messages: [
        {
          role: "user",
          content: [{ type: "text", text: "hello" }],
          timestamp: 1000,
        },
      ],
    },
    {},
  );
  const events: string[] = [];
  for await (const event of stream) events.push(event.type);
  return { events, message: await stream.result() };
}

describe("private hosted model stream adapter", () => {
  it("assembles text deltas and preserves terminal usage", async () => {
    const { events, message } = await parse(
      chunk({ role: "assistant", content: "" }) +
        chunk({ content: "hel" }) +
        chunk({ content: "lo" }) +
        chunk({}, "stop") +
        usage +
        "data: [DONE]\n\n",
    );
    expect(events).toContain("done");
    expect(message.stopReason).toBe("stop");
    expect(message.usage.input).toBe(7);
    expect(message.usage.output).toBe(3);
    expect(message.content.find((part) => part.type === "text")).toMatchObject({
      type: "text",
      text: "hello",
    });
  });

  it("assembles split tool arguments as a tool invocation, never plain text", async () => {
    const { message } = await parse(
      chunk({ role: "assistant", content: "" }) +
        chunk({
          tool_calls: [
            {
              index: 0,
              id: "call_1",
              type: "function",
              function: { name: "read_record", arguments: '{"id":' },
            },
          ],
        }) +
        chunk({
          tool_calls: [{ index: 0, function: { arguments: '"item_7"}' } }],
        }) +
        chunk({}, "tool_calls") +
        usage +
        "data: [DONE]\n\n",
    );
    expect(message.stopReason).toBe("toolUse");
    expect(
      message.content.find((part) => part.type === "toolCall"),
    ).toMatchObject({
      type: "toolCall",
      id: "call_1",
      name: "read_record",
      arguments: { id: "item_7" },
    });
  });
});
