import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { createModels } from "@earendil-works/pi-ai/models";
import { createRegistry, Harness, MemoryStorage } from "@earendil-works/pi-durable";
import { createAI } from "agents/models/pi-ai";
import { afterEach, describe, expect, it, mock } from "bun:test";
import { BOARD_MESSAGE_KIND } from "../src/contract.ts";
import { defaultModel, meteredBinding, withDefaultModel } from "../src/model.ts";

mock.module("cloudflare:workers", () => ({
  DurableObject: class {
    constructor(
      readonly ctx: unknown,
      readonly env: unknown,
    ) {}
  },
}));

/**
 * CAD-2853: verify that hosted runtime configuration uses 40ms partial progress
 * cadence and flushes the leading visible text delta immediately instead of
 * waiting a trailing timer (so even brief sub-40ms replies emit a live preview).
 */
describe("streaming preview cadence and leading-edge flush", () => {
  const cleanups: Array<() => Promise<void>> = [];

  afterEach(async () => {
    while (cleanups.length > 0) {
      await cleanups.pop()!();
    }
  });

  it("configures HostedSession with persistWithinMs: 0 and partialIntervalMs: 40", async () => {
    const { HOSTED_PROGRESS_POLICY } = await import("../src/hosted-session.ts");
    expect(HOSTED_PROGRESS_POLICY).toEqual({
      persistWithinMs: 0,
      partialIntervalMs: 40,
    });
  });

  it("flushes the leading text delta immediately before 40ms and coalesces subsequent deltas at 40ms cadence", async () => {
    const { HOSTED_PROGRESS_POLICY } = await import("../src/hosted-session.ts");
    const seenPreviews: Array<{ t: number; text: string }> = [];
    const encoder = new TextEncoder();

    const makeStream = (emitChunks: (enqueue: (s: string) => void) => Promise<void>) =>
      new Response(
        new ReadableStream<Uint8Array>({
          async start(controller) {
            const enqueue = (s: string) => controller.enqueue(encoder.encode(s));
            await emitChunks(enqueue);
            controller.close();
          },
        }),
        { headers: { "content-type": "text/event-stream" } },
      );

    let mode: "short" | "chunked" = "short";
    const binding = meteredBinding(
      {
        requestModel: async () => {
          if (mode === "short") {
            return makeStream(async (enqueue) => {
              enqueue(
                `data: ${JSON.stringify({ id: "r1", object: "chat.completion.chunk", created: 1, model: "synthetic", choices: [{ index: 0, delta: { role: "assistant", content: "" }, finish_reason: null }] })}\n\n`,
              );
              enqueue(
                `data: ${JSON.stringify({ id: "r1", object: "chat.completion.chunk", created: 1, model: "synthetic", choices: [{ index: 0, delta: { content: "Hey" }, finish_reason: null }] })}\n\n`,
              );
              // Finish in 15ms (well below both 40ms and the former 100ms trailing timer):
              // only a leading-edge flush can publish `live.generation` before settlement.
              await Bun.sleep(15);
              enqueue(
                `data: ${JSON.stringify({ id: "r1", object: "chat.completion.chunk", created: 1, model: "synthetic", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\n`,
              );
              enqueue("data: [DONE]\n\n");
            });
          }
          return makeStream(async (enqueue) => {
            enqueue(
              `data: ${JSON.stringify({ id: "r2", object: "chat.completion.chunk", created: 1, model: "synthetic", choices: [{ index: 0, delta: { role: "assistant", content: "" }, finish_reason: null }] })}\n\n`,
            );
            enqueue(
              `data: ${JSON.stringify({ id: "r2", object: "chat.completion.chunk", created: 1, model: "synthetic", choices: [{ index: 0, delta: { content: "First" }, finish_reason: null }] })}\n\n`,
            );
            await Bun.sleep(5);
            enqueue(
              `data: ${JSON.stringify({ id: "r2", object: "chat.completion.chunk", created: 1, model: "synthetic", choices: [{ index: 0, delta: { content: " second" }, finish_reason: null }] })}\n\n`,
            );
            await Bun.sleep(60);
            enqueue(
              `data: ${JSON.stringify({ id: "r2", object: "chat.completion.chunk", created: 1, model: "synthetic", choices: [{ index: 0, delta: { content: " third" }, finish_reason: null }] })}\n\n`,
            );
            await Bun.sleep(60);
            enqueue(
              `data: ${JSON.stringify({ id: "r2", object: "chat.completion.chunk", created: 1, model: "synthetic", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\n`,
            );
            enqueue("data: [DONE]\n\n");
          });
        },
      },
      () => ({ conversation: "1", task: "2", cutoff: "3" }),
    );

    const ai = createAI({ binding: binding as Ai });
    const model = defaultModel("default");
    const models = createModels();
    models.setProvider(withDefaultModel(ai.provider, model));
    const harness40 = await Harness.open(
      new MemoryStorage(),
      {
        models,
        registry: createRegistry(),
        settings: {
          retry: { enabled: false },
          progress: HOSTED_PROGRESS_POLICY,
        },
      },
      BACKGROUND_CONTEXT,
    );
    cleanups.push(async () => harness40.close(BACKGROUND_CONTEXT));

    const conv = await harness40.root(BACKGROUND_CONTEXT, {
      agent: { model: { provider: model.provider, modelId: model.id } },
    });

    const watch = await conv.watch(BACKGROUND_CONTEXT);
    const t0 = Date.now();
    watch.start((call) => {
      if (call.kind !== "value") return;
      const content = call.value.docs["pi.live"]?.generation?.message?.content;
      if (!Array.isArray(content)) return;
      const text = content
        .filter((p): p is { type: "text"; text: string } => p.type === "text")
        .map((p) => p.text)
        .join("");
      if (text.length > 0 && seenPreviews.at(-1)?.text !== text) {
        seenPreviews.push({ t: Date.now() - t0, text });
      }
    });

    try {
      // 1. Sub-40ms 1-chunk reply ("Hey") still publishes a live preview immediately.
      mode = "short";
      const op1 = await conv.submit(
        {
          type: "input",
          requestId: "op-short",
          content: "Hey",
          entry: {
            kind: BOARD_MESSAGE_KIND,
            data: { messageId: "op-short", transcriptRole: "operator" },
          },
        },
        BACKGROUND_CONTEXT,
      );
      const res1 = await op1.wait(BACKGROUND_CONTEXT);
      expect(res1.status).toBe("done");
      expect(seenPreviews.map((p) => p.text)).toContain("Hey");

      // 2. Multi-chunk reply: leading "First" flushes immediately, +5ms " second" coalesces at ~40ms, +60ms " third" flushes at ~40ms cadence.
      seenPreviews.length = 0;
      mode = "chunked";
      const op2 = await conv.submit(
        {
          type: "input",
          requestId: "op-chunked",
          content: "Stream",
          entry: {
            kind: BOARD_MESSAGE_KIND,
            data: { messageId: "op-chunked", transcriptRole: "operator" },
          },
        },
        BACKGROUND_CONTEXT,
      );
      const res2 = await op2.wait(BACKGROUND_CONTEXT);
      expect(res2.status).toBe("done");
      expect(seenPreviews[0]?.text).toBe("First");
      expect(seenPreviews.some((p) => p.text === "First second")).toBe(true);
      expect(seenPreviews.some((p) => p.text === "First second third")).toBe(true);
    } finally {
      await watch.stop();
    }
  });
});
