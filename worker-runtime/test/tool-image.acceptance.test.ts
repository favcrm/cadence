import { describe, expect, it } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { createModels } from "@earendil-works/pi-ai/models";
import {
  createRegistry,
  Harness,
  MemoryStorage,
} from "@earendil-works/pi-durable";
import { createAI } from "agents/models/pi-ai";
import { runtimeExtension } from "../src/engine.ts";
import {
  defaultModel,
  meteredBinding,
  withDefaultModel,
} from "../src/model.ts";
import { rawSessionFrame } from "../src/session-stream.ts";
import { threadEntries } from "../src/transcript.ts";

// A synthetic one-pixel PNG; no real browsing or provider call is involved.
const PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
const frame = (delta: Record<string, unknown>, finish: string | null = null) =>
  `data: ${JSON.stringify({
    id: "synthetic-response",
    object: "chat.completion.chunk",
    created: 1,
    model: "synthetic-model",
    choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`;

async function observe(
  acceptImages: boolean,
  image: string = PNG,
  legacy = false,
) {
  const requests: Record<string, unknown>[] = [];
  const binding = meteredBinding(
    {
      async requestModel(input) {
        requests.push(input);
        const response =
          requests.length === 1
            ? frame({ role: "assistant", content: "" }) +
              frame({
                tool_calls: [
                  {
                    index: 0,
                    id: "capture-1",
                    type: "function",
                    function: { name: "capture", arguments: "{}" },
                  },
                ],
              }) +
              frame({}, "tool_calls")
            : frame({ role: "assistant", content: "" }) +
              frame({ content: "finished" }) +
              frame({}, "stop");
        return new Response(response + "data: [DONE]\n\n", {
          headers: { "content-type": "text/event-stream" },
        });
      },
    },
    () => ({ conversation: "1", task: "2", cutoff: "3" }),
  );
  const model = defaultModel("default", acceptImages);
  const ai = createAI({ binding: binding as Ai });
  const models = createModels();
  models.setProvider(withDefaultModel(ai.provider, model));
  const registry = createRegistry();
  registry.install(
    runtimeExtension(
      {
        name: "synthetic-browser",
        tools: [
          {
            name: "capture",
            description: "Capture the synthetic page.",
            parameters: {
              type: "object",
              properties: {},
              additionalProperties: false,
            },
            replay: "safe",
            async execute() {
              return {
                output: [
                  { type: "text", text: "Synthetic page capture." },
                  { type: "image", data: image, mimeType: "image/png" },
                ],
              };
            },
          },
        ],
      },
      acceptImages,
    ),
  );
  const harness = await Harness.open(
    new MemoryStorage(),
    { models, registry },
    BACKGROUND_CONTEXT,
  );
  try {
    const conversation = await harness.root(BACKGROUND_CONTEXT, {
      agent: {
        model: {
          provider: model.provider,
          modelId: legacy ? "z-ai/glm-5.3-flash" : model.id,
        },
      },
    });
    const operation = await conversation.submit(
      { type: "input", content: "Inspect this page." },
      BACKGROUND_CONTEXT,
    );
    const outcome = await operation.wait(BACKGROUND_CONTEXT);
    const records = await conversation.entries(
      { order: "ascending" },
      100,
      undefined,
      BACKGROUND_CONTEXT,
    );
    expect(outcome.status).toBe("done");
    return {
      requests,
      thread: threadEntries(records.items, true),
      rawSnapshot: rawSessionFrame({ type: "snapshot", snapshot: { entries: records.items } }),
      rawEvents: records.items.map((entry) => rawSessionFrame({ type: "message_end", entry })),
    };
  } finally {
    await harness.close(BACKGROUND_CONTEXT);
  }
}

describe("visual tool results through the hosted model path", () => {
  it("delivers image bytes to a vision-capable host without exposing them in the board thread", async () => {
    const { requests, thread, rawSnapshot, rawEvents } = await observe(true);
    expect(requests).toHaveLength(2);
    expect(requests[1]).toMatchObject({
      messages: expect.arrayContaining([
        expect.objectContaining({
          role: "user",
          content: expect.arrayContaining([
            { type: "image_url", image_url: { url: `data:image/png;base64,${PNG}` } },
          ]),
        }),
      ]),
    });
    expect(JSON.stringify(thread)).toContain("Synthetic page capture.");
    expect(JSON.stringify(thread)).not.toContain(PNG);
    for (const frame of [rawSnapshot, ...rawEvents]) {
      expect(frame).not.toContain(PNG);
    }
    expect(rawSnapshot).toContain('"redacted":true');
  });

  it("reports unsupported image input instead of silently sending text that claims an image was attached", async () => {
    const { requests, thread } = await observe(false);
    expect(requests).toHaveLength(2);
    expect(JSON.stringify(requests[1])).not.toContain(PNG);
    expect(thread.find((entry) => entry.kind === "tool_result")).toMatchObject({
      text: "Image input is unavailable for this session.",
      payload: { is_error: true },
    });
  });

  it("retains image input when resuming a session with a legacy model id", async () => {
    const { requests } = await observe(true, PNG, true);
    expect(requests).toHaveLength(2);
    expect(JSON.stringify(requests[1])).toContain(`data:image/png;base64,${PNG}`);
  });

  it("refuses malformed and oversized image data before it reaches the model", async () => {
    for (const invalid of ["bad=raw", "A".repeat(192 * 1024 + 4)]) {
      const { requests, thread } = await observe(true, invalid);
      expect(requests).toHaveLength(2);
      expect(JSON.stringify(requests[1])).not.toContain(invalid);
      expect(thread.find((entry) => entry.kind === "tool_result")).toMatchObject({
        text: "Image output is invalid or too large.",
        payload: { is_error: true },
      });
    }
  });
});
