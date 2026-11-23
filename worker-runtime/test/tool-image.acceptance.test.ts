import { describe, expect, it } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { createModels } from "@earendil-works/pi-ai/models";
import {
  createRegistry,
  type Extension,
  Harness,
  MemoryStorage,
} from "@earendil-works/pi-durable";
import { createAI } from "agents/models/pi-ai";
import type { AppScope, RuntimeTool, ToolContent } from "../src/contract.ts";
import { runtimeExtension } from "../src/engine.ts";
import { defaultModel, meteredBinding, withDefaultModel } from "../src/model.ts";
import { rawSessionFrame } from "../src/session-stream.ts";
import type { ToolAuthority } from "../src/tool-policy.ts";
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
      undefined,
      undefined,
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

  /** A model that captures once, then refuses any request carrying image bytes. */
  async function refusingModel() {
    const requests: string[] = [];
    let calls = 0;
    const binding = meteredBinding(
      {
        async requestModel(input) {
          calls += 1;
          const sent = JSON.stringify(input);
          requests.push(sent);
          if (sent.includes("image_url"))
            return Response.json(
              {
                error: {
                  message: "Image input is unavailable for the selected model.",
                  type: "invalid_request_error",
                  code: "image_input_unavailable",
                },
              },
              { status: 404 },
            );
          const body =
            calls === 1
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
                frame({ content: "answered in text" }) +
                frame({}, "stop");
          return new Response(body + "data: [DONE]\n\n", {
            headers: { "content-type": "text/event-stream" },
          });
        },
      },
      () => ({ conversation: "1", task: "2", cutoff: "3" }),
    );
    const model = defaultModel("default", true);
    const models = createModels();
    models.setProvider(withDefaultModel(createAI({ binding: binding as Ai }).provider, model));
    const registry = createRegistry();
    registry.install(
      runtimeExtension(
        { name: "synthetic-browser", tools: [capturing(() => {})] },
        undefined,
        undefined,
        true,
      ),
    );
    const harness = await Harness.open(new MemoryStorage(), { models, registry }, BACKGROUND_CONTEXT);
    const conversation = await harness.root(BACKGROUND_CONTEXT, {
      agent: { model: { provider: model.provider, modelId: model.id } },
    });
    const turn = async (content: string) => {
      const operation = await conversation.submit({ type: "input", content }, BACKGROUND_CONTEXT);
      await operation.wait(BACKGROUND_CONTEXT);
      const records = await conversation.entries(
        { order: "ascending" },
        100,
        undefined,
        BACKGROUND_CONTEXT,
      );
      return { records: records.items, thread: JSON.stringify(threadEntries(records.items, true)) };
    };
    return { requests, turn, close: () => harness.close(BACKGROUND_CONTEXT) };
  }

  it("names a gateway image refusal instead of reporting a network outage", async () => {
    const model = await refusingModel();
    try {
      const { thread } = await model.turn("Inspect this page.");
      expect(thread).toContain("The selected model cannot read images.");
      expect(thread).not.toContain("could not be reached");
      expect(thread).not.toContain("answered in text");
    } finally {
      await model.close();
    }
  });

  it("lets a later turn answer after a refusal, without the image and without losing it", async () => {
    const model = await refusingModel();
    try {
      await model.turn("Inspect this page.");
      const { thread, records } = await model.turn("Forget the image and say hello.");
      expect(thread).toContain("answered in text");
      const last = model.requests.at(-1)!;
      expect(last).not.toContain(PNG);
      expect(last).toContain("the selected model cannot read images");
      // The model input changed; the stored conversation did not.
      expect(JSON.stringify(records)).toContain(PNG);
    } finally {
      await model.close();
    }
  });

  it("does not retry a refusal when the image is not in older history", async () => {
    const seen: string[] = [];
    const binding = meteredBinding(
      {
        async requestModel(input) {
          seen.push(JSON.stringify(input));
          return Response.json(
            { error: { message: "private", code: "image_input_unavailable" } },
            { status: 404 },
          );
        },
      },
      () => ({ conversation: "1", task: "2", cutoff: "3" }),
    );
    const response = (await binding.run("default", {
      messages: [
        { role: "user", content: [{ type: "image_url", image_url: { url: `data:image/png;base64,${PNG}` } }] },
      ],
    })) as Response;
    expect(seen).toHaveLength(1);
    expect(await response.json()).toEqual({
      error: { code: "image_input_unavailable", message: "image_input_unavailable" },
    });
  });

  it("does not rewrite a gateway refusal outside the allowlist", async () => {
    const binding = meteredBinding(
      {
        async requestModel() {
          return Response.json(
            { error: { message: "private upstream detail", code: "other" } },
            { status: 404 },
          );
        },
      },
      () => ({ conversation: "1", task: "2", cutoff: "3" }),
    );
    const response = (await binding.run("default", {
      messages: [],
    })) as Response;
    expect(await response.json()).toEqual({
      error: { message: "private upstream detail", code: "other" },
    });
  });
});

