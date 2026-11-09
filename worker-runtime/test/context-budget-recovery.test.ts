import { Database } from "bun:sqlite";
import { afterEach, describe, expect, it, mock } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { SqliteDatabase, SqliteExecutor, SqliteValue } from "@earendil-works/chord/sqlite";
import { Harness, watchEvents } from "@earendil-works/pi-durable";
import { SqliteStorage } from "@earendil-works/pi-durable/storage/sqlite";
import { BOARD_MESSAGE_KIND, type GenerationIdentity, type ThreadEntry } from "../src/contract.ts";
import { CONTEXT_BUDGET_EXCEEDED_TEXT, defaultModel } from "../src/model.ts";
import type { ModelTaskContext } from "../src/model-task-context.ts";

// The Workers module is absent in Bun; install its lifecycle stub before
// loading the real session. The engine and SQLite are not stubbed.
mock.module("cloudflare:workers", () => ({
  DurableObject: class {
    constructor(readonly ctx: unknown, readonly env: unknown) {}
  },
}));

function sqliteFacade(database: Database): SqliteDatabase {
  const executor: SqliteExecutor = {
    async exec(sql) { database.exec(sql); },
    async run(sql, ...params) { database.prepare(sql).run(...(params as never[])); },
    async get<T extends object>(sql: string, ...params: SqliteValue[]) {
      return (database.prepare(sql).get(...(params as never[])) as T | null | undefined) ?? undefined;
    },
    async all<T extends object>(sql: string, ...params: SqliteValue[]) {
      return database.prepare(sql).all(...(params as never[])) as T[];
    },
  };
  return {
    ...executor,
    async transaction<T>(callback: (transaction: SqliteExecutor) => Promise<T>) {
      database.exec("BEGIN");
      try {
        const result = await callback(executor);
        database.exec("COMMIT");
        return result;
      } catch (error) {
        database.exec("ROLLBACK");
        throw error;
      }
    },
    async close() {},
  };
}

const chunk = (delta: Record<string, unknown>, finish: string | null = null) =>
  `data: ${JSON.stringify({
    id: "synthetic-response", object: "chat.completion.chunk", created: 1,
    model: "synthetic-model", choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`;
function answer(text: string) {
  return new Response(
    chunk({ role: "assistant", content: "" }) + chunk({ content: text }) +
      chunk({}, "stop") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );
}
function toolCall() {
  return new Response(
    chunk({ role: "assistant", content: "" }) +
      chunk({ tool_calls: [{ index: 0, id: "read-1", type: "function",
        function: { name: "read_artifact", arguments: "{}" } }] }) +
      chunk({}, "tool_calls") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );
}
function refused() {
  return Response.json({ error: {
    type: "invalid_request_error", code: "context_budget_exceeded",
    message: "Private gateway details at https://private.invalid",
  } }, { status: 400 });
}

type RequestRecord = { input: Record<string, unknown>; identity: GenerationIdentity; bytes: number };
const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

async function session(
  respond: (requests: readonly RequestRecord[]) => Response | Promise<Response>,
  options: { keepRecentTokens?: number; artifact?: string; reserveTokens?: number } = {},
) {
  const { HostedSession } = await import("../src/hosted-session.ts");
  const database = new Database(":memory:");
  const sql = {
    exec(query: string, ...bindings: (string | number | null)[]) {
      if (/^\s*(SELECT|PRAGMA|WITH)/i.test(query)) {
        const rows = database.prepare(query).all(...bindings);
        return { toArray: () => rows };
      }
      database.prepare(query).run(...bindings);
      return { toArray: () => [] };
    },
  };
  const requests: RequestRecord[] = [];
  let toolExecutions = 0;
  const hosted = new HostedSession(
    { id: { name: "conversation" }, storage: { sql, get: async () => undefined } } as never,
    {} as never,
    {
      modelAlias: "default",
      async requestModel(input: Record<string, unknown>, identity: GenerationIdentity) {
        requests.push({ input, identity: { ...identity }, bytes: Buffer.byteLength(JSON.stringify(input)) });
        const response = await respond(requests);
        const messages = input.messages as Array<{ content?: unknown }>;
        if (messages.some((message) => {
          if (typeof message.content === "string") return message.content.length > 64_000;
          if (!Array.isArray(message.content)) return false;
          return message.content.reduce((total: number, part: { type?: string; text?: string }) =>
            total + (part.type === "text" && typeof part.text === "string" ? part.text.length : 0), 0,
          ) > 64_000;
        })) return Response.json({ error: {
          type: "invalid_request_error", code: "invalid_request", message: "Message too large.",
        } }, { status: 400 });
        return response;
      },
      tools: options.artifact === undefined ? [] : [{
        name: "artifacts", tools: [{
          name: "read_artifact", description: "Read the HTML artifact.",
          parameters: { type: "object", properties: {}, additionalProperties: false },
          replay: "safe",
          async execute() {
            toolExecutions++;
            return { output: [{ type: "text", text: options.artifact! }] };
          },
        }],
      }],
      authorizeTool: async () => {},
      toolRefusal: () => undefined,
    } as never,
  );
  const wired = hosted as unknown as {
    models: never; registry: never; modelTaskContext: ModelTaskContext; harness: unknown;
  };
  const harness = await Harness.open(
    await SqliteStorage.open(sqliteFacade(database), { prefix: "pi_" }),
    {
      models: wired.models, registry: wired.registry,
      telemetry: wired.modelTaskContext.telemetry,
      settings: () => ({
        retry: { enabled: false },
        compaction: { backgroundTokens: 0, keepRecentTokens: options.keepRecentTokens ?? 20_000, ...(options.reserveTokens === undefined ? {} : { reserveTokens: options.reserveTokens }) },
      }),
    },
    BACKGROUND_CONTEXT,
  );
  cleanups.push(async () => { await harness.close(BACKGROUND_CONTEXT); database.close(); });
  const conversation = await harness.root(BACKGROUND_CONTEXT, {
    agent: { model: { provider: defaultModel("default").provider, modelId: "default" } },
  });
  wired.harness = {
    session: () => ({ id: conversation.id, events: () => watchEvents(harness, conversation.id, BACKGROUND_CONTEXT) }),
    conversation: async () => conversation,
    pending: async () => [],
  };
  let operations = 0;
  return {
    hosted, requests, toolExecutions: () => toolExecutions,
    async submit(content: string) {
      const id = `operation-${++operations}`;
      return conversation.submit({
        type: "input", requestId: id, content,
        entry: { kind: BOARD_MESSAGE_KIND, data: { messageId: id } },
      }, BACKGROUND_CONTEXT);
    },
  };
}

