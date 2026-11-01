import { describe, expect, it } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { createModels } from "@earendil-works/pi-ai/models";
import { createRegistry, Harness, MemoryStorage } from "@earendil-works/pi-durable";
import { createAI } from "agents/models/pi-ai";
import { runtimeExtension } from "../src/engine.ts";
import { defaultModel, meteredBinding, withDefaultModel } from "../src/model.ts";
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
  captures = 1,
  maxImages = 1,
) {
  const requests: Record<string, unknown>[] = [];
  const binding = meteredBinding(
    {
      async requestModel(input) {
        requests.push(input);
        const response =
          requests.length <= captures
            ? frame({ role: "assistant", content: "" }) +
              frame({
                tool_calls: [
                  {
                    index: 0,
                    id: `capture-${requests.length}`,
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
    undefined,
    maxImages,
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
  const harness = await Harness.open(new MemoryStorage(), { models, registry }, BACKGROUND_CONTEXT);
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
      messages: records.items.flatMap((record) => record.model ?? []),
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

  it("keeps the latest screenshots within the host's model-specific image limit", async () => {
    const images = ["older", "middle", "newest"].map((value) =>
      Buffer.from(value).toString("base64"),
    );
    const messages = images.map((data) => ({
      role: "user",
      content: [{ type: "image_url", image_url: { url: `data:image/png;base64,${data}` } }],
    }));
    let forwarded: Record<string, unknown> | undefined;
    const binding = meteredBinding(
      {
        async requestModel(input) {
          forwarded = input;
          return Response.json({ ok: true });
        },
      },
      () => ({ conversation: "1", task: "2", cutoff: "3" }),
      undefined,
      2,
    );
    await binding.run("default", { messages });
    const sent = JSON.stringify(forwarded);
    expect(sent).not.toContain(images[0]);
    expect(sent).toContain(images[1]);
    expect(sent).toContain(images[2]);
    expect(sent).toMatch(/image.{0,100}omitted/i);
    expect(messages[0]?.content[0]?.image_url.url).toContain(images[0]);
  });

  it("defaults to the latest screenshot when the model limit is unknown", async () => {
    const { requests, messages: persisted } = await observe(true, PNG, false, 3);
    const messages = requests.at(-1)?.messages as Array<{ content: unknown }>;
    const parts = messages.flatMap((message) =>
      Array.isArray(message.content) ? message.content : [],
    );
    expect(parts.filter((part) => part.type === "image_url")).toHaveLength(1);
    expect(parts.filter((part) => part.type === "text" && /image.{0,100}omitted/i.test(part.text)))
      .toHaveLength(2);
    expect(JSON.stringify(persisted).split(PNG)).toHaveLength(4);
  });

  it("refuses a host image limit outside the gateway's supported range", () => {
    const transport = { requestModel: async () => Response.json({ ok: true }) };
    const identity = () => ({ conversation: "1", task: "2", cutoff: "3" });
    for (const limit of [0, 5, 1.5, Number.NaN]) {
      expect(() => meteredBinding(transport, identity, undefined, limit)).toThrow(
        "Invalid model image limit.",
      );
    }
  });

  it("keeps the newest observations within the whole-request image budget", async () => {
    const { requests, messages: persisted } = await observe(true, PNG, false, 6, 4);
    const finalRequest = requests.at(-1);
    const messages = finalRequest?.messages as Array<{ role: string; content: unknown }>;
    const parts = messages.flatMap((message) =>
      Array.isArray(message.content) ? message.content : [],
    );
    const images = parts
      .filter(
        (part) =>
          part.type === "image_url" ||
          (part.type === "text" && /image.{0,100}omitted/i.test(part.text)),
      )
      .map((part) => (part.type === "image_url" ? "image" : "omitted"));
    expect(images).toEqual(["omitted", "omitted", "image", "image", "image", "image"]);
    expect(JSON.stringify(persisted).split(PNG)).toHaveLength(7);
  });

  it("bounds combined screenshot bytes even when the image count fits", async () => {
    const { requests } = await observe(true, "A".repeat(100 * 1024), false, 2, 4);
    const messages = requests.at(-1)?.messages as Array<{ content: unknown }>;
    const parts = messages.flatMap((message) =>
      Array.isArray(message.content) ? message.content : [],
    );
    expect(parts.filter((part) => part.type === "image_url")).toHaveLength(1);
    expect(JSON.stringify(parts)).toMatch(/image.{0,100}omitted/i);
  });

  it("uses the remaining image budget for an older screenshot that fits", async () => {
    const older = "A".repeat(50 * 1024);
    const middle = "B".repeat(110 * 1024);
    const newest = "C".repeat(100 * 1024);
    const messages = [older, middle, newest].map((data) => ({
      role: "user",
      content: [{ type: "image_url", image_url: { url: `data:image/png;base64,${data}` } }],
    }));
    let forwarded: Record<string, unknown> | undefined;
    const binding = meteredBinding(
      {
        async requestModel(input) {
          forwarded = input;
          return Response.json({ ok: true });
        },
      },
      () => ({ conversation: "1", task: "2", cutoff: "3" }),
      undefined,
      4,
    );
    await binding.run("default", { messages });
    const sent = JSON.stringify(forwarded);
    expect(sent.includes(older)).toBe(true);
    expect(sent.includes(middle)).toBe(false);
    expect(sent.includes(newest)).toBe(true);
    expect(sent).toMatch(/image.{0,100}omitted/i);
    expect(messages[1]?.content[0]?.image_url.url.endsWith(middle)).toBe(true);
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

  it("does not silently drop a stored screenshot when a resumed session becomes text-only", async () => {
    const { messages } = await observe(true);
    const requests: Record<string, unknown>[] = [];
    const binding = meteredBinding(
      {
        async requestModel(input) {
          requests.push(input);
          return new Response(
            frame({ role: "assistant", content: "" }) +
              frame({ content: "finished" }) +
              frame({}, "stop") +
              "data: [DONE]\n\n",
            { headers: { "content-type": "text/event-stream" } },
          );
        },
      },
      () => ({ conversation: "1", task: "4", cutoff: "5" }),
    );
    const model = defaultModel("default");
    const ai = createAI({ binding: binding as Ai });
    const models = createModels();
    models.setProvider(withDefaultModel(ai.provider, model));
    const stream = models.streamSimple(
      model,
      {
        messages: [
          ...messages,
          { role: "user", content: "What did the page show?", timestamp: 1000 },
        ],
      },
      {},
    );
    for await (const _event of stream) {
      // Drain the real adapter to issue the request.
    }
    expect((await stream.result()).stopReason).toBe("stop");
    expect(JSON.stringify(requests[0])).not.toContain(PNG);
    const replayed = (requests[0]?.messages as Array<{ role: string; content: string }>).find(
      (message) => message.role === "tool",
    );
    expect(replayed?.content).toMatch(/image.{0,100}(?:omitt|unavailable|not support)/i);
    expect(JSON.stringify(messages)).toContain(PNG);
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
