import { afterEach, describe, expect, it } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { answer, chunk, cleanups, session } from "./support/hosted-session.ts";

afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

const toolCallResponse = (id: string, name: string, args: unknown) =>
  new Response(
    chunk({ role: "assistant", content: "" }) +
      chunk({
        tool_calls: [
          {
            index: 0,
            id,
            type: "function",
            function: { name, arguments: JSON.stringify(args) },
          },
        ],
      }) +
      chunk({}, "tool_calls") +
      "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );

describe("prepareArguments schema validation errors", () => {
  it("reports missing fields, wrong types, fractional integers, and unknown fields without echoing values", async () => {
    const calls: unknown[] = [];
    const seenToolResults: string[] = [];
    const run = await session(
      (requests) => {
        const last = requests.at(-1)?.input as {
          messages?: Array<{ role?: string; content?: unknown }>;
        };
        for (const msg of last?.messages ?? []) {
          if (msg.role === "tool" && typeof msg.content === "string") {
            seenToolResults.push(msg.content);
          }
        }
        switch (requests.length) {
          case 1:
            return toolCallResponse("call-missing", "demo_call", {
              app: "waba",
              operation: "whatsapp.reply.stage",
            });
          case 2:
            return toolCallResponse("call-wrong-type", "demo_call", {
              app: "waba",
              operation: "whatsapp.reply.stage",
              args: '{"secret":"SENTINEL_VALUE"}',
            });
          case 3:
            return toolCallResponse("call-fractional", "demo_call", {
              app: "waba",
              operation: "whatsapp.reply.stage",
              args: {},
              revision: 1.5,
            });
          case 4:
            return toolCallResponse("call-extra", "demo_call", {
              app: "waba",
              operation: "whatsapp.reply.stage",
              args: {},
              unexpected: "SENTINEL_EXTRA",
            });
          case 5:
            return toolCallResponse("call-valid", "demo_call", {
              app: "waba",
              operation: "whatsapp.reply.stage",
              args: { provider: "linked" },
              revision: 2,
            });
          default:
            return answer("Recovered.");
        }
      },
      {
        tools: [
          {
            name: "test",
            tools: [
              {
                name: "demo_call",
                description: "Synthetic tool for schema validation tests.",
                parameters: {
                  type: "object",
                  properties: {
                    app: { type: "string", minLength: 1 },
                    operation: { type: "string", minLength: 1 },
                    args: { type: "object", additionalProperties: true },
                    revision: { type: "integer" },
                  },
                  required: ["app", "operation", "args"],
                  additionalProperties: false,
                },
                replay: "safe",
                async execute(input: unknown) {
                  calls.push(input);
                  return { output: [{ type: "text", text: "ok" }] };
                },
              },
            ],
          },
        ],
      },
    );

    await (await run.submit("test schema errors")).wait(BACKGROUND_CONTEXT);

    expect(calls).toEqual([
      {
        app: "waba",
        operation: "whatsapp.reply.stage",
        args: { provider: "linked" },
        revision: 2,
      },
    ]);

    const finalResults = seenToolResults.slice(-5);
    expect(finalResults).toHaveLength(5);
    expect(finalResults[0]).toContain('Tool "demo_call" arguments do not match the declared schema:');
    expect(finalResults[0]).toContain("args: missing");

    expect(finalResults[1]).toContain("args: expected object, got string");
    expect(finalResults[1]).not.toContain("SENTINEL_VALUE");

    expect(finalResults[2]).toContain("revision: expected integer, got number");

    expect(finalResults[3]).toContain("unexpected: unknown field");
    expect(finalResults[3]).not.toContain("SENTINEL_EXTRA");

    expect(finalResults[4]).toBe("ok");
  });
});