const LIMIT = 256 * 1024;
const failed = (thread: readonly ThreadEntry[]) =>
  thread.filter((entry) => entry.payload?.status === "failed");

describe("gateway byte-budget recovery", () => {
  it("answers a small turn without compaction", async () => {
    const run = await session(() => answer("Small answer."));
    await (await run.submit("Small question.")).wait(BACKGROUND_CONTEXT);
    expect(run.requests).toHaveLength(1);
    expect((await run.hosted.transcript(false)).at(-1)?.text).toBe("Small answer.");
  });

  it("compacts an older turn once for ASCII, multibyte, and escaped requests", async () => {
    for (const oldText of ["a".repeat(60_000), "中".repeat(20_000), '"'.repeat(30_000)]) {
      const entered = Promise.withResolvers<void>();
      const release = Promise.withResolvers<void>();
      const run = await session(async (requests) => {
        const request = requests.at(-1)!;
        if (request.bytes > LIMIT) return refused();
        if (requests.length === 6) {
          entered.resolve();
          await release.promise;
          return answer("Summary of the earlier turn.");
        }
        return answer(requests.length <= 4 ? "Earlier answer." : "Recovered answer.");
      });
      cleanups.push(async () => release.resolve());
      for (let index = 0; index < 4; index++)
        await (await run.submit(oldText)).wait(BACKGROUND_CONTEXT);
      const current = "current ".repeat(5_000);
      const operation = await run.submit(current);
      await entered.promise;
      expect(failed(await run.hosted.transcript(false))).toHaveLength(0);
      release.resolve();
      await operation.wait(BACKGROUND_CONTEXT);
      const thread = await run.hosted.transcript(false);
      expect(failed(thread)).toHaveLength(0);
      expect(thread.at(-1)?.text).toBe("Recovered answer.");
      expect(run.requests).toHaveLength(7);
      expect(run.requests[3]!.bytes).toBeLessThanOrEqual(LIMIT);
      expect(run.requests[4]!.bytes).toBeGreaterThan(LIMIT);
      expect(run.requests[5]!.bytes).toBeLessThanOrEqual(LIMIT);
      expect(run.requests[6]!.bytes).toBeLessThanOrEqual(LIMIT);
      expect(JSON.stringify(run.requests[6]!.input)).toContain(current);
      expect(run.requests[5]!.identity.task).not.toBe(run.requests[4]!.identity.task);
    }
  }, 15_000);

  it("retains an active artifact page and does not rerun its read after recovery", async () => {
    const artifact = `<html>${"A".repeat(40_000)}</html>`;
    const run = await session((requests) => {
      const request = requests.at(-1)!;
      if (request.bytes > LIMIT) return refused();
      if (requests.length === 5) return toolCall();
      if (requests.length === 7) return answer("Summary of earlier work.");
      return answer(requests.length <= 4 ? "Earlier answer." : "Edited artifact.");
    }, { artifact });
    for (let index = 0; index < 4; index++)
      await (await run.submit("a".repeat(60_000))).wait(BACKGROUND_CONTEXT);
    await (await run.submit("b".repeat(1_000))).wait(BACKGROUND_CONTEXT);
    const thread = await run.hosted.transcript(false);
    expect(run.toolExecutions()).toBe(1);
    expect(run.requests).toHaveLength(8);
    expect(run.requests[5]!.bytes).toBeGreaterThan(LIMIT);
    expect(run.requests[7]!.bytes).toBeLessThanOrEqual(LIMIT);
    const messages = run.requests[7]!.input.messages as Array<{ role: string; content: unknown }>;
    expect(messages.find((message) => message.role === "tool")?.content).toBe(artifact);
    expect(failed(thread)).toHaveLength(0);
    expect(thread.at(-1)?.text).toBe("Edited artifact.");
  });

  it("fails visibly without replaying a tool or summarizing the active artifact", async () => {
    const artifact = `<html>${"A".repeat(40_000)}</html>`;
    const run = await session((requests) => {
      if (requests.length === 1) return answer("Earlier answer.");
      if (requests.length === 2) return toolCall();
      return refused();
    }, { keepRecentTokens: 1, artifact });
    await (await run.submit("Earlier work.")).wait(BACKGROUND_CONTEXT);
    await (await run.submit("Edit the artifact.")).wait(BACKGROUND_CONTEXT);
    const thread = await run.hosted.transcript(false);
    expect(run.toolExecutions()).toBe(1);
    expect(run.requests).toHaveLength(3);
    expect(JSON.stringify(run.requests[2]!.input)).toContain(artifact);
    expect(thread.find((entry) => entry.kind === "tool_result")?.text).toBe(artifact);
    expect(failed(thread).at(-1)?.payload?.error).toBe(CONTEXT_BUDGET_EXCEEDED_TEXT);
  });

  it("reports an uncompactable first turn without another model call", async () => {
    const run = await session(() => refused());
    await (await run.submit("A single turn.")).wait(BACKGROUND_CONTEXT);
    expect(run.requests).toHaveLength(1);
    expect(failed(await run.hosted.transcript(false)).at(-1)?.payload?.error)
      .toBe(CONTEXT_BUDGET_EXCEEDED_TEXT);
  });

  it("delivers the terminal budget failure on an already-open thread stream", async () => {
    const run = await session(() => refused());
    const response = await run.hosted.fetch(new Request("https://board.test/thread-stream"));
    expect(response.status).toBe(200);
    const reader = response.body!.getReader();
    cleanups.push(async () => { await reader.cancel(); });
    const received = (async () => {
      const decoder = new TextDecoder();
      let frames = "";
      while (!frames.includes(CONTEXT_BUDGET_EXCEEDED_TEXT)) {
        const chunk = await reader.read();
        if (chunk.done) throw new Error("Thread stream closed before the failure.");
        frames += decoder.decode(chunk.value, { stream: true });
      }
      return frames;
    })();
    await (await run.submit("A single turn.")).wait(BACKGROUND_CONTEXT);
    const frames = await received;
    expect(frames).toContain(CONTEXT_BUDGET_EXCEEDED_TEXT);
    expect(frames).not.toContain("private.invalid");
  });

  it("stops after one compaction if the retried request is still too large", async () => {
    const run = await session((requests) => {
      if (requests.length === 1) return answer("Earlier answer.");
      if (requests.length === 3) return answer("Summary of earlier work.");
      return refused();
    }, { keepRecentTokens: 1 });
    await (await run.submit("Earlier work.")).wait(BACKGROUND_CONTEXT);
    await (await run.submit("Continue.")).wait(BACKGROUND_CONTEXT);
    expect(run.requests).toHaveLength(4);
    expect(failed(await run.hosted.transcript(false)).at(-1)?.payload?.error)
      .toBe(CONTEXT_BUDGET_EXCEEDED_TEXT);
  });

  it("names a failed blocking threshold compaction that settles without an answer", async () => {
    const run = await session(
      (requests) => {
        if (requests.length === 1) return answer("Earlier answer.");
        if (requests.length === 2)
          return Response.json({ error: { message: "private summary failure" } }, { status: 500 });
        return answer("Unexpected generation.");
      },
      { keepRecentTokens: 1, reserveTokens: 1_000_000 },
    );
    await (await run.submit("Earlier work.")).wait(BACKGROUND_CONTEXT);
    await (await run.submit("Continue.")).wait(BACKGROUND_CONTEXT);
    // The threshold compacts before the second generation, so the failed
    // summary is the last request and no answer follows it.
    expect(run.requests).toHaveLength(2);
    const thread = await run.hosted.transcript(false);
    expect(failed(thread).at(-1)?.payload?.error).toBe("The model request failed.");
    expect(JSON.stringify(thread)).not.toContain("private summary failure");
  });

  it("reports a failed summary without retrying the generation", async () => {
    const run = await session((requests) => {
      if (requests.length === 1) return answer("Earlier answer.");
      if (requests.length === 2) return refused();
      return Response.json({ error: { message: "private summary failure" } }, { status: 500 });
    }, { keepRecentTokens: 1 });
    await (await run.submit("Earlier work.")).wait(BACKGROUND_CONTEXT);
    await (await run.submit("Continue.")).wait(BACKGROUND_CONTEXT);
    expect(run.requests).toHaveLength(3);
    const thread = await run.hosted.transcript(false);
    expect(failed(thread).at(-1)?.payload?.error).toBe(CONTEXT_BUDGET_EXCEEDED_TEXT);
    expect(JSON.stringify(thread)).not.toContain("private summary failure");
  });
});