function capturing(onRun: (app: unknown) => void): RuntimeTool {
  return {
    name: "capture",
    description: "Capture the synthetic page.",
    parameters: {
      type: "object",
      properties: {},
      additionalProperties: false,
    },
    replay: "safe",
    async execute(_input, invocation) {
      onRun(invocation.app);
      return {
        output: [
          { type: "text", text: "Synthetic page capture." },
          { type: "image", data: PNG, mimeType: "image/png" },
        ],
      };
    },
  };
}

describe("a part that is neither text nor an image", () => {
  const output = (accepts: boolean, part: unknown) =>
    (runtimeExtension(
      {
        name: "synthetic",
        tools: [
          {
            name: "relay",
            description: "Relay a synthetic part.",
            parameters: { type: "object", properties: {}, additionalProperties: false },
            replay: "safe",
            execute: async () => ({ output: [part as ToolContent] }),
          },
        ],
      },
      undefined,
      undefined,
      accepts,
    ).tools![0]!.execute as (...args: unknown[]) => Promise<unknown>)(
      {},
      { conversationId: 1, taskId: 2, now: () => 1000 },
      { abortSignal: new AbortController().signal },
    );

  it("is refused with or without the host's opt-in", async () => {
    for (const accepts of [false, true])
      for (const part of [
        { type: "file", data: "UNVALIDATED" },
        { type: "text", text: 42, data: "UNVALIDATED" },
        { data: "UNVALIDATED" },
      ]) {
        const result = await output(accepts, part);
        expect(result).toMatchObject({ isError: true });
        expect(JSON.stringify(result)).not.toContain("UNVALIDATED");
      }
  });

  it("redacts image bytes from raw frames even with no media type", () => {
    const frame = rawSessionFrame({ part: { type: "image", data: "UNVALIDATED" } });
    expect(frame).not.toContain("UNVALIDATED");
    expect(frame).toContain('"redacted":true');
  });
});

describe("image results keep the tool authority and app-scope order", () => {
  const app = { appId: "synthetic-app" } as unknown as AppScope;
  const api = { conversationId: 1, taskId: 2, now: () => 1000 };
  const call = (tool: NonNullable<Extension["tools"]>[number]) =>
    (tool.execute as (...args: unknown[]) => Promise<unknown>)({}, api, {
      abortSignal: new AbortController().signal,
    });
  const allow: ToolAuthority = {
    toolRefusal: () => undefined,
    authorizeTool: async () => {},
  };
  const order = (
    authority: ToolAuthority,
    resolve: () => Promise<AppScope | null>,
    acceptImages = true,
  ) => {
    const handled: unknown[] = [];
    const tool = runtimeExtension(
      { name: "synthetic-browser", tools: [capturing((a) => handled.push(a))] },
      authority,
      resolve,
      acceptImages,
    ).tools![0]!;
    return { handled, run: () => call(tool) };
  };

  it("hands an authorized app-scoped call its scope and returns its image", async () => {
    const { handled, run } = order(allow, async () => app);
    const result = await run();
    expect(handled).toEqual([app]);
    expect(JSON.stringify(result)).toContain(PNG);
  });

  it("does not resolve scope or reach the handler for a refused or revoked call", async () => {
    for (const authority of [
      { ...allow, toolRefusal: () => "tool_effect_refused" },
      {
        ...allow,
        authorizeTool: async () => {
          throw new Error("session_revoked");
        },
      },
    ] satisfies ToolAuthority[]) {
      let resolved = 0;
      const { handled, run } = order(authority, async () => {
        resolved += 1;
        return app;
      });
      const outcome = await run().then(
        (value) => JSON.stringify(value),
        (error: Error) => error.message,
      );
      expect(outcome).toMatch(/tool_effect_refused|session_revoked/);
      expect(outcome).not.toContain(PNG);
      expect(resolved).toBe(0);
      expect(handled).toHaveLength(0);
    }
  });

  it("refuses an unattributable call rather than running it as Home", async () => {
    const { handled, run } = order(allow, async () => {
      throw new Error("operation envelope unavailable");
    });
    await expect(run()).rejects.toThrow("operation envelope unavailable");
    expect(handled).toHaveLength(0);
  });

  it("refuses an image from a scoped call when the host has not opted in", async () => {
    const { handled, run } = order(allow, async () => app, false);
    const result = JSON.stringify(await run());
    expect(handled).toEqual([app]);
    expect(result).toContain("Image input is unavailable for this session.");
    expect(result).not.toContain(PNG);
  });
});
